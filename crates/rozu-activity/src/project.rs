//! Projection to a draw list: perspective, depth size/α, the top-left light, the lit boost, the
//! 22px subsample and the depth sort (`drawOrb` in the reference, minus the canvas calls).

use crate::generate::CAMERA;
use crate::orb::Sample;
use crate::state::Tone;

/// Most dots in one orb's draw list.
pub const MAX_POINTS: usize = 120;
/// Dots drawn at the 22px optical size (any orb ≤ 30px).
pub const POINTS_22: usize = 60;
/// Orbs at or under this size (logical px) use the 22px optical size.
const SMALL_PX: f64 = 30.0;
/// At the small size, dots under this many device px are drawn as pixel-snapped squares.
const SQUARE_DEVICE_PX: f64 = 1.6;
/// The light direction (top-left, toward the viewer); not normalised, as in the reference.
const LIGHT: [f64; 3] = [-0.55, 0.6, 0.58];

/// One dot to draw, in logical px from the orb's top-left corner. Back to front.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DrawDot {
    pub x: f32,
    pub y: f32,
    pub radius: f32,
    /// Final opacity, 0..1.
    pub alpha: f32,
    pub tone: Tone,
    /// Draw as a pixel-snapped square ([`DrawDot::square`]) instead of an AA disc.
    pub pixel_square: bool,
}

impl DrawDot {
    /// The pixel-snapped square for a [`DrawDot::pixel_square`] dot: `(x, y, side)` in logical
    /// px, on the device-pixel grid for `scale`.
    pub fn square(&self, scale: f32) -> (f32, f32, f32) {
        let px = 1.0 / scale;
        let snap = |v: f32| js_round(v * scale) / scale;
        let side = px.max(js_round(2.0 * self.radius * scale) / scale);
        (snap(self.x - self.radius), snap(self.y - self.radius), side)
    }
}

/// JS `Math.round` (halves round up), so snapping matches the reference.
fn js_round(v: f32) -> f32 {
    (v + 0.5).floor()
}

/// Projection options.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectOptions {
    /// Multiply each dot's radius by its state size multiplier `s` (`base · s · k · …`), and
    /// skip dots with `s ≤ .02`. On by default, as in the v7.3 reference `drawOrb`; this is what
    /// draws the 1.3× cue dots, Editing's empty wedge and Offline's sparse cloud. Off reproduces
    /// the v7.2 reference bug, where the perspective factor shadowed `s` (`{...b, s: F/(F-z)}`).
    pub apply_state_size: bool,
}

impl Default for ProjectOptions {
    fn default() -> Self {
        ProjectOptions {
            apply_state_size: true,
        }
    }
}

/// [`project_with`] with the default (v7.3 reference) options.
pub fn project(points: &[Sample], size_px: f32, scale: f32) -> Vec<DrawDot> {
    project_with(points, size_px, scale, ProjectOptions::default())
}

/// Projects `points` (an [`crate::Orb::sample`]) into a draw list for an orb `size_px` logical px
/// wide at `scale` device px per logical px. At most [`MAX_POINTS`] dots, [`POINTS_22`] for
/// orbs ≤ 30px, sorted far to near.
pub fn project_with(
    points: &[Sample],
    size_px: f32,
    scale: f32,
    opts: ProjectOptions,
) -> Vec<DrawDot> {
    let size = f64::from(size_px);
    let dpr = f64::from(scale.max(0.01));
    let small = size <= SMALL_PX;
    let r_orb = size * 0.4;
    let base = if small { size * 0.05 } else { size * 0.017 };
    let px = 1.0 / dpr;

    struct Projected {
        dot: DrawDot,
        z: f64,
    }
    let mut out: Vec<Projected> = points
        .iter()
        .take(MAX_POINTS)
        .enumerate()
        // Keeps both parities, so alternating-bar states keep both bars.
        .filter(|(i, _)| !small || ((i >> 1) + i) % 2 == 0)
        .filter_map(|(_, b)| {
            let z = b.p[2];
            let k = CAMERA / (CAMERA - z);
            let s = if opts.apply_state_size { b.s } else { 1.0 };
            if s <= 0.02 || k <= 0.02 {
                return None;
            }
            let depth = ((z + 1.2) / 2.4).clamp(0.0, 1.0);
            let len = {
                let l = (b.p[0] * b.p[0] + b.p[1] * b.p[1] + b.p[2] * b.p[2]).sqrt();
                if l == 0.0 { 1.0 } else { l }
            };
            let lam = (LIGHT[0] * b.p[0] + LIGHT[1] * b.p[1] + LIGHT[2] * b.p[2]) / len * 0.5 + 0.5;
            let r = (base * s * k * (0.4 + 0.6 * depth) * (0.9 + 0.2 * lam) * (1.0 + 0.55 * b.lit))
                .max(px * 0.75);
            let mut a = (0.12 + 0.88 * depth) * (0.62 + 0.48 * lam);
            a = (a + 0.5 * b.lit).clamp(0.0, 1.0) * b.a;
            if b.dim {
                a *= 0.85;
            }
            Some(Projected {
                dot: DrawDot {
                    x: (size / 2.0 + b.p[0] * k * r_orb) as f32,
                    y: (size / 2.0 - b.p[1] * k * r_orb) as f32,
                    radius: r as f32,
                    alpha: a.clamp(0.0, 1.0) as f32,
                    tone: b.tone,
                    pixel_square: small && r * dpr < SQUARE_DEVICE_PX,
                },
                z,
            })
        })
        .collect();
    // Stable, like the reference's Array.sort, so ties keep index order.
    out.sort_by(|a, b| a.z.total_cmp(&b.z));
    out.into_iter().map(|p| p.dot).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::orb::Orb;
    use crate::state::ActivityState;

    #[test]
    fn subsample_22px_gives_60() {
        let picked = (0..MAX_POINTS).filter(|i| ((i >> 1) + i) % 2 == 0).count();
        assert_eq!(picked, POINTS_22);
        let pts = Orb::new(ActivityState::Thinking, 0.0).sample(0.5);
        assert_eq!(project(&pts, 22.0, 2.0).len(), POINTS_22);
        assert_eq!(project(&pts, 30.0, 1.0).len(), POINTS_22);
        assert_eq!(project(&pts, 88.0, 2.0).len(), MAX_POINTS);
    }

    #[test]
    fn at_most_120_points() {
        for state in ActivityState::ALL {
            let mut orb = Orb::new(ActivityState::Running, 0.0);
            orb.set_state(state, 0.5);
            for k in 0..30 {
                let pts = orb.sample(0.5 + k as f64 * 0.05);
                let mut doubled = pts.clone();
                doubled.extend_from_slice(&pts);
                assert!(project(&doubled, 88.0, 2.0).len() <= MAX_POINTS);
            }
        }
    }

    #[test]
    fn sorted_far_to_near_and_in_bounds() {
        for state in ActivityState::ALL {
            let pts = Orb::new(state, 0.0).sample(0.8);
            let dots = project(&pts, 88.0, 2.0);
            // Dots with a state size of 0 (Editing's wedge, Offline's hidden cloud) are skipped.
            let shown: Vec<_> = pts.iter().filter(|p| p.s > 0.02).collect();
            assert_eq!(dots.len(), shown.len());
            // The nearest drawn sample is drawn last.
            let near = shown
                .iter()
                .max_by(|a, b| a.p[2].total_cmp(&b.p[2]))
                .unwrap();
            let k = CAMERA / (CAMERA - near.p[2]);
            assert!((dots.last().unwrap().x - (44.0 + near.p[0] * k * 35.2) as f32).abs() < 1e-3);
            for d in &dots {
                assert!(d.alpha >= 0.0 && d.alpha <= 1.0, "{state:?}: {d:?}");
                assert!(d.radius > 0.0 && d.x.is_finite() && d.y.is_finite());
                // Error's scatter overshoots the sphere (as in the reference renders).
                assert!(
                    d.x > -30.0 && d.x < 118.0 && d.y > -30.0 && d.y < 118.0,
                    "{state:?}: {d:?}"
                );
                assert!(!d.pixel_square, "only 22px orbs snap");
            }
        }
    }

    #[test]
    fn small_dots_become_pixel_squares() {
        let pts = Orb::new(ActivityState::Offline, 0.0).sample(0.75);
        let dots = project(&pts, 22.0, 1.0);
        assert!(dots.iter().any(|d| d.pixel_square));
        for d in dots.iter().filter(|d| d.pixel_square) {
            assert!(d.radius < 1.6);
            let (x, y, side) = d.square(1.0);
            assert_eq!((x.fract(), y.fract()), (0.0, 0.0));
            assert!(side >= 1.0 && side.fract() == 0.0);
        }
        let big = project(&pts, 22.0, 2.0);
        assert!(
            big.iter().filter(|d| d.pixel_square).count()
                <= dots.iter().filter(|d| d.pixel_square).count()
        );
    }

    #[test]
    fn light_and_depth_brighten_the_front() {
        let mut near = Orb::new(ActivityState::Done, 0.0).sample(5.0)[0];
        near.lit = 0.0;
        near.p = [0.0, 0.0, 1.0];
        let mut far = near;
        far.p = [0.0, 0.0, -1.0];
        let d = project(&[near, far], 88.0, 1.0);
        let (back, front) = (d[0], d[1]);
        assert!(front.radius > 2.0 * back.radius);
        assert!(front.alpha > 3.0 * back.alpha);
    }

    #[test]
    fn state_size_is_applied_by_default() {
        assert!(ProjectOptions::default().apply_state_size);
        // Editing's wedge (s = 0) is empty by default; the v7.2 legacy mode draws every dot.
        let pts = Orb::new(ActivityState::Editing, 0.0).sample(0.06);
        let shown = project(&pts, 88.0, 2.0).len();
        let legacy = ProjectOptions {
            apply_state_size: false,
        };
        assert_eq!(project_with(&pts, 88.0, 2.0, legacy).len(), MAX_POINTS);
        assert!(shown < MAX_POINTS, "the wedge's dots have s = 0");
    }

    #[test]
    fn state_size_scales_the_radius() {
        // radius = base · s · k · …: the same dot at s = 1.3 is 1.3× the s = 1 dot.
        let mut a = Orb::new(ActivityState::Done, 0.0).sample(5.0)[0];
        a.lit = 0.0;
        a.s = 1.0;
        let mut b = a;
        b.s = 1.3;
        let (ra, rb) = (
            project(&[a], 88.0, 2.0)[0].radius,
            project(&[b], 88.0, 2.0)[0].radius,
        );
        assert!((rb / ra - 1.3).abs() < 1e-5, "{ra} {rb}");
        b.s = 0.02;
        assert!(project(&[b], 88.0, 2.0).is_empty());
    }
}
