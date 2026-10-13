//! v7.5 activity pill demo: the dotted-globe orb (22px in the pill, 88px large), the label and a mono
//! timer, cycling all ten states. Labels come from a scripted agent turn run through
//! `rozu_activity::ActivityTracker`.
//!
//! Keys: M reduced motion · Space pause cycle · ←/→ previous/next state · T theme ·
//! B low power (30fps) · S state size (on by default since v7.3; off shows the v7.2 bug, where
//! every dot is drawn at size 1). Rendering pauses (0fps) when the window
//! loses focus and after Done/Error settle; the animation clock stops with it, so it resumes
//! with no jump.
use std::time::{Duration, Instant};

use gpui::{
    App, Application, Bounds, Context, FocusHandle, FontWeight, KeyDownEvent, Pixels, Rgba,
    SharedString, Subscription, Window, WindowBounds, WindowOptions, canvas, div, fill, point,
    prelude::*, px, rgb, size,
};
use rozu_activity::tokens::{ORB_LG_PX, ORB_PX, PILL_HEIGHT_DOCK_PX, Palette, Theme};
use rozu_activity::{
    Activity, ActivityEvent, ActivityState, ActivityTracker, DrawDot, FramePolicy, Orb, OrbSize,
    ProjectOptions, project_with,
};

/// Seconds each state stays up before the cycle moves on.
const STEP_SECS: f64 = 3.2;
/// Poll interval while no frames are needed (the cycle timer and the pill's clock still run).
const IDLE_POLL: Duration = Duration::from_millis(100);

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

struct Demo {
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
    orb: Orb,
    /// The 22px pill's own point set, kept on the same timeline as `orb`.
    orb_small: Orb,
    tracker: ActivityTracker,
    script: Vec<Step>,
    step: usize,
    activity: Activity,
    /// The orb's animation clock: advances only while frames are drawn.
    anim_t: f64,
    /// Wall-clock seconds while focused: drives the cycle and the pill timer.
    wall_t: f64,
    step_started: f64,
    last_tick: Instant,
    shown_secs: u64,
    policy: FramePolicy,
    paused: bool,
    theme: Theme,
    opts: ProjectOptions,
}

impl Demo {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus);
        let activation = cx.observe_window_activation(window, |this: &mut Demo, window, cx| {
            this.policy.focused = window.is_window_active();
            // Don't count the time spent in the background.
            this.last_tick = Instant::now();
            cx.notify();
        });
        cx.spawn(async move |this, cx| {
            loop {
                let Ok(wait) = this.update(cx, |demo: &mut Demo, cx| demo.tick(cx)) else {
                    break;
                };
                cx.background_executor().timer(wait).await;
            }
        })
        .detach();

        let mut demo = Demo {
            focus,
            _subscriptions: vec![activation],
            orb: Orb::new(ActivityState::Thinking, 0.0),
            orb_small: Orb::with_size(OrbSize::Small, ActivityState::Thinking, 0.0),
            tracker: ActivityTracker::new(),
            script: script(),
            step: 0,
            activity: Activity::new(ActivityState::Thinking, ActivityState::Thinking.label()),
            anim_t: 0.0,
            wall_t: 0.0,
            step_started: 0.0,
            last_tick: Instant::now(),
            shown_secs: 0,
            policy: FramePolicy { focused: window.is_window_active(), ..FramePolicy::default() },
            paused: false,
            theme: Theme::Dark,
            opts: ProjectOptions::default(),
        };
        demo.go_to(0);
        demo
    }

    /// Advances the clocks; returns how long to wait before the next tick.
    fn tick(&mut self, cx: &mut Context<Self>) -> Duration {
        let now = Instant::now();
        let dt = now.duration_since(self.last_tick).as_secs_f64().min(0.1);
        self.last_tick = now;
        if !self.policy.focused {
            return IDLE_POLL;
        }
        self.wall_t += dt;
        let animating = self.policy.fps(&self.orb, self.anim_t) > 0;
        if animating {
            self.anim_t += dt;
        }
        if !self.paused && self.wall_t - self.step_started >= STEP_SECS {
            self.go_to((self.step + 1) % self.script.len());
        }
        let secs = (self.wall_t - self.step_started) as u64;
        if animating || secs != self.shown_secs {
            self.shown_secs = secs;
            cx.notify();
        }
        self.policy.interval(&self.orb, self.anim_t).unwrap_or(IDLE_POLL)
    }

    fn go_to(&mut self, step: usize) {
        if step == 0 {
            self.tracker = ActivityTracker::new();
        }
        self.step = step;
        let activity = match &self.script[step] {
            Step::Event(event) => {
                self.tracker.apply(event);
                self.tracker.current().cloned()
            }
            Step::Direct(activity) => Some(activity.clone()),
        };
        if let Some(activity) = activity {
            self.orb.set_state(activity.state, self.anim_t);
            self.orb_small.set_state(activity.state, self.anim_t);
            self.activity = activity;
        }
        self.step_started = self.wall_t;
        self.shown_secs = 0;
    }

    fn on_key(&mut self, event: &KeyDownEvent, _window: &mut Window, cx: &mut Context<Self>) {
        let n = self.script.len();
        match event.keystroke.key.as_str() {
            "m" => {
                let on = !self.orb.reduced_motion();
                self.orb.set_reduced_motion(on);
                self.orb_small.set_reduced_motion(on);
            }
            "space" => self.paused = !self.paused,
            "right" => self.go_to((self.step + 1) % n),
            // Replays the script from the start up to the previous step, so labels stay right.
            "left" => {
                let target = (self.step + n - 1) % n;
                for k in 0..=target {
                    self.go_to(k);
                }
            }
            "t" => {
                self.theme = match self.theme {
                    Theme::Dark => Theme::Light,
                    Theme::Light => Theme::Dark,
                }
            }
            "b" => self.policy.low_power = !self.policy.low_power,
            "s" => self.opts.apply_state_size = !self.opts.apply_state_size,
            _ => return,
        }
        cx.notify();
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

impl Render for Demo {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let palette = Palette::for_theme(self.theme);
        let scale = window.scale_factor();
        let small = project_with(&self.orb_small.sample(self.anim_t), ORB_PX, scale, self.opts);
        let large = project_with(&self.orb.sample(self.anim_t), ORB_LG_PX, scale, self.opts);

        let a = &self.activity;
        let tone = a.state.tone();
        let elapsed = self.shown_secs;
        let timer: SharedString = format!("{}:{:02}", elapsed / 60, elapsed % 60).into();
        let step: SharedString = format!("· {}/{}", self.step + 1, self.script.len()).into();
        let meta = |text: SharedString| div().text_color(rgb(palette.meta)).font_family("Menlo").child(text);

        let fps = self.policy.fps(&self.orb, self.anim_t);
        let status: SharedString = format!(
            "{} · {} fps · reduced motion {} · {} · state size {}{}",
            match self.theme { Theme::Dark => "dark", Theme::Light => "light" },
            fps,
            if self.orb.reduced_motion() { "on" } else { "off" },
            if self.policy.low_power { "low power" } else { "mains" },
            if self.opts.apply_state_size { "on" } else { "off (v7.2 bug)" },
            if self.paused { " · cycle paused" } else { "" },
        )
        .into();
        let (page_bg, page_fg) = match self.theme {
            Theme::Dark => (0x111113, 0xF4F1EA),
            Theme::Light => (0xF7F7F8, 0x25242A),
        };

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
            .text_color(rgb(page_fg))
            .text_sm()
            .child(orb_canvas(large, ORB_LG_PX, palette))
            .child(
                // The pill: [orb 22px] [label] [mono timer] [· step] [detail] [hint].
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
                    .child(orb_canvas(small, ORB_PX, palette))
                    .child(
                        div()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(rgb(palette.label(tone)))
                            .child(SharedString::from(a.label.clone())),
                    )
                    .child(meta(timer))
                    .child(meta(step))
                    .children(a.detail.clone().map(|d| meta(d.into())))
                    .children(a.hint.map(|h| meta(h.into()))),
            )
            .child(div().text_xs().text_color(rgb(palette.meta)).child(status))
            .child(
                div()
                    .text_xs()
                    .text_color(rgb(palette.meta))
                    .child("M reduced motion · Space pause cycle · ←/→ state · T theme · B low power · S state size"),
            )
    }
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(640.), px(360.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |window, cx| cx.new(|cx| Demo::new(window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}
