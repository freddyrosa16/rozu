//! Per-state point generators, ported 1:1 from `docs/design/v7.3/reference/v7.3-orb.js`.
//!
//! Every state is the same spherical cloud of [`N_POINTS`] dots: dot `i` has a Fibonacci-sphere
//! direction `d_i` and a phase `φ_i = (t/T + hash i) mod 1`, and states differ only in how the
//! flow behaves. The arithmetic (including JS's truncating `%`) is kept as in the reference so
//! samples match it at the same `t`.
//!
//! Thinking, Reading, Editing and Running are unchanged from v7.2. Searching, Notes, Waiting,
//! Done, Error and Offline are the v7.3 redesigns: one bold lit cue against faded context dots
//! (the reference's `C` helper, `cue` here).

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

/// The v7.3 cue helper `C(p, on, sizeOn, sizeOff, alphaOff)`: cue dots are lit at full α and
/// `size_on`; context dots are unlit, `size_off` and faded to `alpha_off`.
fn cue(p: Vec3, on: bool, size_on: f64, size_off: f64, alpha_off: f64) -> Dot {
    if on {
        dot(p, 1.0, size_on, 1.0)
    } else {
        dot(p, 0.0, size_off, alpha_off)
    }
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
        // Scan lens: a bright lit cap orbits the rim like a lens sweeping; the rest flows in,
        // faded.
        ActivityState::Searching => {
            let ph = (t / 2.4 + h) % 1.0;
            let mut p = view(sc(dir(i), 1.0 - 0.5 * ph * ph), t, 0.2);
            let a = TAU * t / 2.4;
            let c0 = [0.78 * a.cos(), 0.5 * a.sin(), 0.45];
            let cm = hypot3(c0);
            let c = [c0[0] / cm, c0[1] / cm, c0[2] / cm];
            let m = len_or_one(p);
            let on = (p[0] * c[0] + p[1] * c[1] + p[2] * c[2]) / m > 0.86;
            if on {
                p[0] *= 1.06;
                p[1] *= 1.06;
            }
            cue(p, on, 1.35, 0.5, 0.25)
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
        // Ink stroke: a lit comet stroke writes round a tilted latitude, its tail fading; the
        // cloud drifts out, faded.
        ActivityState::Notes => {
            let d = dir(i);
            let ph = (t / 3.0 + h) % 1.0;
            let p = view(sc(d, 0.7 + 0.3 * ph), t, 0.15);
            let lon = d[2].atan2(d[0]);
            let lag = ((TAU * t / 1.5 - lon) % TAU + TAU) % TAU;
            let band = (d[1] - 0.45 * lon.sin()).abs() < 0.26;
            let on = band && lag < 4.4;
            let mut q = cue(p, on, 1.25 - 0.3 * lag / 4.4, 0.45, 0.18);
            if on {
                q.a = 1.0 - 0.35 * lag / 4.4;
            }
            q
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
        // Pause bars: two lit vertical bands on a frozen cloud that breathes.
        ActivityState::Waiting => {
            let ph = h * 0.85;
            let b = (TAU * t / 2.6).sin();
            let p = view(sc(dir(i), (1.0 - 0.5 * ph * ph) * (1.0 + 0.06 * b)), 0.0, 0.0);
            let on = (p[0].abs() - 0.32).abs() < 0.12 && p[1].abs() < 0.7;
            let mut q = cue(p, on, 1.3, 0.5, 0.22);
            if on {
                q.lit = 0.7 + 0.3 * (b * 0.5 + 0.5);
            }
            q
        }
        // Halo: the cloud settles on the shell; after one outward ripple its silhouette rim
        // lights as a bright halo.
        ActivityState::Done => {
            let d = dir(i);
            let front = 1.0 - 2.0 * cl(t / 1.1);
            let rip = if t < 1.3 {
                0.12 * (-((d[1] - front) / 0.18).powi(2)).exp()
            } else {
                0.0
            };
            let p = view(sc(d, 0.84 + rip), t, 0.12);
            let on = 1.0 - p[2].abs() / len_or_one(p) > 0.6;
            cue(p, on || rip > 0.06, 1.3, 0.55, 0.3)
        }
        // Break: the flow stalls, then a lit third is flung out once into a ragged outer ring;
        // the core stays dim.
        ActivityState::Error => {
            let d = dir(i);
            let ph = h * 0.85;
            let r0 = 0.55 - 0.3 * ph * ph;
            let out = hash(i + 7) < 0.38;
            let sct = if t < 0.18 {
                0.0
            } else {
                spr_o((t - 0.18) / 0.5)
            };
            let r = if out {
                r0 + (0.6 + 0.15 * hash(i + 3)) * sct
            } else {
                r0 * (1.0 - 0.1 * sct)
            };
            cue(view(sc(d, r), 0.0, 0.0), out, 1.3, 0.55, 0.28)
        }
        // Broken ring: a lit vertical ring with a dark gap; a dim spark sputters across the gap
        // on .3s ticks; the rest is a faint sparse cloud.
        ActivityState::Offline => {
            let k = (t / 0.3).floor();
            let burst = if k % 4.0 < 2.0 { (t % 0.3) / 0.3 } else { 1.0 };
            let ph = ((k + burst) * 0.07 + h) % 1.0;
            let p = view(sc(dir(i), 1.0 - 0.4 * ph * ph), t, 0.18);
            let ring = p[0].abs() < 0.15;
            let gap = p[1] > -0.05 && p[1] < 0.5;
            let spark = gap && ring && k % 4.0 == 1.0;
            if !ring && i % 3 != 0 {
                return dot(p, 0.0, 0.0, 1.0);
            }
            cue(p, ring && (!gap || spark), 1.25, 0.5, 0.2)
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

    /// Thinking, Reading, Editing and Running are the liked v7.2 states and must not change in
    /// v7.3. These values were captured from the v7.2 generators before the v7.3 port.
    #[test]
    fn liked_four_are_unchanged_from_v7_2() {
        use ActivityState::*;
        #[rustfmt::skip]
        let points: [(ActivityState, usize, f64, Vec3, f64, f64, f64); 28] = [
            (Thinking, 0, 0.0, [0.027229338167722324, 0.2002356779112461, 0.061940153643105345], 1.0, 1.3, 1.0),
            (Thinking, 7, 0.37, [-0.20617695716085296, 0.6555120824840291, -0.07300789561602741], 0.0, 0.6, 0.4),
            (Thinking, 59, 1.23, [-0.2841845318677268, -0.0319623534947184, 0.11198606262838304], 1.0, 1.3, 1.0),
            (Thinking, 119, 4.9, [0.05185013168818953, -0.42088829455088655, -0.10710287757121442], 0.0, 0.6, 0.4),
            (Thinking, 3, 0.5, [0.03923522799964504, 0.12433155698947855, 0.07077803936582296], 1.0, 1.3, 1.0),
            (Thinking, 42, 0.06, [0.48606045410082455, 0.11091179978121796, 0.15746482220710084], 0.0, 0.6, 0.4),
            (Thinking, 88, 2.71, [-0.3377442997046392, -0.2930258814473795, 0.17530492742743348], 0.0, 0.6, 0.4),
            (Reading, 0, 0.0, [0.10983082899294276, 0.8076601187841367, 0.24983855210394762], 0.0, 0.7, 0.55),
            (Reading, 7, 0.37, [-0.2578108148797743, 0.9414422417923811, -0.12840043765079817], 0.0, 0.7, 0.55),
            (Reading, 59, 1.23, [-0.9181146213641125, -0.014784031715579585, 0.073769324530391], 0.0, 0.7, 0.55),
            (Reading, 119, 4.9, [-0.006477417415013221, -0.8775184001034685, -0.15154867517369075], 0.0, 0.7, 0.55),
            (Reading, 3, 0.5, [0.19999255434221594, 0.700986979998921, 0.42884032133587097], 0.0, 0.7, 0.55),
            (Reading, 42, 0.06, [0.8731091580682094, 0.19610149004152072, 0.2964518639978432], 0.0, 0.7, 0.55),
            (Reading, 88, 2.71, [-0.7335675385864536, -0.3714212542145827, -0.1399017866807512], 0.0, 0.7, 0.55),
            (Editing, 0, 0.0, [0.07658376860561912, 0.5631720730517805, 0.17420953696271455], 0.2, 1.15, 1.0),
            (Editing, 7, 0.37, [-0.14580724455402252, 0.6291939712179894, -0.09923565863064213], 0.2, 1.15, 1.0),
            (Editing, 59, 1.23, [-0.5905406250431272, 0.04498027941590709, -0.12832069931215434], 0.2, 1.15, 1.0),
            (Editing, 119, 4.9, [-0.07181190414977058, -0.557295911999208, -0.1500170798926752], 0.2, 1.15, 1.0),
            (Editing, 3, 0.5, [0.12140266034855403, 0.4865632233258464, 0.3162629506552611], 0.2, 1.15, 1.0),
            (Editing, 42, 0.06, [0.46399829651790775, 0.10257035102281994, 0.16483648966758083], 0.8, 0.0, 1.0),
            (Editing, 88, 2.71, [-0.3143592354695935, -0.1339102859199668, -0.3242886982744833], 0.8, 1.15, 1.0),
            (Running, 0, 0.0, [-0.020195611735898625, 0.14909907081355758, 0.25420068070032703], 1.0, 1.35, 1.0),
            (Running, 7, 0.37, [-0.22173185320901168, 0.022673206198646573, 0.6349080212900531], 0.0, 0.45, 0.18),
            (Running, 59, 1.23, [0.4521350514174245, 0.41823252312633535, -0.18130907496346158], 1.0, 1.35, 1.0),
            (Running, 119, 4.9, [-0.020293043049231612, -0.29264623766424697, -0.48280971725261645], 1.0, 1.35, 1.0),
            (Running, 3, 0.5, [0.14299591644922277, 0.5016884272549142, 0.5811655241257057], 0.0, 0.45, 0.18),
            (Running, 42, 0.06, [0.4401138616034285, 0.5248274855615337, -0.015948097269680633], 1.0, 1.35, 1.0),
            (Running, 88, 2.71, [-0.821148248482257, -0.16872640933502628, -0.410158834443344], 0.0, 0.45, 0.18),
        ];
        for (state, i, t, p, lit, s, a) in points {
            let d = generate(state, i, t);
            let got = [d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a];
            let want = [p[0], p[1], p[2], lit, s, a];
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 1e-12, "{state:?} i={i} t={t}: {d:?}");
            }
        }

        // Whole-cloud checksums: Σ value·(1 + i/7) over all 120 dots.
        #[rustfmt::skip]
        let sums: [(ActivityState, f64, [f64; 6]); 16] = [
            (Thinking, 0.0, [7.351368077579412, -149.1781766309305, -32.38737006387468, 630.7142857142856, 1125.5000000000002, 834.4285714285714]),
            (Thinking, 0.37, [28.516399631150318, -127.34101620198454, -33.50686809059141, 604.4285714285713, 1107.1, 818.6571428571428]),
            (Thinking, 1.23, [1.160716440958479, -152.63502514901342, -45.36402347346189, 671.4285714285714, 1154.0000000000002, 858.8571428571431]),
            (Thinking, 4.9, [-6.299094305682155, -136.20611076498952, -46.731825410350005, 601.8571428571425, 1105.3000000000004, 817.1142857142858]),
            (Reading, 0.0, [0.4331893591776619, -300.43745229612034, -89.21884994815005, 246.0, 921.0, 737.6999999999999]),
            (Reading, 0.37, [2.909786153492934, -298.5533083330603, -90.25283307472081, 183.57142857142856, 889.7857142857143, 709.6071428571429]),
            (Reading, 1.23, [-5.447036796486336, -296.68631040203604, -94.79314563382081, 160.00000000000003, 878.0, 699.0000000000001]),
            (Reading, 4.9, [5.205240253293128, -293.36398766234777, -95.9204811597193, 228.85714285714286, 912.4285714285716, 729.9857142857144]),
            (Editing, 0.0, [0.10950432326276238, -204.1705395158404, -61.35015879186418, 228.0, 990.8071428571426, 1140.0]),
            (Editing, 0.37, [1.1205155062208119, -203.4882487289253, -61.737297514602815, 228.0, 990.8071428571426, 1140.0]),
            (Editing, 1.23, [-2.1460103899665226, -196.93049294938095, -61.376497823597376, 228.0, 990.8071428571426, 1140.0]),
            (Editing, 4.9, [0.10613148594148192, -188.57524576978972, -58.91160531753327, 228.0, 990.8071428571426, 1140.0]),
            (Running, 0.0, [10.108377701169927, -118.58742905742412, -168.3824970863756, 389.7142857142857, 863.7428571428569, 524.765714285714]),
            (Running, 0.37, [-36.09462198837662, -61.61176721001049, -160.1908450345903, 389.7142857142857, 863.7428571428569, 524.765714285714]),
            (Running, 1.23, [36.35348044651302, -117.9833376099485, -167.3649004490568, 389.7142857142857, 863.7428571428569, 524.765714285714]),
            (Running, 4.9, [16.386718454929202, -104.16193039339737, -179.6586398077674, 389.7142857142857, 863.7428571428569, 524.765714285714]),
        ];
        for (state, t, want) in sums {
            let mut got = [0.0; 6];
            for i in 0..N_POINTS {
                let d = generate(state, i, t);
                for (g, v) in got.iter_mut().zip([d.p[0], d.p[1], d.p[2], d.lit, d.s, d.a]) {
                    *g += v * (1.0 + i as f64 / 7.0);
                }
            }
            for (g, w) in got.iter().zip(want) {
                assert!((g - w).abs() < 1e-9, "{state:?} t={t}: {got:?}");
            }
        }
    }

    const REDESIGNED: [ActivityState; 6] = [
        ActivityState::Searching,
        ActivityState::Notes,
        ActivityState::Waiting,
        ActivityState::Done,
        ActivityState::Error,
        ActivityState::Offline,
    ];

    fn frame(state: ActivityState, t: f64) -> Vec<Dot> {
        (0..N_POINTS).map(|i| generate(state, i, t)).collect()
    }

    /// v7.3: each redesigned state's still frame shows one bold lit cue (α 1, large) against
    /// faded context dots (small, α ≤ .3), in the full cloud and in the 22px subsample.
    #[test]
    fn redesigned_still_frames_show_the_cue() {
        for state in REDESIGNED {
            let t = state.reduced_motion_time();
            let dots = frame(state, t);
            let is_cue = |d: &Dot| d.lit >= 0.7 && d.a >= 0.6 && d.s >= 0.95;
            let is_faded = |d: &Dot| d.lit == 0.0 && d.a <= 0.3 && d.s > 0.0 && d.s <= 0.55;
            let cue_n = dots.iter().filter(|d| is_cue(d)).count();
            let faded_n = dots.iter().filter(|d| is_faded(d)).count();
            assert!(cue_n >= 4, "{state:?}: only {cue_n} cue dots at t={t}");
            assert!(faded_n >= 4, "{state:?}: only {faded_n} faded dots at t={t}");
            // Every drawn dot is one or the other.
            for d in dots.iter().filter(|d| d.s > 0.02) {
                assert!(is_cue(d) || is_faded(d), "{state:?}: {d:?}");
            }
            let small_cue = dots
                .iter()
                .enumerate()
                .filter(|(i, d)| ((i >> 1) + i) % 2 == 0 && is_cue(d))
                .count();
            assert!(small_cue >= 2, "{state:?}: cue lost at 22px");
        }
    }

    #[test]
    fn redesigned_cue_sizes_match_the_spec() {
        for state in REDESIGNED {
            for k in 0..60 {
                for d in frame(state, k as f64 * 0.1) {
                    if d.lit > 0.0 {
                        // Notes' comet tail tapers from 1.25× down to .95×.
                        assert!(d.s >= 0.95 && d.s <= 1.4, "{state:?}: {d:?}");
                    } else if d.s > 0.0 {
                        assert!(d.s >= 0.45 && d.s <= 0.55, "{state:?}: {d:?}");
                        assert!(d.a >= 0.18 && d.a <= 0.3, "{state:?}: {d:?}");
                    }
                }
            }
        }
    }

    #[test]
    fn offline_shows_at_most_half_the_dots() {
        for k in 0..100 {
            let t = k as f64 * 0.05;
            let shown = frame(ActivityState::Offline, t)
                .iter()
                .filter(|d| d.s > 0.02)
                .count();
            assert!(shown * 2 <= N_POINTS, "t={t}: {shown} dots shown");
            assert!(shown > 0);
        }
    }

    #[test]
    fn offline_spark_fills_the_gap_on_tick_two() {
        // Lit dots inside the ring's gap (x ≈ 0, y ∈ (−.05, .5)).
        let lit_in_gap = |t: f64| {
            frame(ActivityState::Offline, t)
                .iter()
                .filter(|d| d.lit > 0.0 && d.p[1] > -0.05 && d.p[1] < 0.5)
                .count()
        };
        // Tick 2 of every 4 (.3–.6s, then every 1.2s) is the spark; the other ticks leave the
        // gap dark.
        for cycle in [0.0, 1.2, 2.4] {
            assert!(lit_in_gap(cycle + 0.45) > 0, "no spark at {}", cycle + 0.45);
            for tick in [0.15, 0.75, 1.05] {
                assert_eq!(lit_in_gap(cycle + tick), 0, "gap lit at {}", cycle + tick);
            }
        }
    }

    #[test]
    fn error_settles() {
        // The fling spring reaches 1 at .18 + 1.6·.5 = .98s, and Error has no yaw.
        let settled = frame(ActivityState::Error, 0.98);
        for t in [1.0, crate::SETTLE, 5.0, 60.0] {
            assert_eq!(frame(ActivityState::Error, t), settled, "t={t}");
        }
        // The flung third (lit) sits outside the dim core.
        let (out, core): (Vec<&Dot>, Vec<&Dot>) = settled.iter().partition(|d| d.lit > 0.0);
        let n = out.len() as f64 / N_POINTS as f64;
        assert!((0.25..0.5).contains(&n), "flung fraction {n}");
        let min_out = out.iter().map(|d| hypot3(d.p)).fold(f64::MAX, f64::min);
        let max_core = core.iter().map(|d| hypot3(d.p)).fold(0.0, f64::max);
        assert!(min_out > max_core, "{min_out} vs {max_core}");
    }

    #[test]
    fn done_settles() {
        // After the ripple (1.3s) every dot sits on the .84 shell and only the silhouette
        // rim is lit; the frame policy stops drawing from SETTLE.
        for t in [crate::SETTLE, 1.6, 5.0] {
            let dots = frame(ActivityState::Done, t);
            for d in &dots {
                assert!((hypot3(d.p) - 0.84).abs() < 1e-12, "t={t}: {d:?}");
                let rim = 1.0 - d.p[2].abs() / hypot3(d.p) > 0.6;
                assert_eq!(d.lit > 0.0, rim, "t={t}: {d:?}");
            }
        }
        // Mid-ripple, some dots are pushed out and lit.
        assert!(
            frame(ActivityState::Done, 0.5)
                .iter()
                .any(|d| hypot3(d.p) > 0.9 && d.lit > 0.0)
        );
    }

    #[test]
    fn springs_end_at_one() {
        assert_eq!(crit(0.0), 0.0);
        assert_eq!(crit(1.6), 1.0);
        assert!(crit(1.59) > 0.999);
        assert_eq!(spr_o(1.6), 1.0);
    }
}
