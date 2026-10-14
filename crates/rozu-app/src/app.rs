//! The window: sidebar (nav, projects, repositories with their threads, account) and the
//! centered chat, or the New Chat page.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Instant;

use gpui::{
    AnyElement, App, Application, Bounds, Context, Div, Entity, FocusHandle, Focusable, KeyBinding,
    Menu, MenuItem, MouseButton, ScrollHandle, SharedString, Stateful, Subscription, Svg, Task,
    TitlebarOptions, Window, WindowBounds, WindowOptions, actions, div, point, prelude::*, px,
    relative, rgb, size, svg,
};
use rozu_activity::ActivityState;
use rozu_core::{ApprovalRequest, Decision};
use rozu_providers::ModelChoice;
use tokio::sync::{mpsc, oneshot};

use crate::backend::{self, Backend, Chat, Update};
use crate::composer::{self, Composer, Sent};
use crate::feed::{Ask, Feed, Item, Step};
use crate::orb::OrbView;
use crate::theme::*;

actions!(rozu, [Quit]);

pub fn run() {
    let root = std::env::args_os().nth(1).map(PathBuf::from);
    let root = root
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_default();
    let backend = Backend::new(&root).unwrap_or_else(|err| {
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
            let bounds = Bounds::centered(None, size(px(1280.), px(840.)), cx);
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
                cx.new(|cx| Rozu::new(backend, window, cx))
            })
            .expect("open the window");
            cx.on_window_closed(|cx| cx.quit()).detach();
            cx.activate(true);
        });
}

struct Thread {
    title: SharedString,
    feed: Feed,
    chat: Option<Chat>,
    pending: HashMap<String, oneshot::Sender<Decision>>,
    touched: Instant,
    _updates: Option<Task<()>>,
}

struct Rozu {
    backend: Backend,
    repo: SharedString,
    repo_open: bool,
    threads: Vec<Thread>,
    /// The open thread; `None` shows New Chat.
    current: Option<usize>,
    choices: Vec<ModelChoice>,
    choice: usize,
    composer: Entity<Composer>,
    orbs: HashMap<String, Entity<OrbView>>,
    scroll: ScrollHandle,
    _sent: Subscription,
}

impl Rozu {
    fn new(backend: Backend, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let composer = cx.new(Composer::new);
        let sent = cx.subscribe(&composer, |this, _, Sent(text), cx| {
            this.send(text.clone(), cx)
        });
        window.focus(&composer.focus_handle(cx));
        let repo = backend
            .root()
            .file_name()
            .map_or("workspace".into(), |n| n.to_string_lossy().into_owned());
        let mut app = Self {
            backend,
            repo: repo.into(),
            repo_open: true,
            threads: Vec::new(),
            current: None,
            choices: backend::choices(),
            choice: 0,
            composer,
            orbs: HashMap::new(),
            scroll: ScrollHandle::new(),
            _sent: sent,
        };
        app.open(None, cx);
        app
    }

    fn open(&mut self, thread: Option<usize>, cx: &mut Context<Self>) {
        self.current = thread;
        let placeholder = if thread.is_some() {
            "Send follow-up"
        } else {
            "Plan, build, / for skills, @ for context"
        };
        self.composer.update(cx, |c, cx| {
            c.placeholder = placeholder.into();
            cx.notify();
        });
        self.scroll.scroll_to_bottom();
        cx.notify();
    }

    fn send(&mut self, text: String, cx: &mut Context<Self>) {
        let ix = match self.current {
            Some(ix) if self.threads[ix].feed.running => {
                return self.composer.update(cx, |c, cx| c.set_text(&text, cx));
            }
            Some(ix) => ix,
            None => {
                let title: String = text.lines().next().unwrap_or("").chars().take(60).collect();
                self.threads.push(Thread {
                    title: title.into(),
                    feed: Feed::default(),
                    chat: None,
                    pending: HashMap::new(),
                    touched: Instant::now(),
                    _updates: None,
                });
                self.threads.len() - 1
            }
        };
        if self.threads[ix].chat.is_none() {
            self.start(ix, cx);
        }
        let thread = &mut self.threads[ix];
        thread.feed.send(text.clone());
        thread.touched = Instant::now();
        match &thread.chat {
            Some(chat) if chat.send(text) => {}
            _ => thread.feed.fail(
                "No model is set up: install the claude, codex or grok CLI, or add an API key with `rozu auth set`.".into(),
                false,
            ),
        }
        self.open(Some(ix), cx);
    }

    fn start(&mut self, ix: usize, cx: &mut Context<Self>) {
        let Some(choice) = self.choices.get(self.choice) else {
            return;
        };
        let (tx, mut rx) = mpsc::unbounded_channel();
        let thread = &mut self.threads[ix];
        thread.chat = Some(self.backend.start(choice, tx));
        thread._updates = Some(cx.spawn(async move |this, cx| {
            while let Some(update) = rx.recv().await {
                if this
                    .update(cx, |app, cx| app.update(ix, update, cx))
                    .is_err()
                {
                    break;
                }
            }
        }));
    }

    fn update(&mut self, ix: usize, update: Update, cx: &mut Context<Self>) {
        let thread = &mut self.threads[ix];
        match update {
            Update::Event(event) => thread.feed.apply(event),
            Update::Ask(request, answer) => {
                thread.pending.insert(request.call_id.clone(), answer);
                thread.feed.ask(request);
            }
            Update::Failed { message, offline } => thread.feed.fail(message, offline),
        }
        thread.touched = Instant::now();
        if self.current == Some(ix) {
            self.scroll.scroll_to_bottom();
        }
        cx.notify();
    }

    fn answer(&mut self, ix: usize, item: usize, decision: Decision, cx: &mut Context<Self>) {
        let thread = &mut self.threads[ix];
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
        if let Some(chat) = self.current.and_then(|ix| self.threads[ix].chat.as_ref()) {
            chat.cancel();
        }
        cx.notify();
    }

    fn orb(
        &mut self,
        key: String,
        state: ActivityState,
        cx: &mut Context<Self>,
    ) -> Entity<OrbView> {
        let orb = self
            .orbs
            .entry(key)
            .or_insert_with(|| cx.new(|cx| OrbView::new(state, cx)))
            .clone();
        orb.update(cx, |o, cx| o.set(state, cx));
        orb
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = [
            ("chat", "New Chat"),
            ("search", "Search"),
            ("bot", "Automations"),
            ("blocks", "Customize"),
        ];
        let mut threads: Vec<usize> = (0..self.threads.len()).collect();
        threads.sort_by_key(|&ix| std::cmp::Reverse(self.threads[ix].touched));
        let rows: Vec<AnyElement> = if self.repo_open {
            threads
                .into_iter()
                .map(|ix| self.thread_row(ix, cx).into_any_element())
                .collect()
        } else {
            Vec::new()
        };
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
                let on = label == "New Chat" && self.current.is_none();
                row(label, i, label)
                    .when(on, |r| r.bg(rgb(RAISE)))
                    .when(label == "New Chat", |r| {
                        r.on_click(cx.listener(|this, _, _, cx| this.open(None, cx)))
                    })
            }))
            .child(heading("Projects"))
            .child(row("new-project", "plus", "New Project"))
            .child(heading("Repositories"))
            .child(
                row("repo", "folder", self.repo.clone())
                    .child(div().flex_1())
                    .child(icon(if self.repo_open { "down" } else { "right" }, INK3))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.repo_open = !this.repo_open;
                        cx.notify();
                    })),
            )
            .child(
                div()
                    .id("threads")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .children(rows),
            )
            .child(account())
    }

    fn thread_row(&mut self, ix: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let orb = self.orb(format!("t{ix}"), self.threads[ix].feed.state(), cx);
        let thread = &self.threads[ix];
        div()
            .id(("thread", ix))
            .flex()
            .items_center()
            .gap_3()
            .pl(px(14.))
            .pr_2()
            .py(px(6.))
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(RAISE)))
            .when(self.current == Some(ix), |r| r.bg(rgb(RAISE)))
            .child(orb)
            .child(div().flex_1().truncate().child(thread.title.clone()))
            .child(
                div()
                    .text_size(px(13.5))
                    .text_color(rgb(INK3))
                    .child(age(thread.touched)),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.open(Some(ix), cx)))
    }

    fn thread_page(&mut self, ix: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let mut items = Vec::new();
        for (i, item) in self.threads[ix].feed.items.iter().enumerate() {
            let live = match item {
                Item::Step(s) if s.done.is_none() => Some(s.state),
                Item::Ask(a) => Some(match a.answer {
                    None => ActivityState::Waiting,
                    Some("Denied" | "Expired") => ActivityState::Offline,
                    Some(_) => ActivityState::Done,
                }),
                _ => None,
            };
            items.push((i, live.map(|s| (s, format!("{ix}-{i}")))));
        }
        let items: Vec<AnyElement> = items
            .into_iter()
            .map(|(i, live)| {
                let orb = live.map(|(state, key)| self.orb(key, state, cx));
                self.item(ix, i, orb, cx)
            })
            .collect();
        let thread = &self.threads[ix];
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(
                drag_bar().px(px(10.)).child(
                    div()
                        .bg(rgb(RAISE))
                        .rounded(px(8.))
                        .px(px(10.))
                        .py(px(3.))
                        .child(thread.title.clone()),
                ),
            )
            .child(
                div()
                    .id("feed")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .track_scroll(&self.scroll)
                    .child(
                        column()
                            .gap(px(14.))
                            .py(px(10.))
                            .pb(px(20.))
                            .children(items),
                    ),
            )
            .child(column().pb(px(16.)).child(self.composer_box(false, cx)))
    }

    fn item(
        &self,
        ix: usize,
        i: usize,
        orb: Option<Entity<OrbView>>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        match &self.threads[ix].feed.items[i] {
            Item::User(text) => div()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(BOX))
                .rounded(px(14.))
                .px(px(18.))
                .py(px(12.))
                .child(text.clone())
                .into_any_element(),
            Item::Text(text) => div()
                .line_height(relative(1.6))
                .child(text.clone())
                .into_any_element(),
            Item::Note(text) => div()
                .text_color(rgb(INK3))
                .text_size(px(14.))
                .child(text.clone())
                .into_any_element(),
            Item::Step(step) => step_row(step, orb).into_any_element(),
            Item::Ask(ask) => self.ask_card(ix, i, ask, orb, cx).into_any_element(),
        }
    }

    fn ask_card(
        &self,
        ix: usize,
        i: usize,
        ask: &Ask,
        orb: Option<Entity<OrbView>>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
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
                    .children(orb)
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
                                move |this, _, _, cx| this.answer(ix, i, decision.clone(), cx),
                            ))
                        },
                    )),
            })
    }

    fn composer_box(&self, home: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.current.is_some_and(|ix| self.threads[ix].feed.running);
        let model = self
            .choices
            .get(self.choice)
            .map_or("No model".into(), backend::label);
        let picker = div()
            .id("model")
            .flex()
            .items_center()
            .gap_1()
            .text_size(px(14.))
            .text_color(rgb(INK2))
            .cursor_pointer()
            .child(model)
            .child(icon("down", INK2))
            .on_click(cx.listener(|this, _, _, cx| {
                this.choice = (this.choice + 1) % this.choices.len().max(1);
                cx.notify();
            }));
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
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(14.))
                            .text_color(rgb(INK2))
                            .child(icon("folder", INK2))
                            .child(self.repo.clone()),
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
}

impl Render for Rozu {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let main = match self.current {
            Some(ix) => self.thread_page(ix, cx).into_any_element(),
            None => self.home_page(cx).into_any_element(),
        };
        div()
            .size_full()
            .flex()
            .bg(rgb(BG))
            .text_color(rgb(INK))
            .font_family(".SystemUIFont")
            .text_size(px(15.))
            .child(self.sidebar(cx))
            .child(main)
    }
}

impl Focusable for Rozu {
    fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.composer.focus_handle(cx)
    }
}

fn step_row(step: &Step, orb: Option<Entity<OrbView>>) -> Div {
    let line = div().flex().items_center().gap_2().text_color(rgb(INK2));
    match (&step.done, orb) {
        (None, orb) => line.children(orb).child(step.label.clone()),
        (Some(done), _) if step.failed => line.text_color(rgb(BAD)).child(done.clone()),
        (Some(done), _) => {
            let (verb, rest) = done.split_once(' ').unwrap_or((done, ""));
            line.gap_1()
                .child(verb.to_owned())
                .child(div().text_color(rgb(INK3)).child(rest.to_owned()))
        }
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

fn account() -> impl IntoElement {
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
