//! A 22px activity orb: the v7.5 dotted globe from `rozu-activity`, redrawn at 30fps while it
//! moves. Every status in the app is one of these.

use std::sync::OnceLock;
use std::time::{Duration, Instant};

use gpui::{
    Bounds, Context, Pixels, Rgba, Task, Window, canvas, fill, point, prelude::*, px, size,
};
use rozu_activity::tokens::{ORB_PX, Palette, Theme};
use rozu_activity::{ActivityState, Orb, OrbSize, ProjectOptions, project_with};

const FRAME: Duration = Duration::from_millis(33);

/// Seconds since the app started: the clock every orb runs on.
fn now() -> f64 {
    static START: OnceLock<Instant> = OnceLock::new();
    START.get_or_init(Instant::now).elapsed().as_secs_f64()
}

pub struct OrbView {
    orb: Orb,
    _tick: Task<()>,
}

impl OrbView {
    pub fn new(state: ActivityState, cx: &mut Context<Self>) -> Self {
        let tick = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(FRAME).await;
                let alive = this.update(cx, |o: &mut OrbView, cx| {
                    if o.orb.is_animating(now()) {
                        cx.notify();
                    }
                });
                if alive.is_err() {
                    break;
                }
            }
        });
        Self {
            orb: Orb::with_size(OrbSize::Small, state, now()),
            _tick: tick,
        }
    }

    pub fn set(&mut self, state: ActivityState, cx: &mut Context<Self>) {
        if self.orb.set_state(state, now()) {
            cx.notify();
        }
    }
}

impl Render for OrbView {
    fn render(&mut self, window: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let samples = self.orb.sample(now());
        let dots = project_with(
            &samples,
            ORB_PX,
            window.scale_factor(),
            ProjectOptions::default(),
        );
        let palette = Palette::for_theme(Theme::Dark);
        canvas(
            |_, _, _| {},
            move |bounds: Bounds<Pixels>, (), window, _| {
                let scale = window.scale_factor();
                let snap = |v: Pixels| (f32::from(v) * scale).round() / scale;
                let (ox, oy) = (snap(bounds.origin.x), snap(bounds.origin.y));
                for d in &dots {
                    let c = palette.dot(d.tone);
                    let color = Rgba {
                        r: ((c >> 16) & 0xFF) as f32 / 255.0,
                        g: ((c >> 8) & 0xFF) as f32 / 255.0,
                        b: (c & 0xFF) as f32 / 255.0,
                        a: d.alpha,
                    };
                    let (x, y, side, r) = if d.pixel_square {
                        let (x, y, side) = d.square(scale);
                        (x, y, side, 0.0)
                    } else {
                        (d.x - d.radius, d.y - d.radius, 2.0 * d.radius, d.radius)
                    };
                    let b = Bounds::new(point(px(ox + x), px(oy + y)), size(px(side), px(side)));
                    window.paint_quad(fill(b, color).corner_radii(px(r)));
                }
            },
        )
        .size(px(ORB_PX))
        .flex_none()
    }
}
