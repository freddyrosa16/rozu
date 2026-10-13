//! Per-state point generators, ported 1:1 from `docs/design/v7.2/reference/v7.2-orb.js`.
//!
//! Every state is the same spherical cloud of [`N_POINTS`] dots: dot `i` has a Fibonacci-sphere
//! direction `d_i` and a phase `φ_i = (t/T + hash i) mod 1`, and states differ only in how the
//! flow behaves. The arithmetic (including JS's truncating `%`) is kept as in the reference so
//! samples match it at the same `t`.

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
    /// The state's size multiplier (`s` in the reference). See
    /// [`crate::ProjectOptions::apply_state_size`] for how it's used.
    pub s: f64,
    /// Per-dot α multiplier (`a`; 1 when the reference leaves it unset).
    pub a: f64,
}

fn cl(v: f64) -> f64 {
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

/// Elastic overshoot spring (ζ≈.55), used by Error's scatter.
fn spr_o(u: f64) -> f64 {
    if u <= 0.0 {
        0.0
    } else if u >= 1.6 {
        1.0
    } else {
        1.0 - (-6.0 * u).exp() * ((9.0 * u).cos() + 0.65 * (9.0 * u).sin())
    }
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

fn ease(x: f64) -> f64 {
    x * x
}

fn hypot3(p: Vec3) -> f64 {
    (p[0] * p[0] + p[1] * p[1] + p[2] * p[2]).sqrt()
}

fn dot(p: Vec3, lit: f64, s: f64, a: f64) -> Dot {
    Dot { p, lit, s, a }
}

/// Dot `i` of `state` at `t` seconds into the state.
pub fn generate(state: ActivityState, i: usize, t: f64) -> Dot {
    let h = hash(i);
    match state {
        // Bright dense core: dots rush in, then linger at the core (φ^.55); core dots lit.
        ActivityState::Thinking => {
            let ph = (t / 1.6 + h) % 1.0;
            let r = 1.0 - 0.88 * ph.powf(0.55);
            let core = r < 0.42;
            dot(
                view(sc(dir(i), r), t, 0.5),
                if core { 1.0 } else { 0.0 },
                if core { 1.3 } else { 0.6 },
                if core { 1.0 } else { 0.4 },
            )
        }
        // Off-centre hot spot: the stream converges on a focal point upper right that drifts.
        ActivityState::Searching => {
            let ph = (t / 2.4 + h) % 1.0;
            let d = dir(i);
            let w = TAU * t / 2.4;
            let f = [0.78 + 0.05 * w.cos(), 0.62 + 0.05 * w.sin(), 0.1];
            let e = 0.96 * ph.powf(0.6);
            let p = [
                d[0] + (f[0] - d[0]) * e,
                d[1] + (f[1] - d[1]) * e,
                d[2] + (f[2] - d[2]) * e,
            ];
            let on = e > 0.6;
            dot(
                rot_x(p, 0.15),
                if on { 1.0 } else { 0.0 },
                if on { 1.3 } else { 0.55 },
                if on { 1.0 } else { 0.3 },
            )
        }
        // Tilted lit band across a gently inflowing cloud, sliding slowly.
        ActivityState::Reading => {
            let ph = (t / 2.2 + h) % 1.0;
            let p = view(sc(dir(i), 1.0 - 0.18 * ph), t, 0.25);
            let c = 0.25 * (TAU * t / 4.4).sin();
            let v = 0.75 * p[1] - 0.66 * p[0];
            let on = (v - c).abs() < 0.17;
            dot(
                p,
                if on { 1.0 } else { 0.0 },
                if on { 1.2 } else { 0.7 },
                if on { 1.0 } else { 0.55 },
            )
        }
        // Hollow centre, bright rim: slow outward shell flow; silhouette (fresnel) dots lit.
        ActivityState::Notes => {
            let ph = (t / 3.6 + h) % 1.0;
            let p = view(sc(dir(i), 0.78 + 0.22 * ph), t, 0.3);
            let rim = 1.0 - p[2].abs() / hypot3(p);
            let on = rim > 0.55;
            dot(
                p,
                if on { 1.0 } else { 0.0 },
                if on { 1.2 } else { 0.6 },
                if on { 1.0 } else { 0.3 },
            )
        }
        // Tight ball with a gap: a compressed ball clicking inward every 530ms; a caret-shaped
        // wedge on the right is empty (s = 0).
        ActivityState::Editing => {
            let u = (t % 0.53) / 0.53;
            let pulse = if u < 0.12 {
                u / 0.12
            } else {
                (-(u - 0.12) * 9.0).exp()
            };
            let ph = (t / 2.4 + h) % 1.0;
            let p = view(
                sc(dir(i), (0.66 - 0.08 * ph) * (1.0 - 0.22 * pulse)),
                0.0,
                0.0,
            );
            let gap = p[1].atan2(p[0]).abs() < 0.75;
            dot(
                p,
                if pulse > 0.5 { 0.8 } else { 0.2 },
                if gap { 0.0 } else { 1.15 },
                1.0,
            )
        }
        // Two spiral arms: fast inflow; the twist grows into the core so the lit arms wind.
        ActivityState::Running => {
            let ph = (t / 0.8 + h) % 1.0;
            let d = dir(i);
            let arm = (2.0 * d[2].atan2(d[0])).cos() > 0.55;
            let r = 1.0 - 0.86 * ph;
            let p = rot_x(rot_y(sc(d, r), 2.6 * ph + t * 2.5), 1.15);
            dot(
                p,
                if arm { 1.0 } else { 0.0 },
                if arm { 1.35 } else { 0.45 },
                if arm { 1.0 } else { 0.18 },
            )
        }
        // Paused: the flow freezes mid-stream and the frozen cloud breathes.
        ActivityState::Waiting => {
            let ph = h * 0.85;
            let b = (TAU * t / 2.6).sin();
            let r = (1.0 - 0.86 * ease(ph)) * (1.0 + 0.07 * b);
            dot(
                view(sc(dir(i), r), t, 0.08),
                0.35 + 0.4 * (b * 0.5 + 0.5),
                0.7 + 0.5 * ph,
                1.0,
            )
        }
        // Settle: a still shell with one outward ripple travelling pole to pole.
        ActivityState::Done => {
            let d = dir(i);
            let front = 1.0 - 2.0 * cl(t / 1.1);
            let rip = if t < 1.3 {
                0.13 * (-((d[1] - front) / 0.18).powi(2)).exp()
            } else {
                0.0
            };
            dot(view(sc(d, 0.82 + rip), t, 0.15), 1.0, 1.0, 1.0)
        }
        // Stall + scatter: the flow freezes, then scatters outward once and holds, uneven.
        ActivityState::Error => {
            let ph = h * 0.85;
            let r0 = 1.0 - 0.86 * ease(ph);
            let sct = if t < 0.18 {
                0.0
            } else {
                spr_o((t - 0.18) / 0.5)
            };
            let j = hash(i + 57) * 2.0 - 1.0;
            let d = dir(i);
            let p = sc(
                [
                    d[0] + 0.25 * j * d[2],
                    d[1] + 0.25 * hash(i + 11) - 0.12,
                    d[2] - 0.25 * j * d[0],
                ],
                r0 + (0.28 + 0.25 * hash(i + 3)) * sct,
            );
            dot(view(p, 0.0, 0.0), 1.0, 0.85, 1.0)
        }
        // Sputter: a sparse third of the dots advance in stuttering bursts and stall.
        ActivityState::Offline => {
            if i % 3 != 0 {
                return dot(view(sc(dir(i), 0.5), t, 0.2), 0.0, 0.0, 1.0);
            }
            let k = (t / 0.3).floor();
            let burst = if k % 4.0 < 2.0 { (t % 0.3) / 0.3 } else { 1.0 };
            let ph = ((k + burst) * 0.07 + h) % 1.0;
            dot(
                view(sc(dir(i), 1.0 - 0.86 * ease(ph)), t, 0.2),
                if ph > 0.75 { 0.6 } else { 0.0 },
                0.8 + 0.4 * ph,
                1.0,
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

    #[test]
    fn every_state_is_finite() {
        for state in ActivityState::ALL {
            for k in 0..200 {
                let t = k as f64 * 0.037;
                for i in 0..N_POINTS {
                    let d = generate(state, i, t);
                    let vals = [d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a];
                    assert!(
                        vals.iter().all(|v| v.is_finite()),
                        "{state:?} i={i} t={t}: {d:?}"
                    );
                    assert!(
                        d.p.iter().all(|v| v.abs() < 2.0),
                        "{state:?} escaped: {d:?}"
                    );
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
            assert!((hypot3(fib(i, N_POINTS)) - 1.0).abs() < 1e-12);
        }
    }

    #[test]
    fn springs_end_at_one() {
        assert_eq!(crit(0.0), 0.0);
        assert_eq!(crit(1.6), 1.0);
        assert!(crit(1.59) > 0.999);
        assert_eq!(spr_o(1.6), 1.0);
    }
}
