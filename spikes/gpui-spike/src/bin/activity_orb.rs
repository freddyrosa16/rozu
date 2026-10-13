//! v7.3 activity pill demo: the dot orb (22px in the pill, 88px large), the label and a mono
//! timer.
//!
//! Two modes (key A switches; `-- --agent` starts in the second):
//! - **cycle** (default): all ten states, 3.2s each. Labels come from a scripted turn run through
//!   `rozu_activity::ActivityTracker`; Offline is set directly.
//! - **agent**: replays `rozu_activity::replay::demo_turn()`, a real `rozu_core::AgentEvent`
//!   turn, through the AgentEvent → ActivityState mapping and the pill's debouncer
//!   (`replay::replay`): Thinking, Reading, Running, Waiting while the approval is pending, Running,
//!   Editing, Done. It loops 3s after Done. The pill shows the last `AgentEvent` fed.
//!
//! Keys: M reduced motion · Space pause · ←/→ previous/next state · A mode · T theme ·
//! B low power (30fps) · S state size (on by default as in v7.3; off shows the v7.2 bug, where
//! every dot is drawn at size 1).
//!
//! Reduced motion starts from the OS setting (macOS: System Settings → Accessibility → Display →
//! Reduce motion), re-read every second. M overrides it; the override wins over OS changes until
//! M is pressed again to get back to the OS value (`rozu_activity::ReducedMotion`).
//!
//! Rendering pauses (0fps) when the window loses focus, is fully covered or minimised (macOS
//! occlusion state), and after Done/Error settle. The animation clock stops with it, so it
//! resumes with no jump. Each orb is its own cached view: an animation frame notifies only the two
//! orb views (the 22px one at most 30fps), and the pill text and status lines are reused from the
//! previous frame.
use std::time::{Duration, Instant};

use gpui::{
    AnyView, App, Application, Bounds, Context, Entity, FocusHandle, FontWeight, KeyDownEvent,
    Pixels, Rgba, SharedString, StyleRefinement, Subscription, Window, WindowBounds,
    WindowOptions, canvas, div, fill, point, prelude::*, px, rgb, size,
};
use rozu_activity::replay::{demo_turn, replay};
use rozu_activity::tokens::{ORB_LG_PX, ORB_PX, PILL_HEIGHT_DOCK_PX, Palette, Theme};
use rozu_activity::{
    Activity, ActivityEvent, ActivityState, ActivityTracker, DrawDot, FramePolicy, Orb,
    ProjectOptions, ReducedMotion, Sample, project_with,
};

/// Seconds each state stays up before the cycle moves on.
const STEP_SECS: f64 = 3.2;
/// Seconds the agent turn holds on its last state before it replays.
const AGENT_HOLD_SECS: f64 = 3.0;
/// Poll interval while no frames are needed (the cycle timer and the pill's clock still run).
const IDLE_POLL: Duration = Duration::from_millis(100);
/// How often the OS Reduce motion setting is re-read.
const OS_POLL: Duration = Duration::from_secs(1);
/// The 22px orb's frame cap; the large one draws at the policy's rate.
const SMALL_ORB_FPS: u32 = 30;
/// The pill's slot: the pill hugs its content, left-aligned in a fixed box so its text can be a
/// cached view (a cached view's size comes from its style, not its content).
const PILL_SLOT_W: f32 = 460.0;
/// Where the 22px orb sits over the pill: border 1 + padding 6, centred vertically.
const PILL_ORB_X: f32 = 7.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Cycle,
    Agent,
}

/// One scripted step: the agent event (or a direct activity, for Offline) that produces it.
enum Step {
    Event(ActivityEvent),
    Direct(Activity),
}

fn script() -> Vec<Step> {
    let started = |name: &str, args: &str| {
        Step::Event(ActivityEvent::ToolStarted {
            id: format!("call_{name}"),
            name: name.into(),
            args_json: args.into(),
        })
    };
    let mut offline = Activity::new(ActivityState::Offline, "Offline · retrying");
    offline.detail = Some("in 3s".into());
    vec![
        Step::Event(ActivityEvent::TextDelta),
        started("TodoWrite", r#"{"todos":[]}"#),
        started("read_file", r#"{"path":"README.md"}"#),
        started("grep", r#"{"pattern":"set_size"}"#),
        started("apply_patch", r#"{"path":"src/term/view.rs"}"#),
        started("shell", r#"{"command":"cargo test"}"#),
        Step::Event(ActivityEvent::ApprovalPending { call_id: "call_shell".into(), tool: "shell".into() }),
        Step::Event(ActivityEvent::TurnCompleted),
        Step::Event(ActivityEvent::ToolFinished { id: "call_x".into(), name: "shell".into(), is_error: true }),
        Step::Direct(offline),
    ]
}

/// The agent mode's turn: when each `AgentEvent` arrives (its variant name, for the pill), and
/// what the pill shows over it.
struct AgentTurn {
    events: Vec<(f64, &'static str)>,
    timeline: Vec<(f64, Option<Activity>)>,
}

impl AgentTurn {
    fn new() -> Self {
        let turn = demo_turn();
        let events = turn.iter().map(|(t, e)| (*t, event_name(e))).collect();
        AgentTurn { events, timeline: replay(&turn) }
    }

    /// The timeline entry showing at `t`.
    fn index_at(&self, t: f64) -> usize {
        self.timeline.iter().rposition(|(at, _)| *at <= t).unwrap_or(0)
    }

    /// The last event fed by `t`.
    fn event_at(&self, t: f64) -> &'static str {
        self.events.iter().rev().find(|(at, _)| *at <= t).map_or("", |(_, name)| name)
    }

    fn end(&self) -> f64 {
        self.timeline.last().map_or(0.0, |(t, _)| *t) + AGENT_HOLD_SECS
    }
}

fn event_name(event: &rozu_core::AgentEvent) -> &'static str {
    use rozu_core::AgentEvent::*;
    match event {
        TextDelta(_) => "TextDelta",
        ToolCallStarted { .. } => "ToolCallStarted",
        ToolCallFinished { .. } => "ToolCallFinished",
        ApprovalPending(_) => "ApprovalPending",
        ApprovalResolved { .. } => "ApprovalResolved",
        Approval(_) => "Approval",
        Usage(_) => "Usage",
        TurnFinished(_) => "TurnFinished",
    }
}

/// The orb's state and animation clock, shared by the two orb views. Animation frames bump
/// `frame`; anything else that changes the drawing (a new state, theme, options) bumps `change`.
struct OrbClock {
    orb: Orb,
    /// Advances only while frames are drawn.
    anim_t: f64,
    samples: Vec<Sample>,
    opts: ProjectOptions,
    palette: Palette,
    /// The policy's rate at the last frame.
    fps: u32,
    animating: bool,
    frame: u64,
    change: u64,
}

impl OrbClock {
    fn resample(&mut self) {
        self.orb.sample_into(self.anim_t, &mut self.samples);
    }

    /// Redraws both orbs now.
    fn changed(&mut self, cx: &mut Context<Self>) {
        self.resample();
        self.change += 1;
        cx.notify();
    }
}

/// One orb, redrawn when the clock ticks (at most `max_fps`) or changes.
struct OrbView {
    clock: Entity<OrbClock>,
    size_px: f32,
    _observe: Subscription,
}

impl OrbView {
    fn new(clock: Entity<OrbClock>, size_px: f32, max_fps: u32, cx: &mut Context<Self>) -> Self {
        let mut seen_change = 0;
        let observe = cx.observe(&clock, move |_, clock, cx| {
            let c = clock.read(cx);
            let every = u64::from((c.fps / max_fps).max(1));
            if c.change != seen_change || c.frame % every == 0 {
                seen_change = c.change;
                cx.notify();
            }
        });
        OrbView { clock, size_px, _observe: observe }
    }
}

impl Render for OrbView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let c = self.clock.read(cx);
        let dots = project_with(&c.samples, self.size_px, window.scale_factor(), c.opts);
        orb_canvas(dots, self.size_px, c.palette)
    }
}

/// What the pill shows next to its orb.
#[derive(Clone, PartialEq)]
struct PillContent {
    activity: Activity,
    secs: u64,
    step: SharedString,
    palette: Palette,
}

/// The pill's background, label and meta text; the 22px orb is drawn over it.
struct PillView(PillContent);

impl Render for PillView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let PillContent { activity: a, secs, step, palette } = &self.0;
        let timer: SharedString = format!("{}:{:02}", secs / 60, secs % 60).into();
        let meta = |text: SharedString| div().text_color(rgb(palette.meta)).font_family("Menlo").child(text);
        // The pill: [orb 22px] [label] [mono timer] [· step] [detail] [hint].
        div().flex().flex_row().child(
            div()
                .flex()
                .flex_row()
                .items_center()
                .gap_2()
                .h(px(PILL_HEIGHT_DOCK_PX))
                .pl(px(6.0))
                .pr(px(12.0))
                .rounded_full()
                .bg(rgb(palette.pill_bg))
                .border_1()
                .border_color(rgb(palette.pill_border))
                .child(div().w(px(ORB_PX)).h(px(ORB_PX)).flex_none())
                .child(
                    div()
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(rgb(palette.label(a.state.tone())))
                        .child(SharedString::from(a.label.clone())),
                )
                .child(meta(timer))
                .child(meta(step.clone()))
                .children(a.detail.clone().map(|d| meta(d.into())))
                .children(a.hint.map(|h| meta(h.into()))),
        )
    }
}

/// The status and key-hint lines.
struct StatusView {
    status: SharedString,
    meta: u32,
}

impl Render for StatusView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .items_center()
            .gap_1()
            .text_xs()
            .text_color(rgb(self.meta))
            .child(self.status.clone())
            .child("M reduced motion · Space pause · ←/→ state · A mode · T theme · B low power · S state size")
    }
}

struct Demo {
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    clock: Entity<OrbClock>,
    large: Entity<OrbView>,
    small: Entity<OrbView>,
    pill: Entity<PillView>,
    status: Entity<StatusView>,
    mode: Mode,
    tracker: ActivityTracker,
    script: Vec<Step>,
    agent: AgentTurn,
    /// Seconds into the agent turn.
    turn_t: f64,
    /// The cycle step, or the agent timeline entry.
    step: usize,
    activity: Activity,
    /// Wall-clock seconds while focused and visible: drives the cycle and the pill timer.
    wall_t: f64,
    step_started: f64,
    last_tick: Instant,
    os_polled: Instant,
    motion: ReducedMotion,
    policy: FramePolicy,
    paused: bool,
    theme: Theme,
}

impl Demo {
    fn new(mode: Mode, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let activation = cx.observe_window_activation(window, |this: &mut Demo, window, cx| {
            this.policy.focused = window.is_window_active();
            // Don't count the time spent in the background.
            this.last_tick = Instant::now();
            this.sync_views(cx);
        });
        cx.spawn_in(window, async move |this, cx| {
            loop {
                let Ok(wait) = this.update_in(cx, |demo: &mut Demo, window, cx| demo.tick(window, cx)) else {
                    break;
                };
                cx.background_executor().timer(wait).await;
            }
        })
        .detach();

        let theme = Theme::Dark;
        let palette = Palette::for_theme(theme);
        let motion = ReducedMotion::new(platform::reduce_motion());
        let clock = cx.new(|_| {
            let mut orb = Orb::new(ActivityState::Thinking, 0.0);
            orb.set_reduced_motion(motion.effective());
            let mut clock = OrbClock {
                orb,
                anim_t: 0.0,
                samples: Vec::new(),
                opts: ProjectOptions::default(),
                palette,
                fps: 0,
                animating: false,
                frame: 0,
                change: 0,
            };
            clock.resample();
            clock
        });
        let large = cx.new(|cx| OrbView::new(clock.clone(), ORB_LG_PX, u32::MAX, cx));
        let small = cx.new(|cx| OrbView::new(clock.clone(), ORB_PX, SMALL_ORB_FPS, cx));
        let activity = Activity::new(ActivityState::Thinking, ActivityState::Thinking.label());
        let pill = cx.new(|_| {
            PillView(PillContent { activity: activity.clone(), secs: 0, step: "".into(), palette })
        });
        let status = cx.new(|_| StatusView { status: "".into(), meta: palette.meta });

        let mut demo = Demo {
            focus,
            _subscriptions: vec![activation],
            clock,
            large,
            small,
            pill,
            status,
            mode,
            tracker: ActivityTracker::new(),
            script: script(),
            agent: AgentTurn::new(),
            turn_t: 0.0,
            step: 0,
            activity,
            wall_t: 0.0,
            step_started: 0.0,
            last_tick: Instant::now(),
            os_polled: Instant::now(),
            motion,
            policy: FramePolicy {
                focused: window.is_window_active(),
                visible: platform::window_visible(window),
                ..FramePolicy::default()
            },
            paused: false,
            theme,
        };
        demo.restart(cx);
        demo
    }

    /// Advances the clocks; returns how long to wait before the next tick.
    fn tick(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Duration {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        if now.duration_since(self.os_polled) >= OS_POLL {
            self.os_polled = now;
            if self.motion.set_os(platform::reduce_motion()) {
                self.apply_motion(cx);
            }
        }
        self.policy.visible = platform::window_visible(window);
        if !self.policy.focused || !self.policy.visible {
            self.sync_views(cx);
            return IDLE_POLL;
        }
        self.wall_t += dt;

        let policy = self.policy;
        self.clock.update(cx, |c, cx| {
            let fps = policy.fps(&c.orb, c.anim_t);
            c.fps = fps;
            if fps > 0 {
                c.anim_t += dt;
                c.frame += 1;
                c.resample();
                cx.notify();
            } else if c.animating {
                // The last frame: draw it in views that skipped frames.
                c.change += 1;
                cx.notify();
            }
            c.animating = fps > 0;
        });

        if !self.paused {
            match self.mode {
                Mode::Cycle => {
                    if self.wall_t - self.step_started >= STEP_SECS {
                        self.go_to((self.step + 1) % self.script.len(), cx);
                    }
                }
                Mode::Agent => {
                    self.turn_t += dt;
                    if self.turn_t >= self.agent.end() {
                        self.turn_t = 0.0;
                    }
                    let i = self.agent.index_at(self.turn_t);
                    if i != self.step {
                        self.show_agent(i, cx);
                    }
                }
            }
        }
        self.sync_views(cx);
        let c = self.clock.read(cx);
        self.policy.interval(&c.orb, c.anim_t).unwrap_or(IDLE_POLL)
    }

    /// Starts the current mode from its first state.
    fn restart(&mut self, cx: &mut Context<Self>) {
        match self.mode {
            Mode::Cycle => self.go_to(0, cx),
            Mode::Agent => {
                self.turn_t = 0.0;
                self.show_agent(0, cx);
            }
        }
        self.sync_views(cx);
    }

    fn go_to(&mut self, step: usize, cx: &mut Context<Self>) {
        if step == 0 {
            self.tracker = ActivityTracker::new();
        }
        let activity = match &self.script[step] {
            Step::Event(event) => {
                self.tracker.apply(event);
                self.tracker.current().cloned()
            }
            Step::Direct(activity) => Some(activity.clone()),
        };
        self.show(step, activity, cx);
    }

    fn show_agent(&mut self, i: usize, cx: &mut Context<Self>) {
        let activity = self.agent.timeline[i].1.clone();
        self.show(i, activity, cx);
    }

    /// Shows `activity` (`None`: the pill went idle, so keep the last state).
    fn show(&mut self, step: usize, activity: Option<Activity>, cx: &mut Context<Self>) {
        self.step = step;
        if let Some(activity) = activity {
            self.clock.update(cx, |c, cx| {
                if c.orb.set_state(activity.state, c.anim_t) {
                    c.changed(cx);
                }
            });
            self.activity = activity;
        }
        self.step_started = self.wall_t;
    }

    fn apply_motion(&mut self, cx: &mut Context<Self>) {
        let on = self.motion.effective();
        self.clock.update(cx, |c, cx| {
            c.orb.set_reduced_motion(on);
            c.changed(cx);
        });
    }

    /// Pushes the pill text and status into their views, notifying only the ones that changed.
    fn sync_views(&mut self, cx: &mut Context<Self>) {
        let palette = Palette::for_theme(self.theme);
        let step: SharedString = match self.mode {
            Mode::Cycle => format!("· {}/{}", self.step + 1, self.script.len()).into(),
            Mode::Agent => format!("· {}", self.agent.event_at(self.turn_t)).into(),
        };
        let content = PillContent {
            activity: self.activity.clone(),
            secs: (self.wall_t - self.step_started) as u64,
            step,
            palette,
        };
        self.pill.update(cx, |p, cx| {
            if p.0 != content {
                p.0 = content;
                cx.notify();
            }
        });

        let c = self.clock.read(cx);
        let (fps, state_size) = (self.policy.fps(&c.orb, c.anim_t), c.opts.apply_state_size);
        let motion = match (self.motion.effective(), self.motion.manual.is_some()) {
            (on, true) => format!("{} (M override)", if on { "on" } else { "off" }),
            (on, false) => format!("{} (OS)", if on { "on" } else { "off" }),
        };
        let status: SharedString = format!(
            "{} · {} · {} fps · reduced motion {} · {} · state size {}{}{}",
            match self.mode { Mode::Cycle => "cycle", Mode::Agent => "agent turn" },
            match self.theme { Theme::Dark => "dark", Theme::Light => "light" },
            fps,
            motion,
            if self.policy.low_power { "low power" } else { "mains" },
            if state_size { "on (v7.3)" } else { "off (v7.2 bug)" },
            if self.paused { " · paused" } else { "" },
            if !self.policy.visible { " · occluded" } else if !self.policy.focused { " · unfocused" } else { "" },
        )
        .into();
        self.status.update(cx, |s, cx| {
            if s.status != status || s.meta != palette.meta {
                s.status = status;
                s.meta = palette.meta;
                cx.notify();
            }
        });
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        match (key, self.mode) {
            ("m", _) => {
                self.motion.toggle();
                self.apply_motion(cx);
            }
            ("space", _) => self.paused = !self.paused,
            ("a", _) => {
                self.mode = match self.mode {
                    Mode::Cycle => Mode::Agent,
                    Mode::Agent => Mode::Cycle,
                };
                self.restart(cx);
            }
            ("right", Mode::Cycle) => self.go_to((self.step + 1) % self.script.len(), cx),
            // Replays the script from the start up to the previous step, so labels stay right.
            ("left", Mode::Cycle) => {
                let n = self.script.len();
                let target = (self.step + n - 1) % n;
                for k in 0..=target {
                    self.go_to(k, cx);
                }
            }
            ("right" | "left", Mode::Agent) => {
                let n = self.agent.timeline.len();
                let i = if key == "right" { (self.step + 1) % n } else { (self.step + n - 1) % n };
                self.turn_t = self.agent.timeline[i].0;
                self.show_agent(i, cx);
            }
            ("t", _) => {
                self.theme = match self.theme {
                    Theme::Dark => Theme::Light,
                    Theme::Light => Theme::Dark,
                };
                let palette = Palette::for_theme(self.theme);
                self.clock.update(cx, |c, cx| {
                    c.palette = palette;
                    c.changed(cx);
                });
                // The page background.
                cx.notify();
            }
            ("b", _) => self.policy.low_power = !self.policy.low_power,
            ("s", _) => self.clock.update(cx, |c, cx| {
                c.opts.apply_state_size = !c.opts.apply_state_size;
                c.changed(cx);
            }),
            _ => return,
        }
        self.sync_views(cx);
    }
}

fn color(hex: u32, alpha: f32) -> Rgba {
    Rgba { a: alpha, ..rgb(hex) }
}

/// An orb as a fixed-size canvas: one quad per dot (AA disc, or a pixel-snapped square).
fn orb_canvas(dots: Vec<DrawDot>, size_px: f32, palette: Palette) -> impl IntoElement {
    canvas(
        |_, _, _| {},
        move |bounds: Bounds<Pixels>, (), window, _| {
            let scale = window.scale_factor();
            // Snap the orb's origin to the device grid so the squares land on whole pixels.
            let snap = |v: Pixels| (f32::from(v) * scale).round() / scale;
            let (ox, oy) = (snap(bounds.origin.x), snap(bounds.origin.y));
            for d in &dots {
                let c = color(palette.dot(d.tone), d.alpha);
                if d.pixel_square {
                    let (x, y, side) = d.square(scale);
                    let b = Bounds::new(point(px(ox + x), px(oy + y)), size(px(side), px(side)));
                    window.paint_quad(fill(b, c));
                } else {
                    let r = d.radius;
                    let b = Bounds::new(point(px(ox + d.x - r), px(oy + d.y - r)), size(px(2.0 * r), px(2.0 * r)));
                    window.paint_quad(fill(b, c).corner_radii(px(r)));
                }
            }
        },
    )
    .w(px(size_px))
    .h(px(size_px))
    .flex_none()
}

/// A cached view's box (its size can't come from its content).
fn cached(view: impl Into<AnyView>, style: StyleRefinement) -> AnyView {
    view.into().cached(style)
}

impl Render for Demo {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Only layout and cached views here: an orb frame re-runs this (the root is the orb
        // views' ancestor) but reuses the pill text and status from the previous frame.
        let page_bg = match self.theme {
            Theme::Dark => 0x111113,
            Theme::Light => 0xF7F7F8,
        };
        let orb_box = |side: f32| StyleRefinement::default().w(px(side)).h(px(side)).flex_none();
        div()
            .id("root")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_6()
            .bg(rgb(page_bg))
            .text_sm()
            .child(cached(self.large.clone(), orb_box(ORB_LG_PX)))
            .child(
                div()
                    .relative()
                    .w(px(PILL_SLOT_W))
                    .h(px(PILL_HEIGHT_DOCK_PX))
                    .child(cached(self.pill.clone(), StyleRefinement::default().size_full()))
                    .child(cached(
                        self.small.clone(),
                        orb_box(ORB_PX).absolute().left(px(PILL_ORB_X)).top(px((PILL_HEIGHT_DOCK_PX - ORB_PX) / 2.0)),
                    )),
            )
            .child(cached(self.status.clone(), StyleRefinement::default().w_full().h(px(36.0))))
    }
}

/// What the OS says about motion and the window. macOS only; elsewhere, full motion and always
/// visible.
mod platform {
    #[cfg(target_os = "macos")]
    pub use mac::{reduce_motion, window_visible};

    #[cfg(not(target_os = "macos"))]
    pub fn reduce_motion() -> bool {
        false
    }

    #[cfg(not(target_os = "macos"))]
    pub fn window_visible(_: &gpui::Window) -> bool {
        true
    }

    #[cfg(target_os = "macos")]
    mod mac {
        use objc2::runtime::AnyObject;
        use objc2::{class, msg_send};
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};

        /// `NSWindowOcclusionStateVisible`.
        const OCCLUSION_VISIBLE: usize = 1 << 1;

        /// `NSWorkspace.sharedWorkspace.accessibilityDisplayShouldReduceMotion`.
        pub fn reduce_motion() -> bool {
            // SAFETY: both are documented NSWorkspace class/instance methods with these
            // signatures, sent on the main thread; AppKit is linked by GPUI.
            unsafe {
                let workspace: *mut AnyObject = msg_send![class!(NSWorkspace), sharedWorkspace];
                !workspace.is_null() && msg_send![workspace, accessibilityDisplayShouldReduceMotion]
            }
        }

        /// Whether any part of the window is on screen (`NSWindow.occlusionState`): false when
        /// it's minimised, on another Space, or fully covered. True if the window can't be read.
        pub fn window_visible(window: &gpui::Window) -> bool {
            // (`Window` also has an inherent `window_handle()`, GPUI's own handle.)
            let Ok(handle) = HasWindowHandle::window_handle(window) else {
                return true;
            };
            let RawWindowHandle::AppKit(appkit) = handle.as_raw() else {
                return true;
            };
            // SAFETY: GPUI's AppKit handle is a live NSView for as long as `window` is borrowed;
            // `-[NSView window]` and `-[NSWindow occlusionState]` take no arguments.
            unsafe {
                let view = appkit.ns_view.as_ptr().cast::<AnyObject>();
                let ns_window: *mut AnyObject = msg_send![view, window];
                if ns_window.is_null() {
                    return true;
                }
                let state: usize = msg_send![ns_window, occlusionState];
                state & OCCLUSION_VISIBLE != 0
            }
        }
    }
}

fn main() {
    let mode = if std::env::args().any(|a| a == "--agent") { Mode::Agent } else { Mode::Cycle };
    Application::new().run(move |cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.), px(360.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |window, cx| cx.new(|cx| Demo::new(mode, window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
