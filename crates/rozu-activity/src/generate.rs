//! Per-state point generators, ported 1:1 from `docs/design/v7.4/reference/v7.4-orb.js`.
//!
//! Every state is the same frame: a fully dotted ring of [`RING`] dots (indices `0..RING`) on
//! the plane [`GLYPH_Z`] facing the camera, around a symbol drawn by the other dots. The ring
//! dots never move between states, so a change only reshapes the symbol. Thinking's symbol is a
//! small turning sphere; the rest are flat strokes. The arithmetic (including JS's truncating
//! `%`) is kept as in the reference so samples match it at the same `t`.

use std::f64::consts::{PI, TAU};

use crate::state::ActivityState;

/// A point in orb space (unit-radius sphere, +y up, +z toward the camera).
pub type Vec3 = [f64; 3];

/// Dots per orb (≥30px). 22px orbs draw a 60-dot subsample.
pub const N_POINTS: usize = 120;

/// Camera distance: perspective `k = F / (F − z)`.
pub const CAMERA: f64 = 3.2;

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

/// Fibonacci-sphere direction `i` of `n`.
pub fn fib(i: usize, n: usize) -> Vec3 {
    let y = 1.0 - 2.0 * (i as f64 + 0.5) / n as f64;
    let r = (1.0 - y * y).sqrt();
    let a = i as f64 * 2.399_963_23;
    [a.cos() * r, y, a.sin() * r]
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

fn dot(p: Vec3, lit: f64, s: f64, a: f64) -> Dot {
    Dot { p, lit, s, a }
}

/// Dots `0..RING` are the ring; the rest draw the symbol. Both are contiguous index runs, so
/// the 22px subsample keeps an even share of each.
pub const RING: usize = 44;
/// Symbol dots.
pub const GLYPH: usize = N_POINTS - RING;
/// The plane the ring and the flat symbols sit on (toward the camera).
pub const GLYPH_Z: f64 = 0.3;
/// The ring's radius.
pub const GLYPH_R: f64 = 0.86;

type Pt = [f64; 2];

/// Ring dot `j`, clockwise from 12 o'clock.
fn ring_at(j: usize) -> Vec3 {
    let a = PI / 2.0 - TAU * j as f64 / RING as f64;
    [GLYPH_R * a.cos(), GLYPH_R * a.sin(), GLYPH_Z]
}

/// Ring dot `j`'s angle, clockwise from 12 o'clock.
fn ring_angle(j: usize) -> f64 {
    (TAU * j as f64 / RING as f64) % TAU
}

/// A ring dot: resting (`lit` 0) or lit by a sweep.
fn ring(j: usize, lit: f64, a: f64) -> Dot {
    dot(ring_at(j), lit, 0.85 + 0.4 * lit, a.max(lit))
}

/// A symbol dot on the glyph plane.
fn flat(xy: Pt, lit: f64, s: f64, a: f64) -> Dot {
    dot([xy[0], xy[1], GLYPH_Z], lit, s, a)
}

/// Symbol dot `i` as a fraction `0..=1` along its path.
fn gu(i: usize) -> f64 {
    (i - RING) as f64 / (GLYPH - 1) as f64
}

/// How far (rad, `0..TAU`) ring dot `j` is behind a head moving clockwise at angle `h`.
fn behind(j: usize, h: f64) -> f64 {
    ((h - ring_angle(j)) % TAU + TAU) % TAU
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

/// `n + 1` points round an arc of `c`, radius `r`, from `a0` to `a1`.
fn arc(c: Pt, r: f64, a0: f64, a1: f64, n: usize) -> Vec<Pt> {
    (0..=n)
        .map(|k| {
            let a = a0 + (a1 - a0) * k as f64 / n as f64;
            [c[0] + r * a.cos(), c[1] + r * a.sin()]
        })
        .collect()
}

const CHECK: &[Pt] = &[[-0.46, 0.02], [-0.14, -0.32], [0.48, 0.34]];
const CROSS_A: &[Pt] = &[[-0.36, 0.36], [0.36, -0.36]];
const CROSS_B: &[Pt] = &[[0.36, 0.36], [-0.36, -0.36]];
const NOTES: &[&[Pt]] = &[
    &[[-0.44, 0.26], [0.44, 0.26]],
    &[[-0.44, 0.0], [0.44, 0.0]],
    &[[-0.44, -0.26], [0.16, -0.26]],
];
const READ: &[&[Pt]] = &[
    &[[-0.46, 0.3], [0.46, 0.3]],
    &[[-0.46, 0.1], [0.46, 0.1]],
    &[[-0.46, -0.1], [0.46, -0.1]],
    &[[-0.46, -0.3], [0.2, -0.3]],
];
const PLAY: &[Pt] = &[[-0.2, 0.34], [-0.2, -0.34], [0.36, 0.0], [-0.2, 0.34]];
const PAUSE: &[&[Pt]] = &[
    &[[-0.17, 0.32], [-0.17, -0.32]],
    &[[0.17, 0.32], [0.17, -0.32]],
];

/// The resting ring dot α.
const RING_A: f64 = 0.55;

/// Dot `i` of `state` at `t` seconds into the state.
pub fn generate(state: ActivityState, i: usize, t: f64) -> Dot {
    match state {
        // A small dot sphere turns inside the ring while a lit arc sweeps round the ring.
        ActivityState::Thinking => {
            if i < RING {
                let g = behind(i, TAU * t / 1.6);
                let tail = if g < 2.2 { 1.0 - g / 2.2 } else { 0.0 };
                return ring(i, tail * tail, RING_A);
            }
            let d = fib(i - RING, GLYPH);
            let p = rot_x(
                rot_y([d[0] * 0.46, d[1] * 0.46, d[2] * 0.46], t * 1.1),
                0.45,
            );
            let lit = cl(p[2] / 0.46 * 0.8);
            dot(p, lit * 0.7, 0.8 + 0.35 * lit, 0.4 + 0.6 * lit)
        }
        // Three lines write themselves left to right, then hold and start over.
        ActivityState::Notes => {
            if i < RING {
                return ring(i, 0.0, RING_A);
            }
            let u = gu(i);
            let w = cl((t % 2.6) / 1.9);
            let on = u <= w;
            let head = on && u > w - 0.04 && w < 1.0;
            let s = if head {
                1.5
            } else if on {
                1.0
            } else {
                0.45
            };
            flat(
                along(NOTES, u),
                if on { 1.0 } else { 0.0 },
                s,
                if on { 1.0 } else { 0.16 },
            )
        }
        // Four lines of text; a lit band moves down the page one line at a time.
        ActivityState::Reading => {
            if i < RING {
                return ring(i, 0.0, RING_A);
            }
            let xy = along(READ, gu(i));
            let yb = 0.42 - 0.84 * ((t % 2.2) / 2.2);
            let on = (-((xy[1] - yb) / 0.09).powi(2)).exp();
            flat(xy, on, 0.7 + 0.5 * on, 0.28 + 0.72 * on)
        }
        // A magnifying glass circles slowly, scanning.
        ActivityState::Searching => {
            if i < RING {
                return ring(i, 0.0, RING_A);
            }
            let a = TAU * t / 2.4;
            let c = [-0.08 + 0.12 * a.cos(), 0.08 + 0.12 * a.sin()];
            let u = gu(i);
            let xy = if u < 0.74 {
                along(&[&arc(c, 0.24, 0.0, TAU, 36)], u / 0.74)
            } else {
                let handle: &[Pt] = &[[c[0] + 0.17, c[1] - 0.17], [c[0] + 0.4, c[1] - 0.4]];
                along(&[handle], (u - 0.74) / 0.26)
            };
            flat(xy, 1.0, 1.05, 1.0)
        }
        // A line types out behind a caret, which then blinks.
        ActivityState::Editing => {
            if i < RING {
                return ring(i, 0.0, RING_A);
            }
            let ph = t % 2.4;
            let w = cl(ph / 1.6);
            let x = -0.46 + 0.92 * w;
            let u = gu(i);
            if u < 0.14 {
                let y = -0.24 + 0.5 * (u / 0.14);
                let blink = w >= 1.0 && ((ph - 1.6) / 0.2).floor() % 2.0 == 1.0;
                return if blink {
                    flat([x + 0.05, y], 0.0, 0.0, 0.0)
                } else {
                    flat([x + 0.05, y], 1.0, 0.95, 1.0)
                };
            }
            let v = (u - 0.14) / 0.86;
            let xx = -0.46 + 0.92 * v;
            let on = xx <= x;
            flat(
                [xx, -0.2],
                if on { 1.0 } else { 0.0 },
                if on { 1.0 } else { 0.45 },
                if on { 1.0 } else { 0.16 },
            )
        }
        // A play triangle; two lit arcs chase each other round the ring.
        ActivityState::Running => {
            if i < RING {
                let mut lit: f64 = 0.0;
                for off in [0.0, PI] {
                    let g = behind(i, TAU * t / 0.9 + off);
                    lit = lit.max(if g < 1.4 { 1.0 - g / 1.4 } else { 0.0 });
                }
                return ring(i, lit, RING_A);
            }
            flat(along(&[PLAY], gu(i)), 1.0, 1.05, 1.0)
        }
        // Pause bars; the whole ring breathes, asking for you.
        ActivityState::Waiting => {
            let b = 0.5 + 0.5 * (TAU * t / 1.8).sin();
            if i < RING {
                return ring(i, 0.25 + 0.55 * b, RING_A);
            }
            flat(along(PAUSE, gu(i)), 1.0, 1.05 + 0.15 * b, 1.0)
        }
        // A check mark draws itself once, pops, then holds.
        ActivityState::Done => {
            if i < RING {
                return ring(i, 0.0, RING_A);
            }
            let u = gu(i);
            let on = u <= cl((t - 0.25) / 0.45);
            let pop = if t > 0.7 && t < 0.95 {
                (PI * (t - 0.7) / 0.25).sin()
            } else {
                0.0
            };
            flat(
                along(&[CHECK], u),
                if on { 1.0 } else { 0.25 },
                (if on { 1.15 } else { 0.6 }) * (1.0 + 0.2 * pop),
                if on { 1.0 } else { 0.35 },
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
            if i < RING {
                let mut r = ring(i, 0.0, RING_A);
                r.p[0] += dx;
                return r;
            }
            let xy = along(&[CROSS_A, CROSS_B], gu(i));
            flat([xy[0] + dx, xy[1]], 1.0, 1.15, 1.0)
        }
        // The ring is broken at the top; a retry arrow inside turns slowly.
        ActivityState::Offline => {
            if i < RING {
                let a = ring_angle(i);
                let gap = !(0.55..=TAU - 0.55).contains(&a);
                return if gap {
                    dot(ring_at(i), 0.0, 0.0, 0.0)
                } else {
                    ring(i, 0.0, 0.4)
                };
            }
            let u = gu(i);
            let rot = -TAU * t / 2.4;
            if u < 0.8 {
                let a = rot + PI * 0.5 + 1.6 * PI * (u / 0.8);
                return flat([0.4 * a.cos(), 0.4 * a.sin()], 0.7, 1.05, 0.9);
            }
            let tip = rot + PI * 0.5 + 1.6 * PI;
            let v = (u - 0.8) / 0.2;
            let side = if v < 0.5 { 1.0 } else { -1.0 };
            let k = 0.24 * (if v < 0.5 { v } else { v - 0.5 }) * 2.0;
            // The arrow travels anticlockwise; its barbs point back from the tip.
            let back = tip - PI * 0.5 + side * 0.8;
            flat(
                [
                    0.4 * tip.cos() + k * back.cos(),
                    0.4 * tip.sin() + k * back.sin(),
                ],
                0.7,
                1.05,
                0.9,
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

    fn frame(state: ActivityState, t: f64) -> Vec<Dot> {
        (0..N_POINTS).map(|i| generate(state, i, t)).collect()
    }

    fn xy_len(d: &Dot) -> f64 {
        d.p[0].hypot(d.p[1])
    }

    #[test]
    fn every_state_is_finite() {
        for state in ActivityState::ALL {
            for k in 0..200 {
                let t = k as f64 * 0.037;
                for d in frame(state, t) {
                    let vals = [d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a];
                    assert!(vals.iter().all(|v| v.is_finite()), "{state:?} t={t}: {d:?}");
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

    #[test]
    fn deterministic() {
        for state in ActivityState::ALL {
            for i in [0, 1, 59, 119] {
                assert_eq!(generate(state, i, 0.731), generate(state, i, 0.731));
            }
        }
    }

    #[test]
    fn fib_is_on_unit_sphere() {
        for i in 0..N_POINTS {
            let f = fib(i, N_POINTS);
            assert!(((f[0] * f[0] + f[1] * f[1] + f[2] * f[2]).sqrt() - 1.0).abs() < 1e-12);
        }
    }

    /// Whole-cloud checksums, Σ value·(1 + i/7) over all 120 dots, from the JS reference
    /// (`docs/design/v7.4/reference/v7.4-orb.js`) at the same times.
    #[test]
    fn matches_the_reference() {
        #[rustfmt::skip]
        let sums: [(ActivityState, f64, [f64; 6]); 40] = [
            (Thinking, 0.0, [-38.10003945306034, -59.66857415682875, 26.257035293189794, 149.7327920203205, 992.6198246469887, 588.6462297101664]),
            (Thinking, 0.37, [-38.06346560443972, -59.72079024502736, 26.36513081255245, 129.731228365492, 984.672700764301, 586.5057603163143]),
            (Thinking, 1.23, [-37.82976403215396, -59.79012044062098, 26.50865522732102, 142.7916380430038, 989.8289487941374, 587.585320357731]),
            (Thinking, 4.9, [-38.006980270831384, -59.55916953553002, 26.030550511047768, 136.61745335776249, 987.2851649589428, 584.0055500283909]),
            (Notes, 0.0, [-73.77183323675257, -22.165714285714305, 342.0, 7.285714285714286, 592.3071428571428, 258.38571428571436]),
            (Notes, 0.37, [-73.77183323675257, -22.165714285714305, 342.0, 124.28571428571429, 666.7285714285715, 356.6657142857143]),
            (Notes, 1.23, [-73.77183323675257, -22.165714285714305, 342.0, 525.0, 894.4071428571428, 693.2657142857144]),
            (Notes, 4.9, [-73.77183323675257, -22.165714285714305, 342.0, 960.8571428571429, 1113.1285714285714, 1059.3857142857144]),
            (Reading, 0.0, [-50.08802371294276, -30.017142857142854, 342.0, 30.930189640375353, 840.3365233916161, 389.83830796964173]),
            (Reading, 0.37, [-50.08802371294276, -30.017142857142854, 342.0, 177.56877677296816, 913.6558169579126, 495.4180907051086]),
            (Reading, 1.23, [-50.08802371294276, -30.017142857142854, 342.0, 226.81664836292958, 938.2797527528935, 530.8765582498806]),
            (Reading, 4.9, [-50.08802371294276, -30.017142857142854, 342.0, 128.12777086035143, 888.9353140016042, 459.8205664480244]),
            (Searching, 0.0, [98.47954217188457, -39.59998967180963, 342.0, 960.8571428571429, 1161.1714285714286, 1059.3857142857144]),
            (Searching, 0.37, [48.48494244999473, 55.42411452256196, 342.0, 960.8571428571429, 1161.1714285714286, 1059.3857142857144]),
            (Searching, 1.23, [-131.77073188564148, -48.64654757807511, 342.0, 960.8571428571429, 1161.1714285714286, 1059.3857142857144]),
            (Searching, 4.9, [94.55069258823207, -9.757414288502869, 342.0, 960.8571428571429, 1161.1714285714286, 1059.3857142857144]),
            (Editing, 0.0, [-24.136883070639563, -176.6936054421769, 342.0, 88.0, 628.657142857143, 326.18571428571425]),
            (Editing, 0.37, [-5.414883070639583, -176.6936054421769, 342.0, 235.8571428571429, 709.9785714285715, 450.38571428571424]),
            (Editing, 1.23, [38.10111692936043, -176.6936054421769, 342.0, 705.8571428571429, 968.4785714285715, 845.1857142857142]),
            (Editing, 4.9, [-19.07688307063949, -176.6936054421769, 342.0, 124.28571428571429, 648.6142857142858, 356.6657142857143]),
            (Running, 0.0, [-29.910731209383144, 16.067388457659657, 342.0, 1011.8486746735757, 1181.5680412980016, 1070.2028823578087]),
            (Running, 0.37, [-29.910731209383144, 16.067388457659657, 342.0, 1011.1355214578582, 1181.2827800117145, 1071.185356852144]),
            (Running, 1.23, [-29.910731209383144, 16.067388457659657, 342.0, 1007.7138018111273, 1179.9140921530222, 1070.3011654370184]),
            (Running, 4.9, [-29.910731209383144, 16.067388457659657, 342.0, 1008.2353444447855, 1180.1227092064855, 1069.2821147891475]),
            (Waiting, 0.0, [-2.7223094272286716, -25.864761904761927, 342.0, 1054.9071428571428, 1270.8557142857144, 1059.3857142857144]),
            (Waiting, 0.37, [-2.7223094272286716, -25.864761904761927, 342.0, 1102.2630136920473, 1359.070700121974, 1102.2630136920473]),
            (Waiting, 1.23, [-2.7223094272286716, -25.864761904761927, 342.0, 1009.9019784188499, 1187.0196476378528, 1059.3857142857144]),
            (Waiting, 4.9, [-2.7223094272286716, -25.864761904761927, 342.0, 1006.3912923391057, 1180.479906791784, 1059.3857142857144]),
            (Done, 0.0, [37.400462550267264, -10.621910649673428, 342.0, 245.67857142857144, 732.7928571428574, 439.5642857142857]),
            (Done, 0.37, [37.400462550267264, -10.621910649673428, 342.0, 377.4642857142858, 829.4357142857143, 553.7785714285714]),
            (Done, 1.23, [37.400462550267264, -10.621910649673428, 342.0, 960.8571428571429, 1257.2571428571432, 1059.3857142857144]),
            (Done, 4.9, [37.400462550267264, -10.621910649673428, 342.0, 960.8571428571429, 1257.2571428571432, 1059.3857142857144]),
            (Error, 0.0, [-42.40299514151438, -28.759999999999998, 342.0, 960.8571428571429, 1257.2571428571432, 1059.3857142857144]),
            (Error, 0.37, [54.2212617634737, -28.759999999999998, 342.0, 960.8571428571429, 1257.2571428571432, 1059.3857142857144]),
            (Error, 1.23, [-42.40299514151438, -28.759999999999998, 342.0, 960.8571428571429, 1257.2571428571432, 1059.3857142857144]),
            (Error, 4.9, [-42.40299514151438, -28.759999999999998, 342.0, 960.8571428571429, 1257.2571428571432, 1059.3857142857144]),
            (Offline, 0.0, [63.466577130751176, -68.90277623683612, 342.0, 672.5999999999999, 1139.1928571428573, 926.0857142857144]),
            (Offline, 0.37, [-34.99511212706994, -123.64782712161652, 342.0, 672.5999999999999, 1139.1928571428573, 926.0857142857144]),
            (Offline, 1.23, [-133.54221010297692, 71.23755828298623, 342.0, 672.5999999999999, 1139.1928571428573, 926.0857142857144]),
            (Offline, 4.9, [42.88251307202216, -92.85442727956617, 342.0, 672.5999999999999, 1139.1928571428573, 926.0857142857144]),
        ];
        for (state, t, want) in sums {
            let mut got = [0.0; 6];
            for i in 0..N_POINTS {
                let d = generate(state, i, t);
                for (g, v) in got
                    .iter_mut()
                    .zip([d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a])
                {
                    *g += v * (1.0 + i as f64 / 7.0);
                }
            }
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 1e-9, "{state:?} t={t}: {got:?}");
            }
        }
    }

    /// Every state keeps the full dotted ring in place (Offline's gap and Error's shake aside).
    #[test]
    fn the_ring_is_shared() {
        for state in ActivityState::ALL {
            for t in [0.0, 0.7, 1.5, 3.3] {
                let f = frame(state, t);
                for (j, d) in f.iter().take(RING).enumerate() {
                    let want = ring_at(j);
                    if state == Offline && d.s == 0.0 {
                        continue;
                    }
                    let dx = if state == Error {
                        d.p[0] - want[0]
                    } else {
                        0.0
                    };
                    assert!(
                        (d.p[0] - want[0] - dx).abs() < 1e-12
                            && (d.p[1] - want[1]).abs() < 1e-12
                            && d.p[2] == GLYPH_Z,
                        "{state:?} ring dot {j} moved at t={t}: {d:?}"
                    );
                    assert!(
                        d.s > 0.0 && d.a >= 0.4,
                        "{state:?} ring dot {j} faded: {d:?}"
                    );
                }
            }
        }
    }

    /// Each symbol is distinct: its still frame differs from every other state's.
    #[test]
    fn symbols_differ() {
        let stills: Vec<Vec<Dot>> = ActivityState::ALL
            .iter()
            .map(|s| frame(*s, s.reduced_motion_time())[RING..].to_vec())
            .collect();
        for (a, sa) in stills.iter().enumerate() {
            for (b, sb) in stills.iter().enumerate().skip(a + 1) {
                let diff: f64 = sa
                    .iter()
                    .zip(sb)
                    .map(|(x, y)| (x.p[0] - y.p[0]).abs() + (x.p[1] - y.p[1]).abs())
                    .sum();
                assert!(
                    diff > 3.0,
                    "{:?} and {:?} look alike ({diff})",
                    ActivityState::ALL[a],
                    ActivityState::ALL[b]
                );
            }
        }
    }

    #[test]
    fn thinking_sweeps_the_ring_and_turns_the_sphere() {
        let lit_angle = |t: f64| {
            let f = frame(Thinking, t);
            let (j, _) = f
                .iter()
                .take(RING)
                .enumerate()
                .max_by(|a, b| a.1.lit.total_cmp(&b.1.lit))
                .unwrap();
            ring_angle(j)
        };
        assert!(lit_angle(0.4) > lit_angle(0.0));
        assert!(lit_angle(0.8) > lit_angle(0.4));
        for d in &frame(Thinking, 0.3)[RING..] {
            assert!((xy_len(d).hypot(d.p[2]) - 0.46).abs() < 1e-9, "{d:?}");
        }
        assert_ne!(frame(Thinking, 0.3)[RING].p, frame(Thinking, 0.6)[RING].p);
    }

    #[test]
    fn notes_write_then_hold() {
        let lit = |t: f64| {
            frame(Notes, t)[RING..]
                .iter()
                .filter(|d| d.lit > 0.0)
                .count()
        };
        assert!(lit(0.3) < lit(1.0));
        assert!(lit(1.0) < lit(1.9));
        assert_eq!(lit(1.9), GLYPH);
        assert_eq!(lit(2.5), GLYPH);
        assert!(lit(2.7) < GLYPH, "starts over");
    }

    #[test]
    fn reading_band_moves_down() {
        let band_y = |t: f64| {
            let f = frame(Reading, t);
            let (sum, n) = f[RING..]
                .iter()
                .filter(|d| d.lit > 0.5)
                .fold((0.0, 0), |(s, n), d| (s + d.p[1], n + 1));
            assert!(n > 0, "nothing lit at t={t}");
            sum / n as f64
        };
        assert!(band_y(0.2) > band_y(1.0));
        assert!(band_y(1.0) > band_y(1.9));
    }

    #[test]
    fn searching_is_a_lit_magnifier() {
        for t in [0.0, 1.2] {
            let f = frame(Searching, t);
            assert!(f[RING..].iter().all(|d| d.lit == 1.0));
            // The lens is a circle of radius .24 round a centre that moves with t.
            let lens: Vec<&Dot> = (RING..N_POINTS)
                .filter(|&i| gu(i) < 0.74)
                .map(|i| &f[i])
                .collect();
            let n = lens.len() as f64;
            let cx = lens.iter().map(|d| d.p[0]).sum::<f64>() / n;
            let cy = lens.iter().map(|d| d.p[1]).sum::<f64>() / n;
            for d in lens {
                assert!(
                    ((d.p[0] - cx).hypot(d.p[1] - cy) - 0.24).abs() < 0.02,
                    "{d:?}"
                );
            }
        }
        assert_ne!(frame(Searching, 0.0)[RING].p, frame(Searching, 0.6)[RING].p);
    }

    #[test]
    fn editing_types_then_blinks() {
        let typed = |t: f64| {
            frame(Editing, t)[RING + 11..]
                .iter()
                .filter(|d| d.lit > 0.0)
                .count()
        };
        assert!(typed(0.2) < typed(0.8));
        assert!(typed(0.8) < typed(1.6));
        let caret_on = |t: f64| frame(Editing, t)[RING].s > 0.0;
        assert!(caret_on(1.0));
        assert!(caret_on(1.7));
        assert!(!caret_on(1.9), "the caret blinks off");
        assert!(caret_on(2.1));
    }

    #[test]
    fn running_has_two_arcs() {
        let f = frame(Running, 0.3);
        let lit: Vec<usize> = (0..RING).filter(|&j| f[j].lit > 0.3).collect();
        assert!(lit.len() >= 6, "{lit:?}");
        // Lit dots fall into two groups half a ring apart.
        let a = ring_angle(lit[0]);
        assert!(
            lit.iter().any(|&j| (ring_angle(j) - a - PI).abs() < 0.5),
            "{lit:?}"
        );
        assert!(f[RING..].iter().all(|d| d.lit == 1.0));
    }

    #[test]
    fn waiting_breathes() {
        let ring_lit = |t: f64| frame(Waiting, t)[0].lit;
        assert!(ring_lit(0.45) > ring_lit(1.35));
        assert!((0.25..=0.8).contains(&ring_lit(0.0)));
        assert!(frame(Waiting, 0.0)[RING..].iter().all(|d| d.lit == 1.0));
    }

    #[test]
    fn done_draws_a_check_then_holds() {
        let settled = frame(Done, 0.95);
        for t in [1.0, crate::SETTLE, 5.0] {
            assert_eq!(frame(Done, t), settled, "t={t}");
        }
        for d in &settled[RING..] {
            assert_eq!(d.lit, 1.0, "check dot unlit: {d:?}");
            assert!(xy_len(d) < 0.62);
        }
        let mid = frame(Done, 0.45);
        assert_eq!(mid[RING].lit, 1.0);
        assert!(mid[N_POINTS - 1].lit < 1.0);
    }

    #[test]
    fn error_shakes_then_holds() {
        let settled = frame(Error, 1.25);
        for t in [crate::SETTLE, 5.0, 60.0] {
            assert_eq!(frame(Error, t), settled, "t={t}");
        }
        // Mid-shake the whole frame is shifted sideways relative to where it settles.
        let cx = |f: &[Dot]| f.iter().map(|d| d.p[0]).sum::<f64>() / f.len() as f64;
        assert!((cx(&frame(Error, 0.36)) - cx(&settled)).abs() > 0.05);
        assert_eq!(settled[0].p, ring_at(0), "the ring is back in place");
        assert!(settled[RING..].iter().any(|d| xy_len(d) < 0.02));
    }

    #[test]
    fn offline_has_a_gap_and_a_turning_arrow() {
        let f = frame(Offline, 0.0);
        let gap = f.iter().take(RING).filter(|d| d.s == 0.0).count();
        assert!((4..=10).contains(&gap), "{gap} gap dots");
        assert_eq!(f[0].s, 0.0, "the gap is at 12 o'clock");
        for d in &f[RING..RING + 60] {
            assert!((xy_len(d) - 0.4).abs() < 1e-9, "{d:?}");
        }
        assert_ne!(frame(Offline, 0.0)[RING].p, frame(Offline, 0.5)[RING].p);
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
