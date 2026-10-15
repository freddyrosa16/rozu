//! The window and its state: which page is open, the chats (threads) per project, the chosen
//! model, effort and approval mode, and every action the pages trigger. How each page is drawn
//! lives in a child module: [`sidebar`], [`chat`], [`home`], [`setup`] and [`menu`] (the composer
//! and model picker), on top of the shared [`widgets`].

mod chat;
mod customize;
mod home;
mod menu;
mod search;
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
                window_min_size: Some(size(px(560.), px(440.))),
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

impl Thread {
    fn from_saved(id: usize, root: PathBuf, row: rozu_store::SessionSummary) -> Self {
        Self {
            id,
            root,
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
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Page {
    Home,
    Thread(usize),
    Setup,
    Search,
    Customize,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum DraftKey {
    Project(Option<PathBuf>),
    Thread(usize),
}

#[derive(Default)]
struct Drafts {
    active: Option<DraftKey>,
    saved: HashMap<DraftKey, String>,
}

impl Drafts {
    fn switch(&mut self, next: Option<DraftKey>, text: &str) -> Option<String> {
        let next = next?;
        if self.active.as_ref() == Some(&next) {
            return None;
        }
        if let Some(previous) = self.active.replace(next.clone()) {
            if text.is_empty() {
                self.saved.remove(&previous);
            } else {
                self.saved.insert(previous, text.to_owned());
            }
        }
        Some(self.saved.remove(&next).unwrap_or_default())
    }

    fn discard_submitted(&mut self, key: DraftKey, text: &str) {
        if self.saved.get(&key).is_some_and(|draft| draft == text) {
            self.saved.remove(&key);
        }
    }
}

fn follows_output(page: Page, id: usize, offset_y: f32, max_y: f32) -> bool {
    page == Page::Thread(id) && max_y + offset_y <= 1.
}

fn sidebar_width(viewport: f32, first_run: bool) -> f32 {
    if first_run {
        0.
    } else if viewport < 900. {
        88.
    } else {
        270.
    }
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
    page_focus: FocusHandle,
    focus_page: bool,
    compact: bool,
    sidebar_open: bool,
    providers: Vec<&'static ProviderInfo>,
    choice: Option<ModelChoice>,
    effort: Option<String>,
    /// What runs without asking in new chats (see `rozu chat --approval`).
    approval: ApprovalMode,
    menu: bool,
    /// The model menu's search box, and the window height it must fit in.
    search: Entity<Composer>,
    chat_search: Entity<Composer>,
    search_state: search::SearchState,
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
    drafts: Drafts,
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
            if this.menu
                && let Some(m) = this.matches(query).into_iter().next()
            {
                this.choose(m, cx);
            }
        });
        cx.observe(&search, |_, _, cx| cx.notify()).detach();
        let chat_search = cx.new(|cx| {
            let mut c = Composer::new(cx);
            c.placeholder = "Search chat titles".into();
            c
        });
        cx.observe(&chat_search, |this, _, cx| this.search_chats(cx))
            .detach();
        cx.subscribe(&chat_search, |this, _, Sent(query), cx| {
            if this.page == Page::Search {
                this.chat_search.update(cx, |c, cx| c.set_text(query, cx));
            }
        })
        .detach();
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
            page_focus: cx.focus_handle(),
            focus_page: false,
            compact: false,
            sidebar_open: false,
            providers: catalog::providers(),
            choice,
            menu: false,
            search,
            chat_search,
            search_state: search::SearchState::default(),
            viewport_h: 800.,
            text_w: 700.,
            _search: search_sub,
            live: None,
            plan: PLANS[0].id,
            access: HashMap::new(),
            composer,
            drafts: Drafts::default(),
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
        let key = match page {
            Page::Home => Some(DraftKey::Project(self.project.clone())),
            Page::Thread(id) => Some(DraftKey::Thread(id)),
            Page::Setup | Page::Search | Page::Customize => None,
        };
        let draft = self.drafts.switch(key, &self.composer.read(cx).text);
        if let Page::Thread(id) = page {
            self.load_history(id);
            if let Some(thread) = self.thread_ref(id) {
                let choice = thread.choice.clone();
                let effort = thread.effort.clone();
                self.choice = choice;
                self.effort = effort;
            }
        } else if matches!(page, Page::Home | Page::Customize) {
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
        self.focus_page = true;
        self.sidebar_open = false;
        self.menu = false;
        let placeholder = match page {
            Page::Thread(_) => "Send follow-up",
            _ => "Plan, build, / for skills, @ for context",
        };
        self.composer.update(cx, |c, cx| {
            if let Some(draft) = draft {
                c.set_text(&draft, cx);
            }
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
        if !matches!(self.page, Page::Home | Page::Thread(_)) {
            self.composer.update(cx, |c, cx| c.set_text(&text, cx));
            return;
        }
        let id = match self.page {
            Page::Thread(_) if self.running() => {
                return self.composer.update(cx, |c, cx| c.set_text(&text, cx));
            }
            Page::Thread(id) => id,
            _ => {
                let Some(root) = self.project.clone() else {
                    self.composer.update(cx, |c, cx| c.set_text(&text, cx));
                    return self.pick_project(Some(text), cx);
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
        let follow = follows_output(
            self.page,
            id,
            self.scroll.offset().y.into(),
            self.scroll.max_offset().height.into(),
        );
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
        if follow {
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
                self.search_state.forget(session);
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
        self.pick_project(None, cx);
    }

    fn pick_project(&mut self, pending: Option<String>, cx: &mut Context<Self>) {
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
                if app.page != Page::Setup || pending.is_some() {
                    app.open(Page::Home, cx);
                }
                if let Some(text) = pending {
                    app.drafts.discard_submitted(DraftKey::Project(None), &text);
                    app.send(text, cx);
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
                    if app.page == Page::Home {
                        app.open(Page::Home, cx);
                    }
                }
                if app.page == Page::Search {
                    app.search_chats(cx);
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
            self.choose(catalog::pin(choice), cx);
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
        if self.focus_page {
            window.focus(&self.focus_handle(cx));
            self.focus_page = false;
        }
        self.viewport_h = window.viewport_size().height.into();
        let viewport_w = f32::from(window.viewport_size().width);
        let first_run = self.settings.provider.is_none();
        self.compact = viewport_w < 900.;
        let main_w = viewport_w - sidebar_width(viewport_w, first_run);
        self.text_w = (main_w.min(760.) - 56.).max(1.);
        let main = match self.page {
            Page::Thread(id) => self.thread_page(id, window, cx).into_any_element(),
            Page::Home => self.home_page(cx).into_any_element(),
            Page::Setup => self.setup_page(cx).into_any_element(),
            Page::Search => self.search_page(cx).into_any_element(),
            Page::Customize => self.customize_page(cx).into_any_element(),
        };
        div()
            .track_focus(&self.page_focus)
            .size_full()
            .relative()
            .flex()
            .bg(rgb(BG))
            .text_color(rgb(INK))
            .font_family(".SystemUIFont")
            .text_size(px(15.))
            .when(!first_run && !self.compact, |d| d.child(self.sidebar(cx)))
            .when(!first_run && self.compact, |d| {
                d.child(self.compact_sidebar(cx))
            })
            .child(main)
            .when(!first_run && self.compact && self.sidebar_open, |d| {
                d.child(
                    deferred(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .size_full()
                            .flex()
                            .occlude()
                            .child(self.sidebar(cx))
                            .child(
                                div()
                                    .id("close-sidebar")
                                    .flex_1()
                                    .h_full()
                                    .bg(gpui::rgba(0x00000066))
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.sidebar_open = false;
                                        cx.notify();
                                    })),
                            ),
                    )
                    .with_priority(3),
                )
            })
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
                                window.focus(&this.focus_handle(cx));
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
        match self.page {
            Page::Home | Page::Thread(_) => self.composer.focus_handle(cx),
            Page::Search => self.chat_search.focus_handle(cx),
            Page::Setup | Page::Customize => self.page_focus.clone(),
        }
    }
}

/// The chats saved for `projects`, newest first, ready to be reopened.
fn saved_threads(backend: &Backend, projects: &[PathBuf]) -> Vec<Thread> {
    let mut threads = Vec::new();
    for root in projects {
        for row in backend.saved_chats(root) {
            threads.push(Thread::from_saved(threads.len(), root.clone(), row));
        }
    }
    threads
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_threads_keep_metadata_and_load_history_only_when_opened() {
        for (provider, model, expected_provider, expected_model) in [
            ("claude-cli", "opus", "claude-cli", Some("claude-opus-")),
            (
                "codex",
                "custom-next-model",
                "codex",
                Some("custom-next-model"),
            ),
            ("retired-provider", "old-model", "", None),
        ] {
            let row = rozu_store::SessionSummary {
                id: "saved-session".into(),
                workspace: "/canonical/project".into(),
                provider: provider.into(),
                model: model.into(),
                title: "Saved title — preserved".into(),
                created: 10,
                updated: 42,
                messages: 8,
            };
            let thread = Thread::from_saved(7, "/selected/project".into(), row);
            assert_eq!(thread.id, 7);
            assert_eq!(thread.root, Path::new("/selected/project"));
            assert_eq!(thread.provider, expected_provider);
            match expected_model {
                Some(model) => assert!(thread.choice.as_ref().unwrap().model.starts_with(model)),
                None => assert!(thread.choice.is_none()),
            }
            assert_eq!(thread.title.as_ref(), "Saved title — preserved");
            assert_eq!(thread.session.as_deref(), Some("saved-session"));
            assert_eq!(thread.touched, 42);
            assert!(!thread.loaded);
            assert!(thread.effort.is_none());
            assert!(thread.feed.items.is_empty());
            assert_eq!(thread.feed.state(), ActivityState::Offline);
            assert!(thread.chat.is_none());
            assert!(thread.pending.is_empty());
            assert!(thread._updates.is_none());
        }
    }

    #[test]
    fn compact_navigation_keeps_a_readable_main_pane() {
        for (width, expected_sidebar) in [(560., 88.), (899., 88.), (900., 270.), (1600., 270.)] {
            assert_eq!(sidebar_width(width, false), expected_sidebar);
            assert!(width - sidebar_width(width, false) >= 472.);
            assert_eq!(sidebar_width(width, true), 0.);
        }
    }

    #[test]
    fn output_follows_only_the_visible_chat_at_the_bottom() {
        for (page, offset, maximum, expected) in [
            (Page::Thread(0), 0., 0., true),
            (Page::Thread(0), -100., 100., true),
            (Page::Thread(0), -99.5, 100., true),
            (Page::Thread(0), -40., 100., false),
            (Page::Thread(0), 0., 100., false),
            (Page::Thread(1), -100., 100., false),
            (Page::Home, 0., 0., false),
            (Page::Setup, 0., 0., false),
            (Page::Search, 0., 0., false),
            (Page::Customize, 0., 0., false),
        ] {
            assert_eq!(follows_output(page, 0, offset, maximum), expected);
        }
    }

    #[test]
    fn drafts_stay_with_their_chat_and_project() {
        let mut drafts = Drafts::default();
        let a = Some(DraftKey::Thread(0));
        let b = Some(DraftKey::Thread(1));
        let project_a = Some(DraftKey::Project(Some("/a".into())));
        let project_b = Some(DraftKey::Project(Some("/b".into())));
        assert_eq!(drafts.switch(a.clone(), ""), Some(String::new()));
        assert_eq!(
            drafts.switch(b.clone(), "chat a\nsecond line"),
            Some(String::new())
        );
        assert_eq!(
            drafts.switch(project_a.clone(), "chat b"),
            Some(String::new())
        );
        assert_eq!(
            drafts.switch(project_b.clone(), "project a"),
            Some(String::new())
        );
        assert_eq!(
            drafts.switch(a, "project b"),
            Some("chat a\nsecond line".into())
        );
        assert_eq!(drafts.switch(b, "chat a revised"), Some("chat b".into()));
        assert_eq!(drafts.switch(project_a, "chat b"), Some("project a".into()));
        assert_eq!(
            drafts.switch(project_b, "project a"),
            Some("project b".into())
        );
    }

    #[test]
    fn setup_preserves_the_draft_and_sent_text_does_not_return() {
        let mut drafts = Drafts::default();
        let chat = Some(DraftKey::Thread(0));
        let project = Some(DraftKey::Project(None));
        assert_eq!(drafts.switch(chat.clone(), ""), Some(String::new()));
        assert_eq!(drafts.switch(None, "unsent"), None);
        assert_eq!(drafts.switch(chat.clone(), "unsent"), None);
        assert_eq!(
            drafts.switch(project.clone(), "unsent"),
            Some(String::new())
        );
        assert_eq!(
            drafts.switch(chat.clone(), "new chat"),
            Some("unsent".into())
        );
        assert_eq!(drafts.switch(project, ""), Some("new chat".into()));
        assert_eq!(drafts.switch(chat, "new chat"), Some(String::new()));
    }

    #[test]
    fn choosing_a_folder_for_a_submitted_message_preserves_its_existing_draft() {
        let mut drafts = Drafts::default();
        let unassigned = DraftKey::Project(None);
        let project = Some(DraftKey::Project(Some("/project".into())));
        let chat = Some(DraftKey::Thread(0));
        drafts.switch(project.clone(), "");
        drafts.switch(Some(unassigned.clone()), "existing project draft");

        let existing = drafts.switch(project.clone(), "submitted message").unwrap();
        assert_eq!(existing, "existing project draft");
        drafts.discard_submitted(unassigned.clone(), "submitted message");
        assert_eq!(drafts.switch(chat, &existing), Some(String::new()));
        assert_eq!(drafts.switch(project, ""), Some(existing.clone()));
        assert_eq!(
            drafts.switch(Some(unassigned), &existing),
            Some(String::new())
        );
    }
}
