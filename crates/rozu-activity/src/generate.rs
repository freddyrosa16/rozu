//! Per-state point generators, ported 1:1 from `docs/design/v7.5/reference/v7.5-orb.js`.
//!
//! Every state is the same object: a globe of small matte dots on lines of latitude
//! ([`OrbSize::grid`]), and the state is what the globe is doing. Done and Error gather the dots
//! into a check or a cross, with the rest resting on a faint ring. There are two sizes, tuned
//! separately rather than scaled: [`OrbSize::Large`] (15 rings) and [`OrbSize::Small`] (the 22px
//! pill, 9 rings). The arithmetic (including JS's truncating `%`) is kept as in the reference so
//! samples match it at the same `t`.

use std::f64::consts::{PI, TAU};

use crate::state::ActivityState;

/// A point in orb space (unit-radius sphere, +y up, +z toward the camera).
pub type Vec3 = [f64; 3];

/// Camera distance: perspective `k = F / (F − z)`.
pub const CAMERA: f64 = 3.2;

/// The globe's radius.
pub const R: f64 = 0.9;
/// The globe's tilt toward the camera (rad).
const TILT: f64 = 0.3;
/// The plane Done's and Error's flat symbols sit on (toward the camera).
pub const GLYPH_Z: f64 = 0.3;

/// Which tuned point set to draw: each size is its own dot grid, not a subsample.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OrbSize {
    /// Orbs over 30px: 15 lines of latitude, up to 30 dots round the equator (284 dots).
    Large,
    /// The 22px pill: 9 lines of latitude, up to 15 dots round the equator (86 dots).
    Small,
}

impl OrbSize {
    pub const ALL: [OrbSize; 2] = [OrbSize::Large, OrbSize::Small];

    /// The size for an orb `px` logical px wide.
    pub fn for_px(px: f64) -> Self {
        if px <= 30.0 {
            OrbSize::Small
        } else {
            OrbSize::Large
        }
    }

    /// Lines of latitude.
    pub const fn rings(self) -> usize {
        match self {
            OrbSize::Large => 15,
            OrbSize::Small => 9,
        }
    }

    /// Dots round the equator; rings nearer the poles get proportionally fewer (at least 3).
    pub const fn lon(self) -> usize {
        match self {
            OrbSize::Large => 30,
            OrbSize::Small => 15,
        }
    }

    /// Dot size multiplier (`P.dot`).
    pub const fn dot(self) -> f64 {
        match self {
            OrbSize::Large => 1.0,
            OrbSize::Small => 1.15,
        }
    }

    /// Spin speed multiplier (`P.speed`).
    pub const fn speed(self) -> f64 {
        match self {
            OrbSize::Large => 1.0,
            OrbSize::Small => 1.15,
        }
    }

    /// Dots in ring `j`.
    fn ring_len(self, j: usize) -> usize {
        let phi = -PI / 2.0 + PI * (j as f64 + 0.5) / self.rings() as f64;
        ((self.lon() as f64 * phi.cos()).round() as usize).max(3)
    }

    /// Total dots (`P.N`).
    pub fn dots(self) -> usize {
        (0..self.rings()).map(|j| self.ring_len(j)).sum()
    }

    /// Dot `i`'s `(latitude, longitude, ring)`: `P.grid[i]`.
    pub fn grid(self, i: usize) -> (f64, f64, usize) {
        let mut rest = i;
        for j in 0..self.rings() {
            let n = self.ring_len(j);
            if rest < n {
                let phi = -PI / 2.0 + PI * (j as f64 + 0.5) / self.rings() as f64;
                return (phi, TAU * rest as f64 / n as f64, j);
            }
            rest -= n;
        }
        panic!("dot {i} is past the {self:?} grid");
    }
}

/// Most dots in any size ([`OrbSize::Large`]).
pub const MAX_DOTS: usize = 284;

/// One generated dot: position, lit amount, state size multiplier and α multiplier.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Dot {
    pub p: Vec3,
    /// 0..1; lit dots are larger (×1.55) and brighter (α + .5).
    pub lit: f64,
    /// The state's size multiplier (`s` in the reference): the radius is `base · s · k · …`,
    /// and dots with `s ≤ .02` aren't drawn (see [`crate::ProjectOptions::apply_state_size`]).
    pub s: f64,
    /// Per-dot α multiplier (`a`; 1 when the reference leaves it unset).
    pub a: f64,
}

pub(crate) fn cl(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

pub(crate) fn rot_x(p: Vec3, a: f64) -> Vec3 {
    let (s, c) = a.sin_cos();
    [p[0], p[1] * c - p[2] * s, p[1] * s + p[2] * c]
}

pub(crate) fn rot_y(p: Vec3, a: f64) -> Vec3 {
    let (s, c) = a.sin_cos();
    [p[0] * c + p[2] * s, p[1], -p[0] * s + p[2] * c]
}

/// Deterministic per-dot hash in [0, 1).
pub fn hash(i: usize) -> f64 {
    let x = (i as f64 * 127.1 + 311.7).sin() * 43_758.545_3;
    x - x.floor()
}

/// Deterministic hash of two values in [0, 1).
pub fn hash2(a: f64, b: f64) -> f64 {
    let x = (a * 12.9898 + b * 78.233).sin() * 43_758.545_3;
    x - x.floor()
}

/// Smooth value noise on a 2D lattice.
pub fn vnoise(x: f64, y: f64) -> f64 {
    let (xi, yi) = (x.floor(), y.floor());
    let (mut fx, mut fy) = (x - xi, y - yi);
    fx = fx * fx * (3.0 - 2.0 * fx);
    fy = fy * fy * (3.0 - 2.0 * fy);
    let a = hash2(xi, yi);
    let b = hash2(xi + 1.0, yi);
    let c = hash2(xi, yi + 1.0);
    let d = hash2(xi + 1.0, yi + 1.0);
    a + (b - a) * fx + (c - a) * fy + (a - b - c + d) * fx * fy
}

fn dot(p: Vec3, lit: f64, s: f64, a: f64) -> Dot {
    Dot { p, lit, s, a }
}

type Pt = [f64; 2];

/// Dot `i`'s unit direction before the spin.
fn dir(size: OrbSize, i: usize) -> Vec3 {
    let (phi, lam, _) = size.grid(i);
    let c = phi.cos();
    [c * lam.cos(), phi.sin(), c * lam.sin()]
}

/// Dot `i` on the globe of radius `r`, spun by `yaw` radians and tilted toward the camera.
fn globe(size: OrbSize, i: usize, yaw: f64, r: f64) -> Vec3 {
    let d = dir(size, i);
    rot_x(rot_y([d[0] * r, d[1] * r, d[2] * r], yaw), TILT)
}

/// Where dot `i` faces the camera: 0 at the front, ±π at the back, after a spin of `yaw`.
fn front(size: OrbSize, i: usize, yaw: f64) -> f64 {
    let a = size.grid(i).1 - yaw - PI / 2.0;
    a.sin().atan2(a.cos())
}

/// The point at arc-length fraction `u` along a set of polylines, taken as one path.
fn along(strokes: &[&[Pt]], u: f64) -> Pt {
    let mut segs: Vec<(Pt, Pt, f64)> = Vec::new();
    let mut total = 0.0;
    for pts in strokes {
        for w in pts.windows(2) {
            let l = (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]);
            segs.push((w[0], w[1], l));
            total += l;
        }
    }
    let mut d = u * total;
    for (k, &(a, b, l)) in segs.iter().enumerate() {
        if d <= l || k == segs.len() - 1 {
            let f = if l != 0.0 { cl(d / l) } else { 0.0 };
            return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
        }
        d -= l;
    }
    strokes[0][0]
}

const CHECK: &[Pt] = &[[-0.46, 0.02], [-0.14, -0.32], [0.48, 0.34]];
const CROSS: &[&[Pt]] = &[
    &[[-0.36, 0.36], [0.36, -0.36]],
    &[[0.36, 0.36], [-0.36, -0.36]],
];

/// Done and Error: the first `share` of the dots draw the symbol (those with `u ≤ drawn` lit),
/// the rest rest on a faint dotted ring. `dx` shifts the whole frame, `pop` swells the symbol.
#[allow(clippy::too_many_arguments)]
fn symbol(
    size: OrbSize,
    i: usize,
    strokes: &[&[Pt]],
    share: f64,
    dx: f64,
    drawn: f64,
    pop: f64,
    lit_off: f64,
) -> Dot {
    let total = size.dots();
    let n = (total as f64 * share).round() as usize;
    if i < n {
        let u = i as f64 / (n - 1) as f64;
        let xy = along(strokes, u);
        let on = u <= drawn;
        return dot(
            [xy[0] + dx, xy[1], GLYPH_Z],
            if on { 1.0 } else { lit_off },
            (if on { 1.2 } else { 0.6 }) * (1.0 + 0.2 * pop) * size.dot(),
            if on { 1.0 } else { 0.35 },
        );
    }
    let a = PI / 2.0 - TAU * (i - n) as f64 / (total - n) as f64;
    dot(
        [R * a.cos() + dx, R * a.sin(), GLYPH_Z],
        0.0,
        0.7 * size.dot(),
        0.3,
    )
}

/// Dot `i` of `state` at `t` seconds into the state, for `size`.
pub fn generate(size: OrbSize, state: ActivityState, i: usize, t: f64) -> Dot {
    let k = size.dot();
    match state {
        // Thought weather: patches of light drift over a slowly turning globe.
        ActivityState::Thinking => {
            let (phi, lam, _) = size.grid(i);
            let p = globe(size, i, 0.5 * t * size.speed(), R);
            let n = vnoise(lam * 1.4 + 0.4 * t, phi * 1.3 + 0.2 * t + 7.0 + lam * 0.3);
            let lit = cl(n * 2.2 - 0.9);
            dot(p, lit * 0.7, (0.9 + 0.3 * lit) * k, 0.5 + 0.5 * lit)
        }
        // Three lines of text write themselves round the globe, hold, then start over.
        ActivityState::Notes => {
            let (_, lam, j) = size.grid(i);
            let p = globe(size, i, 0.0, R);
            let mid = (size.rings() - 1) as f64 / 2.0;
            let lines = [mid - 2.0, mid, mid + 2.0].map(|l| l.round() as usize);
            let Some(li) = lines.iter().position(|&l| l == j) else {
                return dot(p, 0.0, 0.75 * k, 0.3);
            };
            let ph = (t % 2.6) / 1.9;
            let w = cl(ph * 3.0 - li as f64);
            let u = ((lam + PI / 2.0) % TAU) / TAU;
            let on = u <= w;
            let head = on && w < 1.0 && u > w - 0.06;
            let s = if head {
                1.6
            } else if on {
                1.15
            } else {
                0.75
            };
            dot(
                p,
                if on { 1.0 } else { 0.0 },
                s * k,
                if on { 1.0 } else { 0.3 },
            )
        }
        // A lit band reads down the globe, line by line.
        ActivityState::Reading => {
            let (phi, _, _) = size.grid(i);
            let p = globe(size, i, 0.25 * t * size.speed(), R);
            let yb = PI / 2.0 - PI * ((t % 2.2) / 2.2) * 1.15 + 0.07;
            let on = (-((phi - yb) / 0.17).powi(2)).exp();
            dot(p, on, (0.8 + 0.5 * on) * k, 0.35 + 0.65 * on)
        }
        // A lens roams the surface; the dots under it swell.
        ActivityState::Searching => {
            let (phi, _, _) = size.grid(i);
            let yaw = 0.3 * t * size.speed();
            let p = globe(size, i, yaw, R);
            let a = TAU * t / 2.4;
            let cphi = 0.35 * (a * 0.7).sin();
            let clam = front(size, i, yaw) - 0.9 * a.sin();
            let d = (phi - cphi).hypot(clam * cphi.cos());
            let lit = cl((0.42 - d) / 0.12);
            dot(p, lit, (0.85 + 0.55 * lit) * k, 0.4 + 0.6 * lit)
        }
        // A caret moves along the equator, retyping the dots behind it.
        ActivityState::Editing => {
            let (_, _, j) = size.grid(i);
            let p = globe(size, i, 0.0, R);
            let eq = ((size.rings() - 1) as f64 / 2.0).round() as usize;
            if j != eq {
                return dot(p, 0.0, 0.8 * k, 0.4);
            }
            let ph = t % 2.4;
            let w = cl(ph / 1.6);
            let cx = -PI * 0.78 + 1.56 * PI * w;
            let f = front(size, i, 0.0);
            let blink = w >= 1.0 && ((ph - 1.6) / 0.2).floor() % 2.0 == 1.0;
            if (f - cx).abs() < PI / size.lon() as f64 {
                return if blink {
                    dot(p, 0.0, 0.6 * k, 0.3)
                } else {
                    dot(p, 1.0, 1.7 * k, 1.0)
                };
            }
            let behind = cx - f;
            let fresh = if behind > 0.0 && behind < 1.2 {
                1.0 - behind / 1.2
            } else {
                0.0
            };
            let typed = f < cx;
            dot(
                p,
                if typed {
                    0.5 + 0.5 * fresh * fresh
                } else {
                    0.0
                },
                (1.0 + 0.4 * fresh) * k,
                if typed { 1.0 } else { 0.35 },
            )
        }
        // The globe spins fast with a helical stripe racing round it.
        ActivityState::Running => {
            let (phi, lam, _) = size.grid(i);
            let p = globe(size, i, 2.4 * t * size.speed(), R);
            let s = 0.5 + 0.5 * (2.0 * phi + lam - TAU * t / 0.9).sin();
            let lit = cl(s * 1.4 - 0.4);
            dot(p, lit * 0.75, (0.9 + 0.25 * lit) * k, 0.5 + 0.5 * lit)
        }
        // The globe stops and breathes, asking for you.
        ActivityState::Waiting => {
            let b = 0.5 + 0.5 * (TAU * t / 1.8).sin();
            let p = globe(size, i, 0.0, R * (0.96 + 0.06 * b));
            dot(p, 0.2 + 0.5 * b, (0.9 + 0.25 * b) * k, 0.6 + 0.4 * b)
        }
        // The dots gather into a check that draws itself, pops, then holds.
        ActivityState::Done => {
            let pop = if t > 0.7 && t < 0.95 {
                (PI * (t - 0.7) / 0.25).sin()
            } else {
                0.0
            };
            symbol(
                size,
                i,
                &[CHECK],
                0.42,
                0.0,
                cl((t - 0.25) / 0.45),
                pop,
                0.25,
            )
        }
        // A cross that shakes once, like a head saying no, then holds.
        ActivityState::Error => {
            let st = t - 0.3;
            let dx = if st > 0.0 && st < 0.9 {
                0.13 * (-5.0 * st).exp() * (28.0 * st).sin()
            } else {
                0.0
            };
            symbol(size, i, CROSS, 0.42, dx, 1.0, 0.0, 1.0)
        }
        // The globe dims and its rings drop out, one flicker at a time.
        ActivityState::Offline => {
            let (_, _, j) = size.grid(i);
            let p = globe(size, i, 0.12 * t, R * 0.92);
            let slot = (t / 0.4).floor();
            let off = hash2(j as f64, slot) < 0.42 || hash2(i as f64, slot) < 0.12;
            dot(
                p,
                0.0,
                if off { 0.0 } else { 0.85 * k },
                if off { 0.0 } else { 0.45 },
            )
        }
    }
}

/// The critically damped morph spring (ω = 7); reaches exactly 1 at u = 1.6.
pub(crate) fn crit(u: f64) -> f64 {
    if u <= 0.0 {
        0.0
    } else if u >= 1.6 {
        1.0
    } else {
        1.0 - (1.0 + 7.0 * u) * (-7.0 * u).exp()
    }
}

/// The brighten pulse added to `lit` for the first 150ms of a change.
pub(crate) fn pulse(tin: f64) -> f64 {
    if tin < crate::PULSE {
        0.8 * (PI * tin / crate::PULSE).sin()
    } else {
        0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ActivityState::*;
    use OrbSize::*;

    fn frame(size: OrbSize, state: ActivityState, t: f64) -> Vec<Dot> {
        (0..size.dots())
            .map(|i| generate(size, state, i, t))
            .collect()
    }

    fn xy_len(d: &Dot) -> f64 {
        d.p[0].hypot(d.p[1])
    }

    #[test]
    fn grids_match_the_reference() {
        assert_eq!(Large.dots(), MAX_DOTS);
        assert_eq!(Small.dots(), 86);
        assert_eq!(OrbSize::for_px(22.0), Small);
        assert_eq!(OrbSize::for_px(30.0), Small);
        assert_eq!(OrbSize::for_px(88.0), Large);
        for size in OrbSize::ALL {
            let mut last = (0, 0.0);
            for i in 0..size.dots() {
                let (phi, lam, j) = size.grid(i);
                assert!(phi.abs() < PI / 2.0 && (0.0..TAU).contains(&lam));
                assert!(j >= last.0, "rings are in order");
                if j == last.0 && i > 0 {
                    assert!(lam > last.1, "dots go round each ring");
                }
                last = (j, lam);
            }
            assert_eq!(last.0, size.rings() - 1);
        }
    }

    #[test]
    fn every_state_is_finite() {
        for size in OrbSize::ALL {
            for state in ActivityState::ALL {
                for k in 0..120 {
                    let t = k as f64 * 0.037;
                    for d in frame(size, state, t) {
                        let vals = [d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a];
                        assert!(
                            vals.iter().all(|v| v.is_finite()),
                            "{size:?} {state:?} t={t}: {d:?}"
                        );
                        assert!(
                            d.p.iter().all(|v| v.abs() < 1.2),
                            "{state:?} escaped: {d:?}"
                        );
                        assert!((0.0..=1.0).contains(&d.lit), "{state:?}: {d:?}");
                        assert!((0.0..=1.0).contains(&d.a), "{state:?}: {d:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn deterministic() {
        for state in ActivityState::ALL {
            for i in [0, 1, 59, 85] {
                assert_eq!(
                    generate(Small, state, i, 0.731),
                    generate(Small, state, i, 0.731)
                );
            }
        }
    }

    /// Whole-cloud checksums, Σ value·(1 + i/7) over all dots, from the JS reference
    /// (`docs/design/v7.5/reference/v7.5-orb.js`) at the same times, for both sizes.
    #[test]
    fn matches_the_reference() {
        #[rustfmt::skip]
        let sums: [(OrbSize, ActivityState, f64, [f64; 6]); 80] = [
            (Large, Thinking, 0.0, [-14.3261699283, 1680.3413729625, 400.0591006128, 909.7388813570, 5812.2595205816, 3662.2420581122]),
            (Large, Thinking, 0.37, [-35.1222103230, 1678.9858024669, 404.4412915049, 904.0098259692, 5809.8042111296, 3658.1498756923]),
            (Large, Thinking, 1.1, [-72.0003486077, 1673.1434145716, 423.3281432808, 848.9334757905, 5786.2000610531, 3618.8096255646]),
            (Large, Thinking, 2.9, [-115.2766007478, 1646.4091557123, 509.7527342979, 750.9813733423, 5744.2205885753, 3548.8438381016]),
            (Large, Notes, 0.0, [-14.3261699283, 1680.3413729625, 400.0591006128, 0.0000000000, 4518.6428571429, 1807.4571428571]),
            (Large, Notes, 0.37, [-14.3261699283, 1680.3413729625, 400.0591006128, 204.8571428571, 4611.7071428571, 1950.8571428571]),
            (Large, Notes, 1.1, [-14.3261699283, 1680.3413729625, 400.0591006128, 813.1428571429, 4862.8642857143, 2376.6571428571]),
            (Large, Notes, 2.9, [-14.3261699283, 1680.3413729625, 400.0591006128, 168.0000000000, 4596.5785714286, 1925.0571428571]),
            (Large, Reading, 0.0, [-14.3261699283, 1680.3413729625, 400.0591006128, 45.2770853182, 4842.5242569448, 2138.1301054568]),
            (Large, Reading, 0.37, [-24.8303417106, 1679.8058072945, 401.7904388206, 837.5500181440, 5238.6607233577, 2653.1075117936]),
            (Large, Reading, 1.1, [-44.8484301683, 1677.9216017252, 407.8815631932, 751.6159917159, 5195.6937101437, 2597.2503946153]),
            (Large, Reading, 2.9, [-86.5750639697, 1669.0324718584, 436.6177034873, 1135.9465122519, 5387.8589704117, 2847.0652329638]),
            (Large, Searching, 0.0, [-14.3261699283, 1680.3413729625, 400.0591006128, 183.2012363579, 5221.8892514254, 2519.8635989576]),
            (Large, Searching, 0.37, [-26.9085465527, 1679.6643718063, 402.2476613038, 210.9734411250, 5237.1639640473, 2536.5269218178]),
            (Large, Searching, 1.1, [-50.6184276323, 1677.1455654870, 410.3902773810, 240.0116248777, 5253.1349651113, 2553.9498320695]),
            (Large, Searching, 2.9, [-96.6647224556, 1665.0996269529, 449.3315218981, 152.9210072179, 5205.2351253984, 2501.6954614736]),
            (Large, Editing, 0.0, [-14.3261699283, 1680.3413729625, 400.0591006128, 73.3616899232, 4981.7678613754, 2436.9928571429]),
            (Large, Editing, 0.37, [-14.3261699283, 1680.3413729625, 400.0591006128, 122.3362640011, 4979.7632940585, 2507.1000000000]),
            (Large, Editing, 1.1, [-14.3261699283, 1680.3413729625, 400.0591006128, 236.2299203962, 4982.1377826783, 2651.1214285714]),
            (Large, Editing, 2.9, [-14.3261699283, 1680.3413729625, 400.0591006128, 141.7097525559, 4979.1272424752, 2532.4500000000]),
            (Large, Running, 0.0, [-14.3261699283, 1680.3413729625, 400.0591006128, 1750.9560271176, 6006.0234376106, 4179.7325895069]),
            (Large, Running, 0.37, [-97.7795423163, 1664.5824531054, 451.0034043503, 1810.0277819481, 6025.7140225541, 4219.1137593940]),
            (Large, Running, 1.1, [-42.4368628374, 1614.8641847116, 611.7290498463, 1771.2386231926, 6012.7843029690, 4193.2543202237]),
            (Large, Running, 2.9, [-82.8083238083, 1670.2386922715, 432.7183208102, 1771.2386231926, 6012.7843029690, 4193.2543202237]),
            (Large, Waiting, 0.0, [-14.1829082290, 1663.5379592329, 396.0585096067, 2711.1857142857, 6175.4785714286, 4819.8857142857]),
            (Large, Waiting, 0.37, [-14.5960441811, 1711.9953931708, 407.5953542926, 4159.0518130180, 6899.4116207947, 5978.1785932715]),
            (Large, Waiting, 1.1, [-13.9066476933, 1631.1348807954, 388.3439186162, 1743.0098338957, 5691.3906312336, 4045.3450099737]),
            (Large, Waiting, 2.9, [-13.9066476933, 1631.1348807954, 388.3439186162, 1743.0098338957, 5691.3906312336, 4045.3450099737]),
            (Large, Done, 0.0, [-385.9049567676, 19.8430811385, 1807.4571428571, 281.2500000000, 4105.8000000000, 1864.2071428571]),
            (Large, Done, 0.37, [-385.9049567676, 19.8430811385, 1807.4571428571, 357.6428571429, 4166.9142857143, 1930.4142857143]),
            (Large, Done, 1.1, [-385.9049567676, 19.8430811385, 1807.4571428571, 1122.0000000000, 4778.4000000000, 2592.8571428571]),
            (Large, Done, 2.9, [-385.9049567676, 19.8430811385, 1807.4571428571, 1122.0000000000, 4778.4000000000, 2592.8571428571]),
            (Large, Error, 0.0, [-560.4261061174, -76.7442857143, 1807.4571428571, 1122.0000000000, 4778.4000000000, 2592.8571428571]),
            (Large, Error, 0.37, [-49.7705408654, -76.7442857143, 1807.4571428571, 1122.0000000000, 4778.4000000000, 2592.8571428571]),
            (Large, Error, 1.1, [-566.1291950579, -76.7442857143, 1807.4571428571, 1122.0000000000, 4778.4000000000, 2592.8571428571]),
            (Large, Error, 2.9, [-560.4261061174, -76.7442857143, 1807.4571428571, 1122.0000000000, 4778.4000000000, 2592.8571428571]),
            (Large, Offline, 0.0, [-13.1800763340, 1545.9140631255, 368.0543725638, 0.0000000000, 2909.0642857143, 1540.0928571429]),
            (Large, Offline, 0.37, [-17.8379005017, 1545.7105347446, 368.7123244887, 0.0000000000, 2909.0642857143, 1540.0928571429]),
            (Large, Offline, 1.1, [-26.9158798983, 1545.1308807063, 370.5861884121, 0.0000000000, 3659.2500000000, 1937.2500000000]),
            (Large, Offline, 2.9, [-48.2764293725, 1542.7216548927, 378.3745605044, 0.0000000000, 2682.6000000000, 1420.2000000000]),
            (Small, Thinking, 0.0, [-4.2882595841, 162.1070954325, 32.5981228788, 99.9436538847, 678.6858008432, 375.4597527748]),
            (Small, Thinking, 0.37, [-7.7312193585, 161.7278198871, 33.8242176086, 99.7714695252, 678.6009385517, 375.3367639466]),
            (Small, Thinking, 1.1, [-13.3688110190, 160.3995881238, 38.1180298113, 92.1817931741, 674.8603123501, 369.9155665529]),
            (Small, Thinking, 2.9, [-16.2713787995, 155.4133970910, 54.2370298932, 74.4539171791, 666.1230020383, 357.2527979851]),
            (Small, Notes, 0.0, [-4.2882595841, 162.1070954325, 32.5981228788, 0.0000000000, 524.5232142857, 182.4428571429]),
            (Small, Notes, 0.37, [-4.2882595841, 162.1070954325, 32.5981228788, 18.1428571429, 532.8689285714, 195.1428571429]),
            (Small, Notes, 1.1, [-4.2882595841, 162.1070954325, 32.5981228788, 110.1428571429, 578.8114285714, 259.5428571429]),
            (Small, Notes, 2.9, [-4.2882595841, 162.1070954325, 32.5981228788, 15.2857142857, 532.9592857143, 193.1428571429]),
            (Small, Reading, 0.0, [-4.2882595841, 162.1070954325, 32.5981228788, 4.9262807383, 562.3240399959, 216.0520824799]),
            (Small, Reading, 0.37, [-6.0439025529, 161.9445413919, 33.1236159007, 99.9153680539, 616.9427652024, 277.7949892350]),
            (Small, Reading, 1.1, [-9.2892020114, 161.4672910655, 34.6664364627, 62.1929187029, 595.2523568256, 253.2753971569]),
            (Small, Reading, 2.9, [-15.2949879432, 159.5443321866, 40.8828397497, 90.2811757469, 611.4031046259, 271.5327642355]),
            (Small, Searching, 0.0, [-4.2882595841, 162.1070954325, 32.5981228788, 13.3427518296, 602.8989333893, 251.2627939549]),
            (Small, Searching, 0.37, [-6.3874549279, 161.9054607497, 33.2499529926, 25.6880152250, 610.7073124869, 258.6699519921]),
            (Small, Searching, 1.1, [-10.1933816669, 161.2851494915, 35.2552506550, 19.9693798627, 607.0902756203, 255.2387707748]),
            (Small, Searching, 2.9, [-16.4258849341, 158.7609456372, 43.4153154953, 17.5052710003, 605.5317267648, 253.7603054573]),
            (Small, Editing, 0.0, [-4.2882595841, 162.1070954325, 32.5981228788, 13.5285989798, 592.4001834069, 248.1285714286]),
            (Small, Editing, 0.37, [-4.2882595841, 162.1070954325, 32.5981228788, 23.5991289012, 592.7073983037, 261.2214285714]),
            (Small, Editing, 1.1, [-4.2882595841, 162.1070954325, 32.5981228788, 39.5634940785, 592.2043875516, 282.5785714286]),
            (Small, Editing, 2.9, [-4.2882595841, 162.1070954325, 32.5981228788, 26.8096404054, 592.6354923582, 265.3071428571]),
            (Small, Running, 0.0, [-4.2882595841, 162.1070954325, 32.5981228788, 175.9923256273, 696.8915819667, 421.3996456563]),
            (Small, Running, 0.37, [-16.5349830834, 158.6601268010, 43.7412353845, 184.5160627863, 700.1590145443, 427.0821370956]),
            (Small, Running, 1.1, [2.4975343182, 152.0930709925, 64.9707415183, 178.6768810017, 697.9206615269, 423.1893492393]),
            (Small, Running, 2.9, [-15.9345538922, 155.1596255790, 55.0574042024, 178.6768810017, 697.9206615269, 423.1893492393]),
            (Small, Waiting, 0.0, [-4.2453769882, 160.4860244782, 32.2721416500, 273.6642857143, 716.8483928571, 486.5142857143]),
            (Small, Waiting, 0.37, [-4.3690411786, 165.1608447226, 33.2122014565, 419.8103942718, 800.8824052777, 603.4311725603]),
            (Small, Waiting, 1.1, [-4.1626837844, 157.3600115066, 31.6435315654, 175.9376123416, 660.6555556678, 408.3329470161]),
            (Small, Waiting, 2.9, [-4.1626837844, 157.3600115066, 31.6435315654, 175.9376123416, 660.6555556678, 408.3329470161]),
            (Small, Done, 0.0, [-34.9118816494, -0.8544128669, 182.4428571429, 32.2500000000, 475.7550000000, 189.3928571429]),
            (Small, Done, 0.37, [-34.9118816494, -0.8544128669, 182.4428571429, 43.8214285714, 486.4007142857, 199.4214285714]),
            (Small, Done, 1.1, [-34.9118816494, -0.8544128669, 182.4428571429, 126.0000000000, 562.0050000000, 270.6428571429]),
            (Small, Done, 2.9, [-34.9118816494, -0.8544128669, 182.4428571429, 126.0000000000, 562.0050000000, 270.6428571429]),
            (Small, Error, 0.0, [-52.3856084267, -9.3857142857, 182.4428571429, 126.0000000000, 562.0050000000, 270.6428571429]),
            (Small, Error, 0.37, [-0.8405631078, -9.3857142857, 182.4428571429, 126.0000000000, 562.0050000000, 270.6428571429]),
            (Small, Error, 1.1, [-52.9612723339, -9.3857142857, 182.4428571429, 126.0000000000, 562.0050000000, 270.6428571429]),
            (Small, Error, 2.9, [-52.3856084267, -9.3857142857, 182.4428571429, 126.0000000000, 562.0050000000, 270.6428571429]),
            (Small, Offline, 0.0, [-3.9451988173, 149.1385277979, 29.9902730485, 0.0000000000, 257.7807142857, 118.6714285714]),
            (Small, Offline, 0.37, [-4.6258512881, 149.0822877678, 30.1720817765, 0.0000000000, 257.7807142857, 118.6714285714]),
            (Small, Offline, 1.1, [-5.9407606625, 148.9454282839, 30.6145112819, 0.0000000000, 457.7492857143, 210.7285714286]),
            (Small, Offline, 2.9, [-8.9681161518, 148.4677351366, 32.1587633635, 0.0000000000, 297.4392857143, 136.9285714286]),
        ];
        for (size, state, t, want) in sums {
            let mut got = [0.0; 6];
            for i in 0..size.dots() {
                let d = generate(size, state, i, t);
                for (g, v) in got
                    .iter_mut()
                    .zip([d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a])
                {
                    *g += v * (1.0 + i as f64 / 7.0);
                }
            }
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 1e-9, "{size:?} {state:?} t={t}: {got:?}");
            }
        }
    }

    /// Every busy state keeps its dots on the globe (radius R, or Waiting's breath).
    #[test]
    fn busy_states_stay_on_the_globe() {
        for size in OrbSize::ALL {
            for state in [
                Thinking, Notes, Reading, Searching, Editing, Running, Waiting, Offline,
            ] {
                for t in [0.0, 0.7, 1.5, 3.3] {
                    for d in frame(size, state, t) {
                        let r = xy_len(&d).hypot(d.p[2]);
                        assert!(
                            (0.82..=0.92).contains(&r),
                            "{size:?} {state:?} t={t} left the globe: {d:?}"
                        );
                    }
                }
            }
        }
    }

    /// Each state's still frame differs from every other state's.
    #[test]
    fn states_differ() {
        for size in OrbSize::ALL {
            let stills: Vec<Vec<Dot>> = ActivityState::ALL
                .iter()
                .map(|s| frame(size, *s, s.reduced_motion_time()))
                .collect();
            for (a, sa) in stills.iter().enumerate() {
                for (b, sb) in stills.iter().enumerate().skip(a + 1) {
                    let diff: f64 = sa
                        .iter()
                        .zip(sb)
                        .map(|(x, y)| {
                            (x.p[0] - y.p[0]).abs()
                                + (x.p[1] - y.p[1]).abs()
                                + (x.lit - y.lit).abs()
                                + (x.a - y.a).abs()
                        })
                        .sum();
                    assert!(
                        diff > 3.0,
                        "{size:?}: {:?} and {:?} look alike ({diff})",
                        ActivityState::ALL[a],
                        ActivityState::ALL[b]
                    );
                }
            }
        }
    }

    #[test]
    fn thinking_turns_with_drifting_patches() {
        let a = frame(Large, Thinking, 0.3);
        let b = frame(Large, Thinking, 0.9);
        assert_ne!(a[10].p, b[10].p, "the globe turns");
        let lit = |f: &[Dot]| f.iter().filter(|d| d.lit > 0.3).count();
        assert!(lit(&a) > 10 && lit(&a) < a.len() / 2, "{}", lit(&a));
        assert_ne!(
            a.iter().map(|d| d.lit).collect::<Vec<_>>(),
            b.iter().map(|d| d.lit).collect::<Vec<_>>(),
            "the patches drift"
        );
    }

    #[test]
    fn notes_write_three_lines_then_hold() {
        let lit = |t: f64| {
            frame(Large, Notes, t)
                .iter()
                .filter(|d| d.lit > 0.0)
                .count()
        };
        let rings: usize = [5, 7, 9].iter().map(|&j| Large.ring_len(j)).sum();
        assert!(lit(0.3) < lit(1.0));
        assert!(lit(1.0) < lit(1.9));
        assert_eq!(lit(1.95), rings);
        assert_eq!(lit(2.5), rings);
        assert!(lit(2.7) < rings, "starts over");
    }

    #[test]
    fn reading_band_moves_down() {
        let band_y = |t: f64| {
            let f = frame(Large, Reading, t);
            let (sum, n) = f
                .iter()
                .filter(|d| d.lit > 0.5)
                .fold((0.0, 0), |(s, n), d| (s + d.p[1], n + 1));
            assert!(n > 0, "nothing lit at t={t}");
            sum / n as f64
        };
        assert!(band_y(0.4) > band_y(1.0));
        assert!(band_y(1.0) > band_y(1.8));
    }

    #[test]
    fn searching_lens_roams_the_front() {
        for t in [0.0, 0.8, 1.6] {
            let f = frame(Large, Searching, t);
            let lens: Vec<&Dot> = f.iter().filter(|d| d.lit > 0.9).collect();
            assert!(lens.len() >= 4, "t={t}: {}", lens.len());
            for d in &lens {
                assert!(d.p[2] > 0.3, "the lens is on the front: {d:?}");
                assert!(d.s > 1.3);
            }
        }
        let c = |t: f64| {
            let f = frame(Large, Searching, t);
            let l: Vec<&Dot> = f.iter().filter(|d| d.lit > 0.9).collect();
            l.iter().map(|d| d.p[0]).sum::<f64>() / l.len() as f64
        };
        assert!((c(0.0) - c(0.8)).abs() > 0.1, "the lens moves");
    }

    #[test]
    fn editing_types_along_the_equator_then_blinks() {
        let eq = 7;
        let typed = |t: f64| {
            frame(Large, Editing, t)
                .iter()
                .enumerate()
                .filter(|(i, d)| Large.grid(*i).2 == eq && d.a == 1.0)
                .count()
        };
        assert!(typed(0.3) < typed(0.9));
        assert!(typed(0.9) < typed(1.6));
        let caret_on = |t: f64| frame(Large, Editing, t).iter().any(|d| d.s > 1.6);
        assert!(caret_on(1.0));
        assert!(caret_on(1.7));
        assert!(!caret_on(1.9), "the caret blinks off");
        assert!(caret_on(2.1));
        for (i, d) in frame(Large, Editing, 0.5).iter().enumerate() {
            if Large.grid(i).2 != eq {
                assert_eq!((d.lit, d.a), (0.0, 0.4));
            }
        }
    }

    #[test]
    fn running_spins_fast() {
        let a = frame(Large, Running, 0.0);
        let b = frame(Large, Running, 0.1);
        let moved = a
            .iter()
            .zip(&b)
            .map(|(x, y)| (x.p[0] - y.p[0]).abs() + (x.p[2] - y.p[2]).abs())
            .sum::<f64>()
            / a.len() as f64;
        assert!(moved > 0.1, "{moved}");
        assert!(a.iter().any(|d| d.lit > 0.6) && a.iter().any(|d| d.lit == 0.0));
    }

    #[test]
    fn waiting_breathes() {
        let r = |t: f64| {
            let d = frame(Large, Waiting, t)[0];
            xy_len(&d).hypot(d.p[2])
        };
        assert!(r(0.45) > r(1.35));
        assert!(frame(Large, Waiting, 0.45)[0].lit > frame(Large, Waiting, 1.35)[0].lit);
    }

    #[test]
    fn done_draws_a_check_then_holds() {
        for size in OrbSize::ALL {
            let settled = frame(size, Done, 0.95);
            for t in [1.0, crate::SETTLE, 5.0] {
                assert_eq!(frame(size, Done, t), settled, "t={t}");
            }
            let n = (size.dots() as f64 * 0.42).round() as usize;
            for d in &settled[..n] {
                assert_eq!(d.lit, 1.0, "check dot unlit: {d:?}");
                assert!(xy_len(d) < 0.62);
            }
            for d in &settled[n..] {
                assert!((xy_len(d) - R).abs() < 1e-9, "ring dot off the ring: {d:?}");
                assert_eq!(d.lit, 0.0);
            }
            let mid = frame(size, Done, 0.45);
            assert_eq!(mid[0].lit, 1.0);
            assert!(mid[n - 1].lit < 1.0);
        }
    }

    #[test]
    fn error_shakes_then_holds() {
        let settled = frame(Large, Error, 1.25);
        for t in [crate::SETTLE, 5.0, 60.0] {
            assert_eq!(frame(Large, Error, t), settled, "t={t}");
        }
        let cx = |f: &[Dot]| f.iter().map(|d| d.p[0]).sum::<f64>() / f.len() as f64;
        assert!((cx(&frame(Large, Error, 0.36)) - cx(&settled)).abs() > 0.05);
        assert!(settled[..119].iter().any(|d| xy_len(d) < 0.02));
        assert!(settled[119..].iter().all(|d| (xy_len(d) - R).abs() < 1e-9));
    }

    #[test]
    fn offline_dims_and_drops_rings() {
        for size in OrbSize::ALL {
            let mut any_off = false;
            for k in 0..10 {
                let f = frame(size, Offline, k as f64 * 0.4);
                let off = f.iter().filter(|d| d.s == 0.0).count();
                any_off |= off > 0;
                assert!(off < f.len(), "never everything");
                for d in f.iter().filter(|d| d.s > 0.0) {
                    assert_eq!((d.lit, d.a), (0.0, 0.45));
                }
            }
            assert!(any_off, "{size:?}: some rings drop out");
        }
    }

    #[test]
    fn springs_end_at_one() {
        assert_eq!(crit(0.0), 0.0);
        assert_eq!(crit(1.6), 1.0);
        assert!(crit(1.59) > 0.999);
    }

    #[test]
    fn along_hits_the_ends() {
        assert_eq!(along(&[CHECK], 0.0), CHECK[0]);
        let end = along(&[CHECK], 1.0);
        assert!((end[0] - CHECK[2][0]).abs() < 1e-12 && (end[1] - CHECK[2][1]).abs() < 1e-12);
    }
}
