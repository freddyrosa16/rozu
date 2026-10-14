//! The state timeline: morphs between states (handoff §4) and reduced-motion frames (§5).

use crate::generate::{Dot, OrbSize, Vec3, breath, crit, generate, hash, pulse};
use crate::state::{ActivityState, Tone};

/// Morph duration (s): a critically damped spring, ω = 7.
pub const MORPH: f64 = 0.6;
/// The morph stagger (s) across the whole cloud: dot `i` starts `hash(i) · STAGGER` late, so a
/// change dissolves across the globe instead of sweeping it ring by ring, whatever the size.
pub const STAGGER: f64 = 0.3;
/// Mid-morph z lift toward the camera: `+LIFT·sin(πq)`.
const LIFT: f64 = 0.55;
/// Brighten pulse (s) on every change; in reduced motion, the cross-fade and α bump.
pub const PULSE: f64 = 0.15;
/// The time (s) after a change at which every point has finished its morph.
pub const MORPH_END: f64 = 1.6 * MORPH + STAGGER;
/// Done and Error are settled (and stop drawing) this long (s) after they start.
pub const SETTLE: f64 = 1.3;

/// Most segments kept; older ones are dropped (only reachable by many changes inside 1.3s).
const MAX_SEGMENTS: usize = 8;

/// One blended dot, ready for [`crate::project`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Sample {
    pub p: Vec3,
    pub lit: f64,
    pub s: f64,
    pub a: f64,
    pub tone: Tone,
    pub dim: bool,
}

impl Sample {
    fn from_dot(d: Dot, state: ActivityState) -> Self {
        Sample {
            p: d.p,
            lit: d.lit,
            s: d.s,
            a: d.a,
            tone: state.tone(),
            dim: state.dim(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Segment {
    state: ActivityState,
    start: f64,
    /// Set on Done and Error that follow Waiting: when that Waiting started. Instead of
    /// morphing (a dip through the small dim globe), the breath keeps going (on Waiting's clock) and
    /// fills up to the full bright globe over [`SETTLE`], in the new tone.
    breath_from: Option<f64>,
}

impl Segment {
    /// The segment's own dot `i` at `t`: what it generates, before any morph.
    fn dot(self, size: OrbSize, i: usize, t: f64) -> Dot {
        match self.breath_from {
            Some(waiting_start) => {
                let e = crit(1.6 * (t - self.start) / SETTLE);
                let b = 0.5 + 0.5 * (std::f64::consts::TAU * (t - waiting_start) / 1.8).sin();
                breath(size, i, b + (1.0 - b) * e)
            }
            None => generate(size, self.state, i, t - self.start),
        }
    }

    /// The reduced-motion still frame's dot `i`.
    fn still(self, size: OrbSize, i: usize) -> Dot {
        match self.breath_from {
            Some(_) => breath(size, i, 1.0),
            None => generate(size, self.state, i, self.state.reduced_motion_time()),
        }
    }
}

/// A timeline of states. Times are seconds on the caller's animation clock (which should stop
/// while the orb is paused, so it resumes with no jump).
#[derive(Debug, Clone)]
pub struct Orb {
    size: OrbSize,
    segments: Vec<Segment>,
    reduced_motion: bool,
}

impl Orb {
    /// A [`OrbSize::Large`] orb.
    pub fn new(state: ActivityState, start: f64) -> Self {
        Orb::with_size(OrbSize::Large, state, start)
    }

    pub fn with_size(size: OrbSize, state: ActivityState, start: f64) -> Self {
        Orb {
            size,
            segments: vec![Segment {
                state,
                start,
                breath_from: None,
            }],
            reduced_motion: false,
        }
    }

    pub fn size(&self) -> OrbSize {
        self.size
    }

    /// Dots per sample: [`OrbSize::dots`].
    pub fn dots(&self) -> usize {
        self.size.dots()
    }

    /// The most recently set state.
    pub fn state(&self) -> ActivityState {
        self.last().state
    }

    /// When the current state was set.
    pub fn state_start(&self) -> f64 {
        self.last().start
    }

    pub fn reduced_motion(&self) -> bool {
        self.reduced_motion
    }

    /// Static frames per state, and a 150ms cross-fade instead of the morph.
    pub fn set_reduced_motion(&mut self, on: bool) {
        self.reduced_motion = on;
    }

    /// Morphs to `state` starting at `t`. Setting the current state again does nothing (the label
    /// may change without a morph). Returns whether a change was made.
    pub fn set_state(&mut self, state: ActivityState, t: f64) -> bool {
        if state == self.state() {
            return false;
        }
        // Only the previous segment's live sample is needed once its own morph has finished.
        let prev = self.last();
        let breath_from =
            (state.plays_once() && prev.state == ActivityState::Waiting).then_some(prev.start);
        if t - prev.start >= MORPH_END {
            let keep = self.segments.len() - 1;
            self.segments.drain(..keep);
        }
        if self.segments.len() >= MAX_SEGMENTS {
            self.segments.remove(0);
        }
        self.segments.push(Segment {
            state,
            start: t,
            breath_from,
        });
        true
    }

    /// Whether the current play-once state (Done, Error) has finished; nothing changes after
    /// this until the next state, so the renderer can stop.
    pub fn is_settled(&self, t: f64) -> bool {
        let cur = self.last();
        cur.state.plays_once() && t - cur.start >= SETTLE.max(MORPH_END)
    }

    /// Whether frames at `t` differ from frames a moment later.
    pub fn is_animating(&self, t: f64) -> bool {
        if self.reduced_motion {
            self.segments.len() > 1 && t - self.state_start() < PULSE
        } else {
            !self.is_settled(t)
        }
    }

    /// All [`Orb::dots`] dots at `t`.
    pub fn sample(&self, t: f64) -> Vec<Sample> {
        let mut out = Vec::with_capacity(self.dots());
        self.sample_into(t, &mut out);
        out
    }

    /// [`Orb::sample`] into a reused buffer.
    pub fn sample_into(&self, t: f64, out: &mut Vec<Sample>) {
        out.clear();
        let k = self.segment_at(t);
        if self.reduced_motion {
            self.sample_reduced(k, t, out);
        } else {
            self.sample_segment(k, t, out);
        }
    }

    fn last(&self) -> Segment {
        *self.segments.last().expect("an orb always has a segment")
    }

    /// The segment active at `t` (the first one if `t` is before every start).
    fn segment_at(&self, t: f64) -> usize {
        let mut k = 0;
        while k + 1 < self.segments.len() && self.segments[k + 1].start <= t {
            k += 1;
        }
        k
    }

    fn sample_segment(&self, k: usize, t: f64, out: &mut Vec<Sample>) {
        let cur = self.segments[k];
        let tin = t - cur.start;
        let (size, n) = (self.size, self.dots());
        if k == 0 {
            out.extend((0..n).map(|i| Sample::from_dot(cur.dot(size, i, t), cur.state)));
            return;
        }
        if cur.breath_from.is_some() {
            // No morph: the breath carries on and fills up, only the tone changes (with the pulse).
            let boost = pulse(tin);
            out.extend((0..n).map(|i| {
                let mut s = Sample::from_dot(cur.dot(size, i, t), cur.state);
                s.lit = (s.lit + boost).clamp(0.0, 1.0);
                s
            }));
            return;
        }
        // The previous state keeps generating, so nothing restarts. If it was itself still
        // morphing (an interrupt), blend from its live sample; otherwise this is exactly the
        // reference's `STATES[pv.state].gen(i, T - pv.start)`.
        let pv = self.segments[k - 1];
        let mut from = Vec::with_capacity(n);
        if t - pv.start < MORPH_END {
            self.sample_segment(k - 1, t, &mut from);
        } else {
            from.extend((0..n).map(|i| Sample::from_dot(pv.dot(size, i, t), pv.state)));
        }
        let boost = pulse(tin);
        for (i, a) in from.iter().enumerate() {
            let b = cur.dot(size, i, t);
            let q = crit((tin - hash(i) * STAGGER) / MORPH);
            let lift = LIFT * (std::f64::consts::PI * q).sin();
            let late = q > 0.5;
            out.push(Sample {
                p: [
                    a.p[0] + (b.p[0] - a.p[0]) * q,
                    a.p[1] + (b.p[1] - a.p[1]) * q,
                    a.p[2] + (b.p[2] - a.p[2]) * q + lift,
                ],
                lit: (a.lit + (b.lit - a.lit) * q + boost).clamp(0.0, 1.0),
                s: a.s + (b.s - a.s) * q,
                a: a.a + (b.a - a.a) * q,
                tone: if late { cur.state.tone() } else { a.tone },
                dim: if late { cur.state.dim() } else { a.dim },
            });
        }
    }

    /// Reduced motion: each state is one still frame. A change dissolves dot by dot from the old
    /// frame to the new one over 150ms (no dot moves) with a single α bump.
    fn sample_reduced(&self, k: usize, t: f64, out: &mut Vec<Sample>) {
        let cur = self.segments[k];
        let (size, n) = (self.size, self.dots());
        let still = |seg: Segment, i| Sample::from_dot(seg.still(size, i), seg.state);
        let tin = t - cur.start;
        if k == 0 || tin >= PULSE {
            out.extend((0..n).map(|i| still(cur, i)));
            return;
        }
        let pv = self.segments[k - 1];
        let x = (tin / PULSE).clamp(0.0, 1.0);
        let bump = 1.0 + 0.6 * (std::f64::consts::PI * x).sin();
        out.extend((0..n).map(|i| {
            let mut s = if hash(i + 101) < x {
                still(cur, i)
            } else {
                still(pv, i)
            };
            s.a *= bump;
            s
        }));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::MAX_DOTS;
    use crate::state::ActivityState::*;
    use std::fmt::Debug;

    fn close(a: &Sample, b: &Sample) -> bool {
        a.p.iter().zip(b.p).all(|(x, y)| (x - y).abs() < 1e-9)
            && (a.lit - b.lit).abs() < 1e-9
            && (a.s - b.s).abs() < 1e-9
            && (a.a - b.a).abs() < 1e-9
            && a.tone == b.tone
            && a.dim == b.dim
    }

    fn assert_close(got: &[Sample], want: &[Sample], ctx: impl Debug) {
        for (a, b) in got.iter().zip(want) {
            assert!(close(a, b), "{ctx:?}: {a:?} vs {b:?}");
        }
    }

    fn target(state: ActivityState, tin: f64) -> Vec<Sample> {
        (0..MAX_DOTS)
            .map(|i| Sample::from_dot(generate(OrbSize::Large, state, i, tin), state))
            .collect()
    }

    /// Done or Error after Waiting: no morph, the breath fills up in the new tone and holds, and
    /// the next change starts from that full globe.
    #[test]
    fn done_and_error_after_waiting_keep_the_breath() {
        for (to, tone) in [(Done, Tone::Success), (Error, Tone::Danger)] {
            let mut orb = Orb::new(Waiting, 0.0);
            orb.set_state(to, 3.0);
            for (a, b) in orb.sample(3.0).iter().zip(target(Waiting, 3.0)) {
                assert_eq!(a.p, b.p, "{to:?}");
                assert_eq!(a.tone, tone);
            }
            let full: Vec<Sample> = (0..MAX_DOTS)
                .map(|i| Sample::from_dot(breath(OrbSize::Large, i, 1.0), to))
                .collect();
            let t = 3.0 + SETTLE + 0.01;
            assert!(orb.is_settled(t));
            assert_close(&orb.sample(t), &full, to);
            orb.set_state(Thinking, 6.0);
            assert_close(&orb.sample(6.0), &full, to);

            let mut orb = Orb::new(Waiting, 0.0);
            orb.set_reduced_motion(true);
            orb.set_state(to, 3.0);
            assert_close(&orb.sample(4.0), &full, to);
        }
        let mut orb = Orb::new(Running, 0.0);
        orb.set_state(Error, 3.0);
        let t = 3.0 + MORPH_END + 0.01;
        assert_close(&orb.sample(t), &target(Error, t - 3.0), Error);
    }

    #[test]
    fn first_state_is_the_generator() {
        let s = Orb::new(Thinking, 0.0).sample(0.4);
        assert_eq!(s.len(), MAX_DOTS);
        assert_eq!(s, target(Thinking, 0.4));
    }

    #[test]
    fn morph_starts_at_the_previous_state() {
        let mut orb = Orb::new(Reading, 0.0);
        orb.set_state(Running, 2.0);
        // q = 0 at the change and the pulse is sin 0.
        assert_close(&orb.sample(2.0), &target(Reading, 2.0), Running);
    }

    #[test]
    fn morph_ends_at_the_target_state() {
        for (from, to) in [
            (Thinking, Done),
            (Running, Error),
            (Waiting, Offline),
            (Done, Searching),
        ] {
            let mut orb = Orb::new(from, 0.0);
            orb.set_state(to, 3.0);
            let t = 3.0 + MORPH_END + 0.01;
            assert_close(&orb.sample(t), &target(to, t - 3.0), (from, to));
        }
    }

    #[test]
    fn tone_flips_mid_morph() {
        let mut orb = Orb::new(Thinking, 0.0);
        orb.set_state(Done, 1.0);
        assert!(orb.sample(1.01).iter().all(|s| s.tone == Tone::Neutral));
        assert!(
            orb.sample(1.0 + MORPH_END)
                .iter()
                .all(|s| s.tone == Tone::Success)
        );
    }

    #[test]
    fn pulse_peaks_then_decays_by_150ms() {
        let mut orb = Orb::new(Reading, 0.0);
        orb.set_state(Searching, 5.0);
        // Dot 0 is about a fifth of the way through its morph at 75ms, with the .8 pulse peak on top.
        let base = generate(OrbSize::Large, Reading, 0, 5.075).lit;
        let peak = orb.sample(5.075)[0].lit;
        assert!(
            peak >= (base + 0.7).min(1.0) - 0.05,
            "peak {peak}, base {base}"
        );
        assert_eq!(pulse(PULSE), 0.0);
        assert_eq!(pulse(0.2), 0.0);
        assert!(pulse(0.149) < 0.02);
    }

    #[test]
    fn same_state_is_ignored() {
        let mut orb = Orb::new(Thinking, 0.0);
        assert!(!orb.set_state(Thinking, 1.0));
        assert_eq!(orb.state_start(), 0.0);
    }

    #[test]
    fn interrupt_continues_from_the_live_sample() {
        let mut orb = Orb::new(Thinking, 0.0);
        orb.set_state(Running, 1.0);
        let before = orb.sample(1.3);
        orb.set_state(Done, 1.3);
        assert_close(&before, &orb.sample(1.3), "jump at interrupt");
    }

    #[test]
    fn segments_are_pruned() {
        let mut orb = Orb::new(Thinking, 0.0);
        let cycle = |orb: &mut Orb, start: f64, step: f64| {
            for (k, s) in ActivityState::ALL
                .iter()
                .cycle()
                .skip(1)
                .take(40)
                .enumerate()
            {
                orb.set_state(*s, start + k as f64 * step);
            }
        };
        cycle(&mut orb, 0.0, 2.0);
        assert!(orb.segments.len() <= 2);
        cycle(&mut orb, 100.0, 0.01);
        assert!(orb.segments.len() <= MAX_SEGMENTS);
    }

    #[test]
    fn settles_after_done() {
        let mut orb = Orb::new(Running, 0.0);
        orb.set_state(Done, 1.0);
        assert!(!orb.is_settled(2.0));
        assert!(orb.is_settled(1.0 + SETTLE + 1e-9));
        assert!(!orb.is_animating(3.0));
        assert!(!Orb::new(Thinking, 0.0).is_settled(100.0));
    }

    #[test]
    fn reduced_motion_is_static() {
        for state in ActivityState::ALL {
            let mut orb = Orb::new(state, 0.0);
            orb.set_reduced_motion(true);
            let a = orb.sample(0.0);
            assert_eq!(a, orb.sample(1.7));
            assert_eq!(a, orb.sample(42.0));
            assert_eq!(a, target(state, state.reduced_motion_time()));
            assert!(!orb.is_animating(5.0));
        }
    }

    #[test]
    fn reduced_motion_change_never_moves_a_dot() {
        let mut orb = Orb::new(Thinking, 0.0);
        orb.set_reduced_motion(true);
        orb.set_state(Editing, 1.0);
        assert!(orb.is_animating(1.05));
        let old = target(Thinking, Thinking.reduced_motion_time());
        let new = target(Editing, Editing.reduced_motion_time());
        for k in 0..=15 {
            let s = orb.sample(1.0 + k as f64 * 0.01);
            for (i, d) in s.iter().enumerate() {
                assert!(d.p == old[i].p || d.p == new[i].p);
            }
        }
        let after = 1.0 + PULSE + 1e-9;
        assert_eq!(orb.sample(after), new);
        assert!(!orb.is_animating(after));
    }
}
