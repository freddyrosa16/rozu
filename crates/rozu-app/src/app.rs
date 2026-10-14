//! The window and its state: which page is open, the chats (threads) per project, the chosen
//! model, effort and approval mode, and every action the pages trigger. How each page is drawn
//! lives in a child module: [`sidebar`], [`chat`], [`home`], [`setup`] and [`menu`] (the composer
//! and model picker), on top of the shared [`widgets`].

mod chat;
mod home;
mod menu;
mod setup;
mod sidebar;
mod widgets;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt, AnyElement, App, Application, Bounds, Context, Corner, Div, Entity,
    FocusHandle, Focusable, FontWeight, HighlightStyle, KeyBinding, Menu, MenuItem, MouseButton,
    PathPromptOptions, PromptLevel, ScrollHandle, SharedString, Stateful, StyledText, Subscription,
    Svg, Task, TitlebarOptions, Window, WindowBounds, WindowOptions, actions, anchored, deferred,
    div, point, prelude::*, pulsating_between, px, relative, rgb, size, svg,
};
use rozu_activity::ActivityState;
use rozu_core::{ApprovalMode, ApprovalRequest, Decision};
use rozu_providers::{ModelChoice, ProviderInfo, registry};
use rozu_store::unix_now;
use tokio::sync::{mpsc, oneshot};

use crate::backend::{Backend, Chat, Update};
use crate::catalog::{self, APPROVAL_MODES, Access, PLANS};
use crate::composer::{self, Composer, Sent};
use crate::feed::{Ask, Feed, Item, Step};
use crate::markdown::{self, Block};
use crate::settings::Settings;
use crate::theme::*;
use widgets::*;

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
    choice: Option<ModelChoice>,
    effort: Option<String>,
    title: SharedString,
    feed: Feed,
    /// The saved chat behind it, once it has one (a reopened chat has one from the start).
    session: Option<String>,
    /// Whether `feed` holds the saved history; a chat listed from disk loads it when opened.
    loaded: bool,
    chat: Option<Chat>,
    pending: HashMap<String, oneshot::Sender<Decision>>,
    /// Unix seconds of the last activity.
    touched: u64,
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
    /// What runs without asking in new chats (see `rozu chat --approval`).
    approval: ApprovalMode,
    menu: bool,
    /// The model menu's search box, and the window height it must fit in.
    search: Entity<Composer>,
    viewport_h: f32,
    /// The chat column's inner width. Text gets it as a fixed width: GPUI wraps text at the
    /// width it was offered, which inside the scrolling feed is wider than the column.
    text_w: f32,
    _search: Subscription,
    /// Codex's own model list, once it answered.
    live: catalog::Live,
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
            .map(catalog::pin);
        let first_run = choice.is_none();
        let approval = settings
            .approval
            .as_deref()
            .and_then(catalog::approval_mode)
            .unwrap_or_default();
        let threads = saved_threads(&backend, &settings.projects);
        let next_id = threads.len();
        let mut app = Self {
            backend,
            project: launch.or_else(|| settings.projects.last().cloned()),
            effort: settings.effort.clone(),
            approval,
            settings,
            collapsed: HashSet::new(),
            threads,
            next_id,
            page: Page::Home,
            providers: catalog::providers(),
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
        if let Page::Thread(id) = page {
            self.load_history(id);
            if let Some(thread) = self.thread_ref(id) {
                let choice = thread.choice.clone();
                let effort = thread.effort.clone();
                self.choice = choice;
                self.effort = effort;
            }
        } else if page == Page::Home {
            self.choice = self
                .settings
                .provider
                .as_deref()
                .and_then(|provider| {
                    registry::resolve(Some(provider), self.settings.model.as_deref()).ok()
                })
                .map(catalog::pin);
            self.effort = self.settings.effort.clone();
        }
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

    fn thread_ref(&self, id: usize) -> Option<&Thread> {
        self.threads.iter().find(|t| t.id == id)
    }

    fn running(&self) -> bool {
        matches!(self.page, Page::Thread(id) if self.threads.iter().any(|t| t.id == id && t.feed.running))
    }

    /// Reads a saved chat's history into its feed, once.
    fn load_history(&mut self, id: usize) {
        let Some(thread) = self.thread_ref(id) else {
            return;
        };
        if thread.loaded {
            return;
        }
        let history = thread
            .session
            .as_deref()
            .and_then(|session| self.backend.history(session));
        if let Some(thread) = self.thread(id) {
            thread.loaded = true;
            if let Some(session) = history {
                thread.feed = Feed::from_session(&session);
            }
        }
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
                    choice: self.choice.clone(),
                    effort: self.effort.clone(),
                    title: title.into(),
                    feed: Feed::default(),
                    session: None,
                    loaded: true,
                    chat: None,
                    pending: HashMap::new(),
                    touched: unix_now(),
                    _updates: None,
                });
                id
            }
        };
        if self.thread(id).is_some_and(|t| t.chat.is_none())
            && let Err(err) = self.start(id, cx)
        {
            if let Some(thread) = self.thread(id) {
                thread.feed.send(text);
                thread.feed.fail(format!("{err:#}"), false);
            }
            self.open(Page::Thread(id), cx);
            return;
        }
        let choice = self.choice.clone();
        let effort = self.effort.clone();
        let Some(thread) = self.thread(id) else {
            return;
        };
        thread.feed.send(text.clone());
        thread.touched = unix_now();
        match (&thread.chat, choice) {
            (Some(chat), Some(choice)) if chat.send(text, &choice, effort.as_deref()) => {
                thread.provider = choice.provider.id;
            }
            _ => thread.feed.fail(
                "No AI is set up yet: click the gear at the bottom left to pick one.".into(),
                false,
            ),
        }
        self.open(Page::Thread(id), cx);
    }

    fn start(&mut self, id: usize, cx: &mut Context<Self>) -> anyhow::Result<()> {
        let Some(choice) = self.choice.clone() else {
            anyhow::bail!("No AI is set up yet: click the gear at the bottom left to pick one.");
        };
        let effort = self.effort.clone();
        let mode = self.approval;
        let (tx, mut rx) = mpsc::unbounded_channel();
        self.load_history(id);
        let Some((root, resume)) = self.thread(id).map(|t| (t.root.clone(), t.session.clone()))
        else {
            anyhow::bail!("the chat was closed");
        };
        let chat = self
            .backend
            .start(&root, &choice, effort, mode, resume.as_deref(), tx)?;
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
            thread.session = Some(chat.session_id.clone());
            thread.provider = choice.provider.id;
            thread.chat = Some(chat);
            thread._updates = Some(updates);
        }
        Ok(())
    }

    fn update(&mut self, id: usize, update: Update, cx: &mut Context<Self>) {
        let Some(thread) = self.thread(id) else {
            return;
        };
        match update {
            Update::Event(event) => {
                match &event {
                    rozu_core::AgentEvent::ApprovalResolved { request_id, .. } => {
                        thread.pending.remove(request_id);
                    }
                    rozu_core::AgentEvent::TurnFinished(_) => thread.pending.clear(),
                    _ => {}
                }
                thread.feed.apply(event);
            }
            Update::Ask(request, answer) => {
                thread.pending.insert(request.call_id.clone(), answer);
                thread.feed.ask(request);
            }
            Update::Failed { message, offline } => {
                thread.pending.clear();
                thread.feed.fail(message, offline);
            }
        }
        thread.touched = unix_now();
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
            Decision::ApproveUnsandboxed => rozu_runtime::unsandboxed_approval_label(&ask.request),
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

    /// Closes a chat: stops its turn, denies its open prompts and drops it from the sidebar.
    /// With `forget`, its saved history is deleted too.
    fn delete(&mut self, id: usize, forget: bool, cx: &mut Context<Self>) {
        if let Some(index) = self.threads.iter().position(|t| t.id == id) {
            let mut thread = self.threads.remove(index);
            if forget && let Some(session) = &thread.session {
                self.backend.forget(session, thread.chat.take());
            }
        }
        if self.page == Page::Thread(id) {
            self.open(Page::Home, cx);
        }
        cx.notify();
    }

    /// Asks before deleting a chat that has a saved history.
    fn remove_chat(&mut self, id: usize, window: &mut Window, cx: &mut Context<Self>) {
        let saved = self
            .threads
            .iter()
            .any(|t| t.id == id && t.session.is_some());
        if !saved {
            return self.delete(id, true, cx);
        }
        let answer = window.prompt(
            PromptLevel::Warning,
            "Delete this chat?",
            Some("Its saved history is removed from this Mac."),
            &["Delete", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await != Ok(0) {
                return;
            }
            let _ = this.update(cx, |app, cx| app.delete(id, true, cx));
        })
        .detach();
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
                if app.settings.add_project(root.clone()) {
                    app.settings.save();
                    let mut saved = saved_threads(&app.backend, std::slice::from_ref(&root));
                    for thread in &mut saved {
                        thread.id = app.next_id;
                        app.next_id += 1;
                    }
                    app.threads.extend(saved);
                }
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
            Some("Its chats close but stay saved; they come back if you add the folder again. The folder and its files stay on your Mac."),
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
                    app.delete(id, false, cx);
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
        let efforts = catalog::efforts(choice.provider.id);
        if self
            .effort
            .as_deref()
            .is_some_and(|e| !efforts.contains(&e))
        {
            self.effort = None;
        }
        self.choice = Some(choice);
        if let Page::Thread(id) = self.page {
            let choice = self.choice.clone();
            let effort = self.effort.clone();
            if let Some(thread) = self.thread(id) {
                thread.choice = choice;
                thread.effort = effort;
            }
        }
        self.save_choice();
        cx.notify();
    }

    fn set_effort(&mut self, effort: Option<String>, cx: &mut Context<Self>) {
        self.effort = effort.clone();
        if let Page::Thread(id) = self.page
            && let Some(thread) = self.thread(id)
        {
            thread.effort = effort;
        }
        self.save_choice();
        cx.notify();
    }

    /// Applies to chats started from now on; a running chat keeps its mode.
    fn set_approval(&mut self, mode: ApprovalMode, cx: &mut Context<Self>) {
        self.approval = mode;
        self.settings.approval = Some(catalog::approval_arg(mode).to_owned());
        self.settings.save();
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
        self.open(Page::Setup, cx);
        if let Some(choice) = &self.choice
            && let Some(plan) = PLANS.iter().find(|p| p.id == choice.provider.id)
        {
            self.plan = plan.id;
        } else {
            self.pick_plan(PLANS[0].id, cx);
        }
        self.check(cx);
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
        self.providers = catalog::providers();
        self.open(Page::Home, cx);
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

/// The chats saved for `projects`, newest first, ready to be reopened.
fn saved_threads(backend: &Backend, projects: &[PathBuf]) -> Vec<Thread> {
    let mut threads = Vec::new();
    for root in projects {
        for row in backend.saved_chats(root) {
            threads.push(Thread {
                id: threads.len(),
                root: root.clone(),
                provider: registry::provider_info(&row.provider).map_or("", |p| p.id),
                choice: registry::resolve(Some(&row.provider), Some(&row.model))
                    .ok()
                    .map(catalog::pin),
                effort: None,
                title: row.title.into(),
                feed: Feed::default(),
                session: Some(row.id),
                loaded: false,
                chat: None,
                pending: HashMap::new(),
                touched: row.updated,
                _updates: None,
            });
        }
    }
    threads
}
