//! Per-orb point generators, ported 1:1 from `docs/design/v7.4/reference/v7.4-orb.js`.
//!
//! There are five orbs ([`OrbKind`]). Working and Waiting are the spherical cloud of
//! [`N_POINTS`] dots (dot `i` has a Fibonacci-sphere direction `d_i`); Done, Error and Offline
//! gather the same dots into a flat glyph facing the camera: a faint ring around a bold check or
//! cross, or a dashed ring. The arithmetic (including JS's truncating `%`) is kept as in the
//! reference so samples match it at the same `t`.

use std::f64::consts::{PI, TAU};

use crate::state::OrbKind;

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

fn dir(i: usize) -> Vec3 {
    fib(i, N_POINTS)
}

/// The shared camera: yaw `t·yaw` around y, then a fixed .3 rad tilt.
fn view(p: Vec3, t: f64, yaw: f64) -> Vec3 {
    rot_x(rot_y(p, t * yaw), 0.3)
}

fn sc(d: Vec3, r: f64) -> Vec3 {
    [d[0] * r, d[1] * r, d[2] * r]
}

fn hypot3(p: Vec3) -> f64 {
    (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt()
}

/// `Math.hypot(...p) || 1`.
fn len_or_one(p: Vec3) -> f64 {
    let m = hypot3(p);
    if m == 0.0 { 1.0 } else { m }
}

fn dot(p: Vec3, lit: f64, s: f64, a: f64) -> Dot {
    Dot { p, lit, s, a }
}

/// Glyph dots `0..RING` are the faint ring; the rest draw the check or the cross. Both are
/// contiguous index runs, so the 22px subsample keeps an even share of each.
pub(crate) const RING: usize = 44;
/// The glyph plane (toward the camera, so glyph dots are near and bright).
pub(crate) const GLYPH_Z: f64 = 0.3;
/// The glyph ring's radius.
pub(crate) const GLYPH_R: f64 = 0.86;

const CHECK: [[f64; 2]; 3] = [[-0.46, 0.02], [-0.14, -0.32], [0.48, 0.34]];
const CROSS_A: [[f64; 2]; 2] = [[-0.36, 0.36], [0.36, -0.36]];
const CROSS_B: [[f64; 2]; 2] = [[0.36, 0.36], [-0.36, -0.36]];

/// Dot `j` of `n` on the glyph ring, starting at angle `rot`.
fn ring_at(j: usize, n: usize, rot: f64) -> Vec3 {
    let a = TAU * j as f64 / n as f64 + rot;
    [GLYPH_R * a.cos(), GLYPH_R * a.sin(), GLYPH_Z]
}

/// The point at arc-length fraction `u` along a polyline.
fn along(pts: &[[f64; 2]], u: f64) -> [f64; 2] {
    let seg: Vec<f64> = pts
        .windows(2)
        .map(|w| (w[1][0] - w[0][0]).hypot(w[1][1] - w[0][1]))
        .collect();
    let mut d = u * seg.iter().sum::<f64>();
    for (k, &l) in seg.iter().enumerate() {
        if d <= l || k == seg.len() - 1 {
            let f = if l != 0.0 { cl(d / l) } else { 0.0 };
            let (a, b) = (pts[k], pts[k + 1]);
            return [a[0] + (b[0] - a[0]) * f, a[1] + (b[1] - a[1]) * f];
        }
        d -= l;
    }
    pts[0]
}

/// Dot `i` of `orb` at `t` seconds into it.
pub fn generate(orb: OrbKind, i: usize, t: f64) -> Dot {
    match orb {
        // Orbit: a slowly turning sphere with a lit comet (and a fainter twin opposite it)
        // circling it on a tilted path, the tails fading. A spinner at 22px; at 88px the comets
        // grow in front and shrink behind.
        OrbKind::Working => {
            let d = dir(i);
            let r = 0.9 + 0.05 * (TAU * t / 2.8 + 3.0 * d[1]).sin();
            let p = view(sc(d, r), t, 0.35);
            let m = len_or_one(p);
            // Orbit frame: the orbit is q.y ≈ 0, tilted toward the camera.
            let q = rot_x([p[0] / m, p[1] / m, p[2] / m], -0.75);
            let th = q[2].atan2(q[0]);
            let head = TAU * t / 1.4;
            let band = (-(q[1] / 0.5).powi(2)).exp();
            let mut lit: f64 = 0.0;
            for (off, gain) in [(0.0, 1.0), (PI, 0.55)] {
                let lag = ((head + off - th) % TAU + TAU) % TAU;
                let tail = if lag < 2.4 { 1.0 - lag / 2.4 } else { 0.0 };
                lit = lit.max(tail * tail.sqrt() * gain);
            }
            let lit = cl(1.35 * band * lit);
            dot(p, lit, 0.6 + 0.8 * lit, 0.38 + 0.62 * lit)
        }
        // Ping: the sphere stops turning and breathes; a lit ring pings outward from its face
        // every 1.8s, and the face glows faintly with the breath in between.
        OrbKind::Waiting => {
            let d = dir(i);
            let b = (TAU * t / 1.8).sin();
            let p = view(sc(d, 0.86 * (1.0 + 0.04 * b)), 0.0, 0.0);
            let m = len_or_one(p);
            let g = (p[2] / m).clamp(-1.0, 1.0).acos();
            let u = (t / 1.8) % 1.0;
            let front = u * 2.2;
            let ping = (-((g - front) / 0.28).powi(2)).exp() * (1.0 - u);
            let lit = cl(ping * 1.4);
            let glow = 0.25 * (b * 0.5 + 0.5) * (p[2] / m).max(0.0);
            dot(p, lit, 0.5 + 0.8 * lit, 0.32 + 0.68 * lit.max(glow))
        }
        // Check: a faint ring around a check mark that draws itself once, pops, then holds.
        OrbKind::Done => {
            if i < RING {
                return dot(ring_at(i, RING, PI / 2.0), 0.0, 0.5, 0.3);
            }
            let u = (i - RING) as f64 / (N_POINTS - RING - 1) as f64;
            let xy = along(&CHECK, u);
            let on = u <= cl((t - 0.25) / 0.45);
            let pop = if t > 0.7 && t < 0.95 {
                (PI * (t - 0.7) / 0.25).sin()
            } else {
                0.0
            };
            dot(
                [xy[0], xy[1], GLYPH_Z],
                if on { 1.0 } else { 0.25 },
                (if on { 1.15 } else { 0.6 }) * (1.0 + 0.2 * pop),
                if on { 1.0 } else { 0.35 },
            )
        }
        // Cross: a faint ring around a cross that shakes once, like a head saying no, then
        // holds.
        OrbKind::Error => {
            let st = t - 0.3;
            let dx = if st > 0.0 && st < 0.9 {
                0.13 * (-5.0 * st).exp() * (28.0 * st).sin()
            } else {
                0.0
            };
            if i < RING {
                let r = ring_at(i, RING, PI / 2.0);
                return dot([r[0] + dx, r[1], r[2]], 0.0, 0.5, 0.3);
            }
            let j = i - RING;
            let n = N_POINTS - RING;
            let half = n >> 1;
            let (u, stroke) = if j < half {
                (j as f64 / (half - 1) as f64, &CROSS_A)
            } else {
                ((j - half) as f64 / (n - half - 1) as f64, &CROSS_B)
            };
            let xy = along(stroke, u);
            dot([xy[0] + dx, xy[1], GLYPH_Z], 1.0, 1.15, 1.0)
        }
        // Dashed ring: six dashes turning slowly, the way a connection retries; each dash has a
        // brighter middle. Nothing inside it.
        OrbKind::Offline => {
            let a = TAU * i as f64 / N_POINTS as f64 - TAU * t / 2.4;
            let seg = (i as f64 / N_POINTS as f64 * 6.0) % 1.0;
            let r = [
                GLYPH_R * (a + PI / 2.0).cos(),
                GLYPH_R * (a + PI / 2.0).sin(),
                GLYPH_Z,
            ];
            if seg > 0.62 {
                return dot(r, 0.0, 0.0, 0.0);
            }
            let mid = (seg - 0.31).abs() < 0.08;
            dot(
                r,
                if mid { 0.6 } else { 0.0 },
                if mid { 0.9 } else { 0.65 },
                if mid { 0.8 } else { 0.45 },
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
    use std::collections::BTreeSet;

    use super::*;

    fn frame(orb: OrbKind, t: f64) -> Vec<Dot> {
        (0..N_POINTS).map(|i| generate(orb, i, t)).collect()
    }

    #[test]
    fn every_orb_is_finite() {
        for orb in OrbKind::ALL {
            for k in 0..200 {
                let t = k as f64 * 0.037;
                for d in frame(orb, t) {
                    let vals = [d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a];
                    assert!(vals.iter().all(|v| v.is_finite()), "{orb:?} t={t}: {d:?}");
                    assert!(d.p.iter().all(|v| v.abs() < 2.0), "{orb:?} escaped: {d:?}");
                    assert!((0.0..=1.0).contains(&d.lit), "{orb:?}: {d:?}");
                    assert!((0.0..=1.0).contains(&d.a), "{orb:?}: {d:?}");
                }
            }
        }
    }

    #[test]
    fn deterministic() {
        for orb in OrbKind::ALL {
            for i in [0, 1, 59, 119] {
                assert_eq!(generate(orb, i, 0.731), generate(orb, i, 0.731));
            }
        }
    }

    #[test]
    fn fib_is_on_unit_sphere() {
        for i in 0..N_POINTS {
            assert!((hypot3(fib(i, N_POINTS)) - 1.0).abs() < 1e-12);
        }
    }

    /// Whole-orb checksums, Σ value·(1 + i/7) over all 120 dots, from the JS reference
    /// (`docs/design/v7.4/reference/v7.4-orb.js`) at the same times.
    #[test]
    fn matches_the_reference() {
        #[rustfmt::skip]
        let sums: [(OrbKind, f64, [f64; 6]); 20] = [
            (OrbKind::Working, 0.0, [-0.3448376731681193, -276.1251884844912, -84.77270738267663, 149.73920259291538, 803.7913620743327, 526.0383056076075]),
            (OrbKind::Working, 0.37, [-0.25221519733937825, -275.60338860310753, -84.59655614044176, 177.978357343883, 826.3826858751066, 543.5465815532076]),
            (OrbKind::Working, 1.23, [-0.055589230341731355, -309.0926530893078, -94.88454550950327, 134.27273134863543, 791.4181850789089, 516.4490934361542]),
            (OrbKind::Working, 4.9, [0.6982836155551402, -303.85830868088766, -93.71352143139049, 167.87960905186551, 818.3036872414924, 537.2853576121566]),
            (OrbKind::Waiting, 0.0, [-0.3312412028439988, -281.84285564636644, -86.56566248834665, 22.990737558202508, 588.392590046562, 395.5516962153188]),
            (OrbKind::Waiting, 0.37, [-0.34397758206042495, -292.6798453026354, -89.89415270968956, 100.9822851561251, 650.7858281249, 448.37312052404076]),
            (OrbKind::Waiting, 1.23, [-0.3191370469943151, -271.5438052245762, -83.40239577818461, 121.25001865574683, 667.0000149245976, 448.20719126940185]),
            (OrbKind::Waiting, 4.9, [-0.3181928466568822, -270.7404144715007, -83.15564106582019, 108.79953469691075, 657.0396277575286, 438.95031795408215]),
            (OrbKind::Done, 0.0, [112.98222426186747, -10.621910649673456, 342.0, 245.67857142857144, 670.0928571428572, 394.7785714285714]),
            (OrbKind::Done, 0.37, [112.98222426186747, -10.621910649673456, 342.0, 377.4642857142858, 766.7357142857145, 508.99285714285713]),
            (OrbKind::Done, 1.23, [112.98222426186747, -10.621910649673456, 342.0, 960.8571428571429, 1194.5571428571432, 1014.6]),
            (OrbKind::Done, 4.9, [112.98222426186747, -10.621910649673456, 342.0, 960.8571428571429, 1194.5571428571432, 1014.6]),
            (OrbKind::Error, 0.0, [37.79088085580005, -28.108571428571462, 342.0, 960.8571428571429, 1194.5571428571432, 1014.6]),
            (OrbKind::Error, 0.37, [134.41513776078816, -28.108571428571462, 342.0, 960.8571428571429, 1194.5571428571432, 1014.6]),
            (OrbKind::Error, 1.23, [37.79088085580005, -28.108571428571462, 342.0, 960.8571428571429, 1194.5571428571432, 1014.6]),
            (OrbKind::Error, 4.9, [37.79088085580005, -28.108571428571462, 342.0, 960.8571428571429, 1194.5571428571432, 1014.6]),
            (OrbKind::Offline, 0.0, [281.50349996093166, -7.371428571428682, 342.0, 97.19999999999999, 496.7999999999999, 372.5999999999999]),
            (OrbKind::Offline, 0.37, [153.37035076077015, -236.16962962446348, 342.0, 97.19999999999999, 496.7999999999999, 372.5999999999999]),
            (OrbKind::Offline, 1.23, [-280.0573629976591, 29.4352149683911, 342.0, 97.19999999999999, 496.7999999999999, 372.5999999999999]),
            (OrbKind::Offline, 4.9, [270.0036346991289, -79.97872028669418, 342.0, 97.19999999999999, 496.7999999999999, 372.5999999999999]),
        ];
        for (orb, t, want) in sums {
            let mut got = [0.0; 6];
            for i in 0..N_POINTS {
                let d = generate(orb, i, t);
                for (g, v) in got
                    .iter_mut()
                    .zip([d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a])
                {
                    *g += v * (1.0 + i as f64 / 7.0);
                }
            }
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 1e-9, "{orb:?} t={t}: {got:?}");
            }
        }
    }

    /// Lit dots of Working, as (count, mean angle around the view axis).
    fn comet(t: f64) -> (usize, f64) {
        let lit: Vec<Dot> = frame(OrbKind::Working, t)
            .into_iter()
            .filter(|d| d.lit > 0.6)
            .collect();
        let (sx, sy) = lit
            .iter()
            .fold((0.0, 0.0), |(x, y), d| (x + d.p[0], y + d.p[1]));
        (lit.len(), sy.atan2(sx))
    }

    #[test]
    fn working_comet_is_always_visible_and_goes_round() {
        let mut angles = vec![];
        for k in 0..28 {
            let t = k as f64 * 0.05;
            let (n, a) = comet(t);
            assert!(n >= 4, "only {n} lit dots at t={t}");
            angles.push(a);
        }
        // Over one 1.4s loop the lit side covers every quadrant of the orb.
        let quadrants: BTreeSet<i32> = angles
            .iter()
            .map(|a| ((a + PI) / (PI / 2.0)).floor() as i32)
            .collect();
        assert_eq!(quadrants.len(), 4, "{angles:?}");
    }

    #[test]
    fn working_is_a_sphere() {
        for t in [0.0, 0.5, 1.3] {
            for d in frame(OrbKind::Working, t) {
                let r = hypot3(d.p);
                assert!((0.84..=0.96).contains(&r), "t={t}: {r}");
            }
        }
    }

    #[test]
    fn waiting_pings_from_the_face() {
        // Early in a ping the lit dots face the camera; later they're near the rim.
        let lit_z = |t: f64| {
            let lit: Vec<Dot> = frame(OrbKind::Waiting, t)
                .into_iter()
                .filter(|d| d.lit > 0.5)
                .collect();
            assert!(!lit.is_empty(), "nothing lit at t={t}");
            lit.iter().map(|d| d.p[2] / hypot3(d.p)).sum::<f64>() / lit.len() as f64
        };
        assert!(lit_z(0.1) > 0.8);
        assert!(lit_z(0.7) < lit_z(0.1) - 0.3);
        // The sphere doesn't turn: the same dot is at the same direction a loop later.
        let a = generate(OrbKind::Waiting, 17, 0.3);
        let b = generate(OrbKind::Waiting, 17, 0.3 + 1.8);
        assert!((a.p[0] - b.p[0]).abs() < 1e-9 && (a.p[1] - b.p[1]).abs() < 1e-9);
    }

    #[test]
    fn done_draws_a_check_then_holds() {
        let settled = frame(OrbKind::Done, 0.95);
        for t in [1.0, crate::SETTLE, 5.0] {
            assert_eq!(frame(OrbKind::Done, t), settled, "t={t}");
        }
        for (i, d) in settled.iter().enumerate() {
            if i < RING {
                assert!((hypot3([d.p[0], d.p[1], 0.0]) - GLYPH_R).abs() < 1e-9);
                assert_eq!(d.lit, 0.0);
            } else {
                assert_eq!(d.lit, 1.0, "check dot {i} unlit");
                assert!(hypot3([d.p[0], d.p[1], 0.0]) < 0.62);
            }
        }
        // Mid-draw, the start of the stroke is lit and its end isn't yet.
        let mid = frame(OrbKind::Done, 0.45);
        assert_eq!(mid[RING].lit, 1.0);
        assert!(mid[N_POINTS - 1].lit < 1.0);
    }

    #[test]
    fn error_shakes_then_holds() {
        let settled = frame(OrbKind::Error, 1.25);
        for t in [crate::SETTLE, 5.0, 60.0] {
            assert_eq!(frame(OrbKind::Error, t), settled, "t={t}");
        }
        // Centred once settled, displaced mid-shake.
        let cx = |f: &[Dot]| f.iter().map(|d| d.p[0]).sum::<f64>() / f.len() as f64;
        assert!(cx(&settled).abs() < 1e-9);
        assert!(cx(&frame(OrbKind::Error, 0.36)).abs() > 0.05);
        // The two strokes cross at the centre.
        assert!(
            settled[RING..]
                .iter()
                .any(|d| hypot3([d.p[0], d.p[1], 0.0]) < 0.02)
        );
    }

    #[test]
    fn offline_is_a_dashed_ring() {
        for t in [0.0, 0.7, 2.0] {
            let f = frame(OrbKind::Offline, t);
            let shown: Vec<&Dot> = f.iter().filter(|d| d.s > 0.02).collect();
            assert!(shown.len() * 3 < N_POINTS * 2 && shown.len() * 2 > N_POINTS);
            for d in shown {
                assert!((hypot3([d.p[0], d.p[1], 0.0]) - GLYPH_R).abs() < 1e-9);
            }
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
        assert_eq!(along(&CHECK, 0.0), CHECK[0]);
        let end = along(&CHECK, 1.0);
        assert!((end[0] - CHECK[2][0]).abs() < 1e-12 && (end[1] - CHECK[2][1]).abs() < 1e-12);
    }
}
