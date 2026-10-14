//! The window: sidebar (nav, project folders with their threads, account), the centered chat or
//! the New Chat page, and the setup page that picks and signs in to an AI on first run.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt, AnyElement, App, Application, Bounds, Context, Corner, Div, Entity,
    FocusHandle, Focusable, FontWeight, HighlightStyle, KeyBinding, Menu, MenuItem, MouseButton,
    PathPromptOptions, PromptLevel, ScrollHandle, SharedString, Stateful, StyledText, Subscription,
    Svg, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, anchored, deferred,
    div, point, prelude::*, pulsating_between, px, relative, rgb, size, svg,
};
use rozu_activity::ActivityState;
use rozu_core::{ApprovalRequest, Decision};
use rozu_providers::{ModelChoice, ProviderInfo, registry};
use tokio::sync::{mpsc, oneshot};

use crate::backend::{self, Access, Backend, Chat, PLANS, Update};
use crate::composer::{self, Composer, Sent};
use crate::feed::{Ask, Feed, Item, Step};
use crate::markdown::{self, Block};
use crate::settings::Settings;
use crate::theme::*;

actions!(rozu, [Quit]);

pub fn run() {
    let launch = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .filter(|p| p.parent().is_some())
        .and_then(|p| p.canonicalize().ok());
    let backend = Backend::new().unwrap_or_else(|err| {
        eprintln!("rozu-app: {err:#}");
        std::process::exit(1);
    });
    Application::new()
        .with_assets(Assets)
        .run(move |cx: &mut App| {
            composer::bind_keys(cx);
            cx.bind_keys([KeyBinding::new("cmd-q", Quit, None)]);
            cx.on_action(|_: &Quit, cx| cx.quit());
            cx.set_menus(vec![Menu {
                name: "rozu".into(),
                items: vec![MenuItem::action("Quit rozu", Quit)],
            }]);
            let screen = cx.primary_display().map(|d| d.bounds().size);
            let fit = |want: f32, have: Option<f32>| px(have.map_or(want, |h| want.min(h - 80.)));
            let bounds = Bounds::centered(
                None,
                size(
                    fit(1280., screen.map(|s| s.width.into())),
                    fit(840., screen.map(|s| s.height.into())),
                ),
                cx,
            );
            let options = WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                titlebar: Some(TitlebarOptions {
                    title: Some("rozu".into()),
                    appears_transparent: true,
                    traffic_light_position: Some(point(px(16.), px(16.))),
                }),
                ..Default::default()
            };
            cx.open_window(options, |window, cx| {
                cx.new(|cx| Rozu::new(backend, launch, window, cx))
            })
            .expect("open the window");
            cx.on_window_closed(|cx| cx.quit()).detach();
            cx.activate(true);
        });
}

struct Thread {
    id: usize,
    root: PathBuf,
    /// Whose model runs it, for the logo next to it.
    provider: &'static str,
    title: SharedString,
    feed: Feed,
    chat: Option<Chat>,
    pending: HashMap<String, oneshot::Sender<Decision>>,
    touched: Instant,
    _updates: Option<Task<()>>,
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Home,
    Thread(usize),
    Setup,
}

struct Rozu {
    backend: Backend,
    settings: Settings,
    /// Where New Chat starts.
    project: Option<PathBuf>,
    collapsed: HashSet<PathBuf>,
    threads: Vec<Thread>,
    next_id: usize,
    page: Page,
    providers: Vec<&'static ProviderInfo>,
    choice: Option<ModelChoice>,
    effort: Option<String>,
    menu: bool,
    /// The model menu's search box, and the window height it must fit in.
    search: Entity<Composer>,
    viewport_h: f32,
    /// The chat column's inner width. Text gets it as a fixed width: GPUI wraps text at the
    /// width it was offered, which inside the scrolling feed is wider than the column.
    text_w: f32,
    _search: Subscription,
    /// Codex's own model list, once it answered.
    live: backend::Live,
    /// The plan picked on the setup page, and what each plan's CLI says.
    plan: &'static str,
    access: HashMap<&'static str, Access>,
    composer: Entity<Composer>,
    scroll: ScrollHandle,
    _sent: Subscription,
}

impl Rozu {
    fn new(
        backend: Backend,
        launch: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let composer = cx.new(Composer::new);
        let sent = cx.subscribe(&composer, |this, _, Sent(text), cx| {
            this.send(text.clone(), cx)
        });
        let search = cx.new(|cx| {
            let mut c = Composer::new(cx);
            c.placeholder = "Search models".into();
            c
        });
        let search_sub = cx.subscribe(&search, |this, _, Sent(query), cx| {
            if let Some(m) = this.matches(query).into_iter().next() {
                this.choose(m, cx);
            }
        });
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        window.focus(&composer.focus_handle(cx));
        let mut settings = Settings::load();
        if let Some(root) = launch.clone()
            && settings.add_project(root)
        {
            settings.save();
        }
        let choice = settings
            .provider
            .as_deref()
            .and_then(|p| registry::resolve(Some(p), settings.model.as_deref()).ok())
            .map(backend::pin);
        let first_run = choice.is_none();
        let mut app = Self {
            backend,
            project: launch.or_else(|| settings.projects.last().cloned()),
            effort: settings.effort.clone(),
            settings,
            collapsed: HashSet::new(),
            threads: Vec::new(),
            next_id: 0,
            page: Page::Home,
            providers: backend::providers(),
            choice,
            menu: false,
            search,
            viewport_h: 800.,
            text_w: 700.,
            _search: search_sub,
            live: Vec::new(),
            plan: PLANS[0].id,
            access: HashMap::new(),
            composer,
            scroll: ScrollHandle::new(),
            _sent: sent,
        };
        let codex = app.backend.codex_models();
        cx.spawn(async move |this, cx| {
            if let Ok(live) = codex.await {
                let _ = this.update(cx, |app, cx| {
                    app.live = live;
                    cx.notify();
                });
            }
        })
        .detach();
        if first_run {
            app.setup(cx);
        } else {
            app.open(Page::Home, cx);
        }
        app
    }

    fn open(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.menu = false;
        let placeholder = match page {
            Page::Thread(_) => "Send follow-up",
            _ => "Plan, build, / for skills, @ for context",
        };
        self.composer.update(cx, |c, cx| {
            c.placeholder = placeholder.into();
            cx.notify();
        });
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn thread(&mut self, id: usize) -> Option<&mut Thread> {
        self.threads.iter_mut().find(|t| t.id == id)
    }

    fn running(&self) -> bool {
        matches!(self.page, Page::Thread(id) if self.threads.iter().any(|t| t.id == id && t.feed.running))
    }

    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let id = match self.page {
            Page::Thread(_) if self.running() => {
                return self.composer.update(cx, |c, cx| c.set_text(&text, cx));
            }
            Page::Thread(id) => id,
            _ => {
                let Some(root) = self.project.clone() else {
                    self.composer.update(cx, |c, cx| c.set_text(&text, cx));
                    return self.add_project(cx);
                };
                let title: String = text.lines().next().unwrap_or("").chars().take(60).collect();
                let id = self.next_id;
                self.next_id += 1;
                self.collapsed.remove(&root);
                self.threads.push(Thread {
                    id,
                    root,
                    provider: self.choice.as_ref().map_or("", |c| c.provider.id),
                    title: title.into(),
                    feed: Feed::default(),
                    chat: None,
                    pending: HashMap::new(),
                    touched: Instant::now(),
                    _updates: None,
                });
                id
            }
        };
        if self.thread(id).is_some_and(|t| t.chat.is_none()) {
            self.start(id, cx);
        }
        let Some(thread) = self.thread(id) else {
            return;
        };
        thread.feed.send(text.clone());
        thread.touched = Instant::now();
        match &thread.chat {
            Some(chat) if chat.send(text) => {}
            _ => thread.feed.fail(
                "No AI is set up yet: click the gear at the bottom left to pick one.".into(),
                false,
            ),
        }
        self.open(Page::Thread(id), cx);
    }

    fn start(&mut self, id: usize, cx: &mut Context<Self>) {
        let Some(choice) = self.choice.clone() else {
            return;
        };
        let effort = self.effort.clone();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let Some(root) = self.thread(id).map(|t| t.root.clone()) else {
            return;
        };
        let chat = self.backend.start(&root, &choice, effort, tx);
        let updates = cx.spawn(async move |this, cx| {
            while let Some(update) = rx.recv().await {
                if this
                    .update(cx, |app, cx| app.update(id, update, cx))
                    .is_err()
                {
                    break;
                }
            }
        });
        if let Some(thread) = self.thread(id) {
            thread.chat = Some(chat);
            thread._updates = Some(updates);
        }
    }

    fn update(&mut self, id: usize, update: Update, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(id) else {
            return;
        };
        match update {
            Update::Event(event) => thread.feed.apply(event),
            Update::Ask(request, answer) => {
                thread.pending.insert(request.call_id.clone(), answer);
                thread.feed.ask(request);
            }
            Update::Failed { message, offline } => thread.feed.fail(message, offline),
        }
        thread.touched = Instant::now();
        if self.page == Page::Thread(id) {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    fn answer(&mut self, id: usize, item: usize, decision: Decision, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(id) else {
            return;
        };
        let Some(Item::Ask(ask)) = thread.feed.items.get_mut(item) else {
            return;
        };
        ask.answer = Some(match decision {
            Decision::Approve => "Allowed",
            Decision::AlwaysAllowPrefix => "Always allowed",
            Decision::ApproveUnsandboxed => "Allowed without the sandbox",
            Decision::Deny { .. } => "Denied",
        });
        if let Some(tx) = thread.pending.remove(&ask.request.call_id) {
            let _ = tx.send(decision);
        }
        cx.notify();
    }

    fn stop(&mut self, cx: &mut Context<Self>) {
        if let Page::Thread(id) = self.page
            && let Some(chat) = self.thread(id).and_then(|t| t.chat.as_ref())
        {
            chat.cancel();
        }
        cx.notify();
    }

    /// Closes a chat: stops its turn, denies its open prompts and forgets it.
    fn delete(&mut self, id: usize, cx: &mut Context<Self>) {
        if let Some(chat) = self.thread(id).and_then(|t| t.chat.as_ref()) {
            chat.cancel();
        }
        self.threads.retain(|t| t.id != id);
        if self.page == Page::Thread(id) {
            self.open(Page::Home, cx);
        }
        cx.notify();
    }

    fn add_project(&mut self, cx: &mut Context<Self>) {
        let picked = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open".into()),
        });
        cx.spawn(async move |this, cx| {
            let Ok(Ok(Some(paths))) = picked.await else {
                return;
            };
            let Some(root) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |app, cx| {
                app.settings.add_project(root.clone());
                app.settings.save();
                app.collapsed.remove(&root);
                app.project = Some(root);
                if app.page != Page::Setup {
                    app.open(Page::Home, cx);
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn remove_project(&mut self, root: PathBuf, window: &mut Window, cx: &mut Context<Self>) {
        let answer = window.prompt(
            PromptLevel::Warning,
            &format!("Remove {} from rozu?", name(&root)),
            Some("Its chats close. The folder and its files stay on your Mac."),
            &["Remove", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let _ = this.update(cx, |app, cx| {
                let ids: Vec<usize> = app
                    .threads
                    .iter()
                    .filter(|t| t.root == root)
                    .map(|t| t.id)
                    .collect();
                for id in ids {
                    app.delete(id, cx);
                }
                app.settings.remove_project(&root);
                app.settings.save();
                if app.project.as_ref() == Some(&root) {
                    app.project = app.settings.projects.last().cloned();
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose(&mut self, choice: ModelChoice, cx: &mut Context<Self>) {
        let efforts = backend::efforts(choice.provider.id);
        if self
            .effort
            .as_deref()
            .is_some_and(|e| !efforts.contains(&e))
        {
            self.effort = None;
        }
        self.choice = Some(choice);
        self.save_choice();
        cx.notify();
    }

    fn set_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        self.effort = effort;
        self.save_choice();
        cx.notify();
    }

    fn save_choice(&mut self) {
        if self.page == Page::Setup && self.settings.provider.is_none() {
            return;
        }
        self.settings.provider = self.choice.as_ref().map(|c| c.provider.id.to_owned());
        self.settings.model = self.choice.as_ref().map(|c| c.model.clone());
        self.settings.effort = self.effort.clone();
        self.settings.save();
    }

    /// Opens the setup page on the current plan and checks every plan's CLI.
    fn setup(&mut self, cx: &mut Context<Self>) {
        if let Some(choice) = &self.choice
            && let Some(plan) = PLANS.iter().find(|p| p.id == choice.provider.id)
        {
            self.plan = plan.id;
        } else {
            self.pick_plan(PLANS[0].id, cx);
        }
        self.check(cx);
        self.open(Page::Setup, cx);
    }

    fn pick_plan(&mut self, id: &'static str, cx: &mut Context<Self>) {
        self.plan = id;
        if self.choice.as_ref().is_none_or(|c| c.provider.id != id)
            && let Ok(choice) = registry::resolve(Some(id), None)
        {
            self.choose(choice, cx);
        }
        cx.notify();
    }

    fn check(&mut self, cx: &mut Context<Self>) {
        for plan in PLANS {
            self.access.insert(plan.id, Access::Checking);
            let rx = self.backend.check(plan.id);
            cx.spawn(async move |this, cx| {
                let access = rx.await.unwrap_or(Access::Missing);
                let _ = this.update(cx, |app, cx| {
                    app.access.insert(plan.id, access);
                    cx.notify();
                });
            })
            .detach();
        }
        cx.notify();
    }

    fn finish_setup(&mut self, cx: &mut Context<Self>) {
        self.settings.provider = Some(self.plan.to_owned());
        self.save_choice();
        self.providers = backend::providers();
        self.open(Page::Home, cx);
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = [
            ("chat", "New Chat"),
            ("search", "Search"),
            ("bot", "Automations"),
            ("blocks", "Customize"),
        ];
        let (repos, folders): (Vec<PathBuf>, Vec<PathBuf>) = self
            .settings
            .projects
            .iter()
            .cloned()
            .partition(|p| p.join(".git").exists());
        let folders: Vec<AnyElement> = folders
            .into_iter()
            .map(|p| self.project_group(p, cx))
            .collect();
        let repos: Vec<AnyElement> = repos
            .into_iter()
            .map(|p| self.project_group(p, cx))
            .collect();
        let setup = self.page == Page::Setup;
        div()
            .w(px(270.))
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(SIDE))
            .border_r_1()
            .border_color(rgb(LINE))
            .px_2()
            .pb_2()
            .child(drag_bar())
            .children(nav.into_iter().map(|(i, label)| {
                let on = label == "New Chat" && self.page == Page::Home;
                row(label, i, label)
                    .when(on, |r| r.bg(rgb(RAISE)))
                    .when(label == "New Chat", |r| {
                        r.on_click(cx.listener(|this, _, _, cx| this.open(Page::Home, cx)))
                    })
            }))
            .child(
                div()
                    .id("projects")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(heading("Projects"))
                    .child(
                        row("new-project", "plus", "New Project")
                            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                    )
                    .children(folders)
                    .when(!repos.is_empty(), |d| {
                        d.child(heading("Repositories")).children(repos)
                    }),
            )
            .child(
                account()
                    .id("account")
                    .cursor_pointer()
                    .when(setup, |r| r.bg(rgb(RAISE)))
                    .on_click(cx.listener(|this, _, _, cx| this.setup(cx))),
            )
    }

    fn project_group(&mut self, root: PathBuf, cx: &mut Context<Self>) -> AnyElement {
        let open = !self.collapsed.contains(&root);
        let mut ids: Vec<usize> = self
            .threads
            .iter()
            .filter(|t| t.root == root)
            .map(|t| t.id)
            .collect();
        ids.sort_by_key(|&id| {
            std::cmp::Reverse(self.threads.iter().find(|t| t.id == id).map(|t| t.touched))
        });
        let rows: Vec<AnyElement> = if open {
            ids.into_iter()
                .map(|id| self.thread_row(id, cx).into_any_element())
                .collect()
        } else {
            Vec::new()
        };
        let key = root.display().to_string();
        let (new_root, del_root, toggle_root) = (root.clone(), root.clone(), root.clone());
        let here = self.page == Page::Home && self.project.as_ref() == Some(&root);
        div()
            .child(
                row(
                    SharedString::from(format!("p:{key}")),
                    "folder",
                    name(&root),
                )
                .group("project")
                .when(here, |r| r.bg(rgb(RAISE)))
                .child(div().flex_1())
                .child(
                    hover_icon(SharedString::from(format!("pnew:{key}")), "project", "plus")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.project = Some(new_root.clone());
                            this.open(Page::Home, cx);
                        })),
                )
                .child(
                    hover_icon(
                        SharedString::from(format!("pdel:{key}")),
                        "project",
                        "trash",
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.remove_project(del_root.clone(), window, cx);
                    })),
                )
                .child(icon(if open { "down" } else { "right" }, INK3))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.collapsed.remove(&toggle_root) {
                        this.collapsed.insert(toggle_root.clone());
                    }
                    cx.notify();
                })),
            )
            .children(rows)
            .into_any_element()
    }

    fn thread_row(&mut self, id: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let Some(thread) = self.threads.iter().find(|t| t.id == id) else {
            return div().id(("thread", id));
        };
        let dot = match thread.feed.state() {
            ActivityState::Error => Some(BAD),
            ActivityState::Waiting => Some(WARN),
            _ if thread.feed.running => Some(WARN),
            _ => None,
        };
        div()
            .id(("thread", id))
            .group("thread")
            .flex()
            .items_center()
            .gap_3()
            .pl(px(14.))
            .pr_2()
            .py(px(6.))
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(RAISE)))
            .when(self.page == Page::Thread(id), |r| r.bg(rgb(RAISE)))
            .child(logo(thread.provider))
            .child(div().flex_1().truncate().child(thread.title.clone()))
            .children(dot.map(|c| div().size(px(7.)).flex_none().rounded_full().bg(rgb(c))))
            .child(
                div()
                    .text_size(px(13.5))
                    .text_color(rgb(INK3))
                    .group_hover("thread", |s| s.invisible())
                    .child(age(thread.touched)),
            )
            .child(
                hover_icon(("del", id), "thread", "trash")
                    .absolute()
                    .right(px(8.))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.delete(id, cx);
                    })),
            )
            .relative()
            .on_click(cx.listener(move |this, _, _, cx| this.open(Page::Thread(id), cx)))
    }

    fn thread_page(&mut self, id: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(thread) = self.threads.iter().find(|t| t.id == id) else {
            return div().flex_1();
        };
        let steps: Vec<bool> = thread
            .feed
            .items
            .iter()
            .map(|item| matches!(item, Item::Step(_)))
            .collect();
        let items: Vec<AnyElement> = (0..steps.len())
            .map(|i| {
                let gap = match i {
                    0 => 0.,
                    _ if steps[i] && steps[i - 1] => 4.,
                    _ => 16.,
                };
                div()
                    .w_full()
                    .min_w_0()
                    .mt(px(gap))
                    .child(self.item(id, i, cx))
                    .into_any_element()
            })
            .collect();
        let title = self
            .threads
            .iter()
            .find(|t| t.id == id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                drag_bar().px(px(20.)).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(INK3))
                        .child(title),
                ),
            )
            .child(
                div()
                    .id("feed")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(column().pt(px(8.)).pb(px(24.)).children(items)),
            )
            .child(column().pb(px(16.)).child(self.composer_box(false, cx)))
    }

    fn item(&self, id: usize, i: usize, cx: &mut Context<Self>) -> AnyElement {
        let Some(thread) = self.threads.iter().find(|t| t.id == id) else {
            return div().into_any_element();
        };
        match &thread.feed.items[i] {
            Item::User(text) => div()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(BOX))
                .rounded(px(14.))
                .px(px(18.))
                .py(px(12.))
                .w(px(self.text_w))
                .child(text.clone())
                .into_any_element(),
            Item::Text(text) => rich(text, self.text_w).into_any_element(),
            Item::Note(text) => div()
                .text_color(rgb(INK3))
                .text_size(px(14.))
                .child(text.clone())
                .into_any_element(),
            Item::Step(step) => step_row(i, step).into_any_element(),
            Item::Ask(ask) => self.ask_card(id, i, ask, cx).into_any_element(),
        }
    }

    fn ask_card(&self, id: usize, i: usize, ask: &Ask, cx: &mut Context<Self>) -> impl IntoElement {
        let req = &ask.request;
        let title = match req.tool.as_str() {
            "shell" => "Run a command?".to_owned(),
            t if t.contains("patch") || t.contains("write") => "Edit a file?".to_owned(),
            t => format!("Use {t}?"),
        };
        let preview: String = req.preview.lines().take(24).collect::<Vec<_>>().join("\n");
        let mut choices = vec![("Allow once", Decision::Approve, true)];
        if req.allow_rule.is_some() {
            choices.push(("Always allow", Decision::AlwaysAllowPrefix, false));
        }
        if req.can_run_unsandboxed() {
            choices.push((
                "Run without the sandbox",
                Decision::ApproveUnsandboxed,
                false,
            ));
        }
        choices.push((
            "Deny",
            Decision::Deny {
                reason: "denied by the user".into(),
            },
            false,
        ));
        div()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(BOX))
            .rounded(px(12.))
            .px(px(16.))
            .py(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(icon("ask", if ask.answer.is_none() { WARN } else { INK3 }))
                    .child(title),
            )
            .child(
                div()
                    .font_family("Menlo")
                    .text_size(px(13.))
                    .bg(rgb(BG))
                    .rounded(px(8.))
                    .px(px(10.))
                    .py(px(8.))
                    .child(preview),
            )
            .child(
                div()
                    .text_color(rgb(INK3))
                    .text_size(px(14.))
                    .child(confinement(req)),
            )
            .child(match ask.answer {
                Some(answer) => div().text_color(rgb(INK3)).text_size(px(14.)).child(answer),
                None => div()
                    .flex()
                    .gap_2()
                    .children(choices.into_iter().enumerate().map(
                        |(n, (label, decision, primary))| {
                            button(("ask", n), label, primary).on_click(cx.listener(
                                move |this, _, _, cx| this.answer(id, i, decision.clone(), cx),
                            ))
                        },
                    )),
            })
    }

    fn composer_box(&self, home: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.running();
        let model = self.choice.as_ref().map_or("No model".into(), |c| {
            backend::label(c, self.effort.as_deref(), &self.live)
        });
        let picker = div()
            .relative()
            .child(
                div()
                    .id("model")
                    .flex()
                    .items_center()
                    .gap_1()
                    .text_size(px(14.))
                    .text_color(rgb(INK2))
                    .cursor_pointer()
                    .child(model)
                    .child(icon("down", INK2))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.providers = backend::providers();
                        this.menu = true;
                        this.search.update(cx, |s, cx| s.set_text("", cx));
                        window.focus(&this.search.focus_handle(cx));
                        cx.notify();
                    })),
            )
            .when(self.menu, |d| {
                d.child(
                    div().absolute().top(px(-8.)).right_0().child(
                        deferred(
                            anchored()
                                .anchor(Corner::BottomRight)
                                .snap_to_window_with_margin(px(8.))
                                .child(self.model_menu(cx)),
                        )
                        .with_priority(2),
                    ),
                )
            });
        let go = div()
            .id("go")
            .size(px(34.))
            .flex_none()
            .rounded_full()
            .bg(rgb(INK))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(icon(if running { "stop" } else { "up" }, BG))
            .on_click(cx.listener(move |this, _, window, cx| {
                if running {
                    this.stop(cx);
                } else {
                    let composer = this.composer.clone();
                    window.dispatch_action(Box::new(composer::Submit), cx);
                    window.focus(&composer.focus_handle(cx));
                }
            }));
        let shell = div().border_1().border_color(rgb(LINE)).bg(rgb(BOX));
        if home {
            shell
                .rounded(px(18.))
                .p(px(14.))
                .flex()
                .flex_col()
                .gap_3()
                .child(div().min_h(px(52.)).flex().child(self.composer.clone()))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_3()
                        .child(div().flex_1())
                        .child(picker)
                        .child(go),
                )
        } else {
            shell
                .rounded(px(22.))
                .py(px(8.))
                .pl(px(18.))
                .pr(px(8.))
                .flex()
                .items_center()
                .gap_3()
                .child(self.composer.clone())
                .child(picker)
                .child(go)
        }
    }

    /// Every model the menu can offer that matches `query`, grouped by provider.
    fn matches(&self, query: &str) -> Vec<ModelChoice> {
        let query = query.trim().to_lowercase();
        self.providers
            .iter()
            .flat_map(|p| backend::models(p, &self.live))
            .filter(|m| {
                let name = format!(
                    "{} {} {}",
                    backend::provider_name(m.provider),
                    backend::model_name(&m.model, &self.live),
                    m.model
                );
                query.is_empty() || name.to_lowercase().contains(&query)
            })
            .collect()
    }

    /// The model menu, like Cursor's: a search box, a scrolling list grouped by provider, and
    /// the effort for the chosen model. Kept inside the window.
    fn model_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).text.clone();
        let models = self.matches(&query);
        let mut list = div()
            .id("model-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_1()
            .flex()
            .flex_col();
        let mut last = "";
        for m in models.iter().cloned() {
            if m.provider.id != last {
                last = m.provider.id;
                list = list.child(menu_heading(backend::provider_name(m.provider)));
            }
            let on = self.choice.as_ref() == Some(&m);
            let key = SharedString::from(format!("m:{}", m.spec()));
            let label = backend::model_name(&m.model, &self.live);
            list = list.child(
                div()
                    .id(key)
                    .flex()
                    .items_center()
                    .px_2()
                    .py(px(4.))
                    .rounded(px(6.))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(LINE)))
                    .when(on, |r| r.bg(rgb(LINE)))
                    .child(div().flex_1().truncate().child(label))
                    .when(on, |r| r.child(icon("check", INK)))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(m.clone(), cx))),
            );
        }
        if models.is_empty() {
            list = list.child(div().p_2().text_size(px(13.)).text_color(rgb(INK3)).child(
                if self.providers.is_empty() {
                    "No AI is set up. Click the gear to pick one."
                } else {
                    "No models match."
                },
            ));
        }
        let efforts = self
            .choice
            .as_ref()
            .map_or(&[][..], |c| backend::efforts(c.provider.id));
        div()
            .id("model-menu")
            .occlude()
            .w(px(300.))
            .max_h(px((self.viewport_h - 120.).clamp(160., 420.)))
            .bg(rgb(RAISE))
            .border_1()
            .border_color(rgb(LINE))
            .rounded(px(12.))
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .text_size(px(13.))
                    .child(icon("search", INK3))
                    .child(self.search.clone()),
            )
            .child(list)
            .when(!efforts.is_empty(), |d| {
                d.child(
                    div()
                        .border_t_1()
                        .border_color(rgb(LINE))
                        .px_2()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(INK3))
                                .child("Effort"),
                        )
                        .child(self.effort_chips(efforts, cx)),
                )
            })
    }

    fn effort_chips(&self, efforts: &'static [&'static str], cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(
                chip("effort-default", "Default", self.effort.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.set_effort(None, cx))),
            )
            .children(efforts.iter().map(|&e| {
                chip(
                    SharedString::from(format!("effort-{e}")),
                    e,
                    self.effort.as_deref() == Some(e),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_effort(Some(e.into()), cx)))
            }))
    }

    fn home_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ideas = [
            (
                "plan",
                "Start with a plan",
                "Align on implementation before writing code",
            ),
            (
                "bug",
                "Debug an issue",
                "Find root causes and fix tricky bugs",
            ),
            ("test", "Write tests", "Cover the code you just changed"),
            (
                "diff",
                "Review my changes",
                "Check the diff before you push",
            ),
        ];
        let folder = self
            .project
            .as_deref()
            .map_or("Choose a folder".into(), name);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(drag_bar())
            .child(
                column()
                    .flex_1()
                    .justify_center()
                    .gap(px(14.))
                    .pb(px(44.))
                    .child(
                        div()
                            .id("folder")
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(14.))
                            .text_color(rgb(INK2))
                            .cursor_pointer()
                            .child(icon("folder", INK2))
                            .child(folder)
                            .child(icon("down", INK3))
                            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                    )
                    .child(self.composer_box(true, cx))
                    .child(div().mt_2().flex().flex_col().children(
                        ideas.into_iter().enumerate().map(|(n, (i, title, text))| {
                            row(("idea", n), i, title)
                                .rounded_none()
                                .py(px(12.))
                                .when(n < 3, |r| r.border_b_1().border_color(rgb(LINE)))
                                .child(div().text_color(rgb(INK3)).child(text))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.composer
                                        .update(cx, |c, cx| c.set_text(&format!("{title}: "), cx));
                                    window.focus(&this.composer.focus_handle(cx));
                                }))
                        }),
                    )),
            )
    }

    /// Pick the AI you pay for, sign in through its CLI, pick a model, effort and folder.
    fn setup_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let first_run = self.settings.provider.is_none();
        let plans: Vec<AnyElement> = PLANS
            .iter()
            .map(|plan| {
                let access = self
                    .access
                    .get(plan.id)
                    .copied()
                    .unwrap_or(Access::Checking);
                let on = self.plan == plan.id;
                div()
                    .id(plan.id)
                    .flex()
                    .items_center()
                    .gap_3()
                    .px(px(16.))
                    .py(px(12.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(rgb(if on { INK2 } else { LINE }))
                    .bg(rgb(BOX))
                    .cursor_pointer()
                    .child(logo(plan.id).size(px(22.)))
                    .child(
                        div().flex_1().flex().flex_col().child(plan.name).child(
                            div()
                                .text_size(px(14.))
                                .text_color(rgb(INK3))
                                .child(plan.plan),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(rgb(INK2))
                            .child(access_text(access)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.pick_plan(plan.id, cx)))
                    .into_any_element()
            })
            .collect();
        let plan = PLANS
            .iter()
            .find(|p| p.id == self.plan)
            .unwrap_or(&PLANS[0]);
        let access = self
            .access
            .get(plan.id)
            .copied()
            .unwrap_or(Access::Checking);
        let action = match access {
            Access::Missing => Some(("Install and sign in", true)),
            Access::SignedOut => Some(("Sign in", true)),
            Access::Installed => Some(("Sign in", false)),
            Access::Checking | Access::Ready => None,
        };
        let ready = matches!(access, Access::Ready | Access::Installed);
        let models: Vec<ModelChoice> = registry::provider_info(plan.id)
            .map(|p| backend::models(p, &self.live))
            .unwrap_or_default();
        let efforts = backend::efforts(plan.id);
        let folder = self.project.as_deref().map_or("None yet".into(), name);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(drag_bar())
            .child(
                div()
                    .id("setup")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        column()
                            .max_w(px(620.))
                            .gap(px(14.))
                            .py(px(24.))
                            .child(div().text_size(px(28.)).child(if first_run {
                                "Welcome to rozu"
                            } else {
                                "Your AI"
                            }))
                            .child(div().text_color(rgb(INK2)).child(
                                "Pick the AI you already pay for. rozu works through its official \
                             app, so your plan covers it and rozu never sees your password.",
                            ))
                            .children(plans)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .children(action.map(|(label, primary)| {
                                        let cmd = match access {
                                            Access::Missing => {
                                                format!("{} && {}", plan.install, plan.login)
                                            }
                                            _ => plan.login.to_owned(),
                                        };
                                        button("sign-in", label, primary)
                                            .on_click(move |_, _, _| backend::in_terminal(&cmd))
                                    }))
                                    .child(
                                        button("check", "Check again", false)
                                            .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                                    ),
                            )
                            .when(action.is_some(), |d| {
                                d.child(div().text_size(px(14.)).text_color(rgb(INK3)).child(
                                    "This opens Terminal. Finish there, then press Check again.",
                                ))
                            })
                            .child(label_text("Model"))
                            .child(div().flex().flex_wrap().gap_1().children(
                                models.into_iter().map(|m| {
                                    let on = self.choice.as_ref() == Some(&m);
                                    chip(
                                        SharedString::from(format!("sm:{}", m.model)),
                                        backend::model_name(&m.model, &self.live),
                                        on,
                                    )
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.choose(m.clone(), cx)
                                        }),
                                    )
                                }),
                            ))
                            .when(!efforts.is_empty(), |d| {
                                d.child(label_text("Effort"))
                                    .child(self.effort_chips(efforts, cx))
                            })
                            .child(label_text("Folder"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(icon("folder", INK2))
                                    .child(folder)
                                    .child(button("folder", "Choose folder", false).on_click(
                                        cx.listener(|this, _, _, cx| this.add_project(cx)),
                                    )),
                            )
                            .child(
                                div().pt_3().child(
                                    button("continue", "Continue", ready)
                                        .when(!ready, |b| b.opacity(0.5).cursor_default())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if ready {
                                                this.finish_setup(cx);
                                            }
                                        })),
                                ),
                            ),
                    ),
            )
    }
}

impl Render for Rozu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.viewport_h = window.viewport_size().height.into();
        let side = if self.settings.provider.is_some() {
            270.
        } else {
            0.
        };
        let main_w = f32::from(window.viewport_size().width) - side;
        self.text_w = main_w.min(760.) - 56.;
        let main = match self.page {
            Page::Thread(id) => self.thread_page(id, cx).into_any_element(),
            Page::Home => self.home_page(cx).into_any_element(),
            Page::Setup => self.setup_page(cx).into_any_element(),
        };
        let first_run = self.settings.provider.is_none();
        div()
            .size_full()
            .relative()
            .flex()
            .bg(rgb(BG))
            .text_color(rgb(INK))
            .font_family(".SystemUIFont")
            .text_size(px(15.))
            .when(!first_run, |d| d.child(self.sidebar(cx)))
            .child(main)
            .when(self.menu, |d| {
                d.child(
                    deferred(
                        div()
                            .id("backdrop")
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .occlude()
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.menu = false;
                                window.focus(&this.composer.focus_handle(cx));
                                cx.notify();
                            })),
                    )
                    .with_priority(1),
                )
            })
    }
}

impl Focusable for Rozu {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

fn name(root: &Path) -> SharedString {
    root.file_name()
        .map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
        .into()
}

fn access_text(access: Access) -> &'static str {
    match access {
        Access::Checking => "Checking",
        Access::Missing => "Not installed",
        Access::SignedOut => "Not signed in",
        Access::Ready => "Ready",
        Access::Installed => "Installed",
    }
}

fn label_text(text: &'static str) -> Div {
    div()
        .pt_2()
        .text_size(px(14.))
        .text_color(rgb(INK3))
        .child(text)
}

fn menu_heading(text: &'static str) -> Div {
    div()
        .px_2()
        .pt_2()
        .pb_1()
        .text_size(px(13.))
        .text_color(rgb(INK3))
        .child(text)
}

fn chip(id: impl Into<gpui::ElementId>, label: impl Into<SharedString>, on: bool) -> Stateful<Div> {
    div()
        .id(id)
        .border_1()
        .border_color(rgb(if on { INK } else { LINE }))
        .when(on, |b| b.bg(rgb(INK)).text_color(rgb(BG)))
        .rounded_full()
        .px(px(12.))
        .py(px(3.))
        .text_size(px(14.))
        .cursor_pointer()
        .child(label.into())
}

/// An icon button shown while the pointer is over its `group`.
fn hover_icon(
    id: impl Into<gpui::ElementId>,
    group: &'static str,
    i: &'static str,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .rounded(px(6.))
        .p(px(2.))
        .cursor_pointer()
        .invisible()
        .group_hover(group, |s| s.visible())
        .hover(|s| s.bg(rgb(LINE)))
        .child(icon(i, INK3))
}

fn step_row(i: usize, step: &Step) -> Div {
    let (name, color) = match (&step.done, step.failed) {
        (Some(_), true) => ("fail", BAD),
        (Some(_), false) => ("check", INK3),
        (None, _) => match step.state {
            ActivityState::Reading => ("read", INK2),
            ActivityState::Searching => ("search", INK2),
            ActivityState::Editing => ("edit", INK2),
            ActivityState::Running => ("run", INK2),
            ActivityState::Waiting => ("ask", WARN),
            _ => ("think", INK2),
        },
    };
    let mark = icon(name, color).size(px(15.));
    let line = div()
        .flex()
        .items_center()
        .gap_2()
        .text_color(rgb(INK2))
        .child(match step.done {
            Some(_) => mark.into_any_element(),
            None => mark
                .with_animation(
                    ("step", i),
                    Animation::new(Duration::from_millis(1600))
                        .repeat()
                        .with_easing(pulsating_between(0.3, 1.)),
                    |s, a| s.opacity(a),
                )
                .into_any_element(),
        });
    match &step.done {
        None => line.child(step.label.clone()),
        Some(done) if step.failed => line.text_color(rgb(BAD)).child(done.clone()),
        Some(done) => {
            let (verb, rest) = done.split_once(' ').unwrap_or((done, ""));
            line.child(
                div()
                    .flex()
                    .gap_1()
                    .child(verb.to_owned())
                    .child(div().text_color(rgb(INK3)).child(rest.to_owned())),
            )
        }
    }
}

/// The agent's reply, Markdown drawn as paragraphs, headings, lists and code blocks.
fn rich(text: &str, w: f32) -> Div {
    div()
        .w(px(w))
        .flex()
        .flex_col()
        .gap(px(10.))
        .line_height(relative(1.6))
        .children(markdown::parse(text).into_iter().map(|block| {
            match block {
                Block::Para(t) => div().w(px(w)).child(spans(t)),
                Block::Heading(t) => div()
                    .w(px(w))
                    .pt(px(6.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(spans(t)),
                Block::Item(marker, t) => div()
                    .w(px(w))
                    .flex()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_none()
                            .w(px(14.))
                            .text_color(rgb(INK3))
                            .child(marker),
                    )
                    .child(div().w(px(w - 24.)).child(spans(t))),
                Block::Code(code) => div()
                    .w(px(w))
                    .overflow_hidden()
                    .bg(rgb(CODE_BG))
                    .border_1()
                    .border_color(rgb(LINE))
                    .rounded(px(10.))
                    .px(px(14.))
                    .py(px(10.))
                    .font_family("Menlo")
                    .text_size(px(13.))
                    .line_height(relative(1.5))
                    .text_color(rgb(INK2))
                    .child(code),
            }
        }))
}

fn spans(t: markdown::Inline) -> StyledText {
    let styles: Vec<_> = t
        .spans
        .into_iter()
        .map(|(r, s)| {
            let mut h = HighlightStyle::default();
            if s.bold {
                h.font_weight = Some(FontWeight::SEMIBOLD);
            }
            if s.code {
                h.color = Some(rgb(INK).into());
                h.background_color = Some(rgb(RAISE).into());
            }
            (r, h)
        })
        .collect();
    StyledText::new(t.text).with_highlights(styles)
}

fn logo(provider: &str) -> Svg {
    match provider {
        "codex" | "openai" => icon("logo-openai", INK),
        "grok" | "xai" => icon("logo-grok", INK),
        _ => icon("logo-claude", CLAUDE),
    }
}

/// How an approved call would run.
fn confinement(req: &ApprovalRequest) -> String {
    if req.blocked_retry {
        return "The sandbox blocked this. Run it again without the sandbox?".into();
    }
    let network = if req.network {
        "Network on."
    } else {
        "No network."
    };
    format!("Sandbox: {}. {network}", req.sandbox)
}

/// A centered column as wide as the mockup's chat.
fn column() -> Div {
    div()
        .w_full()
        .max_w(px(760.))
        .mx_auto()
        .px(px(28.))
        .flex()
        .flex_col()
}

/// The top 44px: drags the window (its title bar is hidden).
fn drag_bar() -> Div {
    div()
        .h(px(44.))
        .flex_none()
        .flex()
        .items_center()
        .text_size(px(14.))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
}

fn heading(text: &'static str) -> Div {
    div()
        .text_color(rgb(INK3))
        .text_size(px(14.))
        .px_2()
        .pt(px(18.))
        .pb(px(6.))
        .child(text)
}

fn row(
    id: impl Into<gpui::ElementId>,
    i: &'static str,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .px_2()
        .py(px(6.))
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(RAISE)))
        .child(icon(i, INK2))
        .child(label.into())
}

fn button(id: impl Into<gpui::ElementId>, label: &'static str, primary: bool) -> Stateful<Div> {
    div()
        .id(id)
        .border_1()
        .border_color(rgb(if primary { INK } else { LINE }))
        .when(primary, |b| b.bg(rgb(INK)).text_color(rgb(BG)))
        .rounded_full()
        .px(px(14.))
        .py(px(4.))
        .text_size(px(14.))
        .cursor_pointer()
        .child(label)
}

fn icon(name: &'static str, color: u32) -> Svg {
    svg()
        .path(name)
        .size(px(17.))
        .flex_none()
        .text_color(rgb(color))
}

fn account() -> Div {
    let name = std::env::var("USER").unwrap_or_default();
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .pt(px(10.))
        .px_1()
        .child(
            div()
                .size(px(30.))
                .rounded_full()
                .bg(rgb(RAISE))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(INK2))
                .child(initial),
        )
        .child(div().flex_1().child(name))
        .child(icon("gear", INK3))
}

fn age(at: Instant) -> String {
    match at.elapsed().as_secs() {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}
