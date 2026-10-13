//! Pacing the pill so fast tool calls don't flicker it.
//!
//! The design docs have no pill-specific debounce, so this applies the nearest rule they do
//! have (handoff Appendix A, tool-call card: no spinner under 300ms) to working states, and
//! never delays anything the user has to see or act on:
//!
//! 1. Waiting, Done, Error and Offline show at once, and so does going idle. An approval prompt
//!    is never held back.
//! 2. A new label for the state already shown (e.g. `Reading a.rs` → `Reading b.rs`) shows at
//!    once: there's no morph, only text.
//! 3. Otherwise a state stays up at least [`MIN_DWELL`] before another replaces it. Changes
//!    offered in between are coalesced: only the latest one shows, at the end of the dwell,
//!    and states that would have flashed for less than that are skipped.
//!
//! Times are seconds on the host's clock (the same `T` as [`crate::Orb`]).

use crate::events::Activity;
use crate::state::ActivityState;

/// The shortest time (s) a working state stays up before another working state replaces it.
pub const MIN_DWELL: f64 = 0.3;

/// Sits between [`crate::ActivityTracker`] and the renderer: offer every change, show
/// [`Debouncer::shown`], and call [`Debouncer::poll`] at [`Debouncer::deadline`].
#[derive(Debug, Clone, Default)]
pub struct Debouncer {
    shown: Option<Activity>,
    /// When the shown state appeared.
    since: f64,
    /// The latest change held back by the dwell (`Some(None)`: go idle).
    queued: Option<Option<Activity>>,
}

impl Debouncer {
    pub fn new() -> Self {
        Self::default()
    }

    /// What the pill shows now; `None` when idle.
    pub fn shown(&self) -> Option<&Activity> {
        self.shown.as_ref()
    }

    /// When a held-back change is due; `None` when nothing is held back.
    pub fn deadline(&self) -> Option<f64> {
        self.queued.as_ref().map(|_| self.since + MIN_DWELL)
    }

    /// Offers the tracker's current activity at `now`. Returns whether [`Self::shown`] changed.
    pub fn offer(&mut self, next: Option<&Activity>, now: f64) -> bool {
        let next = next.cloned();
        if next == self.shown {
            self.queued = None;
            return false;
        }
        if self.shows_at_once(next.as_ref()) || now - self.since >= MIN_DWELL {
            self.show(next, now);
            true
        } else {
            self.queued = Some(next);
            false
        }
    }

    /// Shows the held-back change if its time has come. Returns whether [`Self::shown`] changed.
    pub fn poll(&mut self, now: f64) -> bool {
        match (self.deadline(), self.queued.take()) {
            (Some(due), Some(next)) if now >= due => {
                self.show(next, now);
                true
            }
            (_, queued) => {
                self.queued = queued;
                false
            }
        }
    }

    fn shows_at_once(&self, next: Option<&Activity>) -> bool {
        let Some(next) = next else {
            return true; // idle
        };
        match &self.shown {
            None => true,
            Some(shown) if shown.state == next.state => true,
            Some(_) => matches!(
                next.state,
                ActivityState::Waiting
                    | ActivityState::Done
                    | ActivityState::Error
                    | ActivityState::Offline
            ),
        }
    }

    fn show(&mut self, next: Option<Activity>, now: f64) {
        let same_state = matches!((&self.shown, &next), (Some(a), Some(b)) if a.state == b.state);
        if !same_state {
            self.since = now;
        }
        self.shown = next;
        self.queued = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ActivityState::*;

    fn a(state: ActivityState, label: &str) -> Activity {
        Activity::new(state, label)
    }

    #[test]
    fn working_states_dwell_and_coalesce() {
        let mut d = Debouncer::new();
        assert!(
            d.offer(Some(&a(Thinking, "Thinking…")), 0.0),
            "first state at once"
        );
        // A read that starts and finishes inside the dwell never shows.
        assert!(!d.offer(Some(&a(Reading, "Reading a.rs")), 0.1));
        assert!(!d.offer(Some(&a(Editing, "Editing a.rs")), 0.2));
        assert_eq!(d.deadline(), Some(MIN_DWELL));
        assert!(!d.poll(0.25));
        assert!(d.poll(0.3));
        assert_eq!(d.shown().unwrap().state, Editing);
        assert_eq!(d.deadline(), None);
        // Back to what's shown before the dwell ends: nothing to do.
        assert!(!d.offer(Some(&a(Running, "Running ls")), 0.35));
        assert!(!d.offer(Some(&a(Editing, "Editing a.rs")), 0.4));
        assert_eq!(d.deadline(), None);
    }

    #[test]
    fn prompts_results_and_idle_never_wait() {
        for state in [Waiting, Done, Error, Offline] {
            let mut d = Debouncer::new();
            d.offer(Some(&a(Running, "Running ls")), 0.0);
            assert!(d.offer(Some(&a(state, state.label())), 0.01), "{state:?}");
        }
        let mut d = Debouncer::new();
        d.offer(Some(&a(Running, "Running ls")), 0.0);
        assert!(d.offer(None, 0.01));
        assert!(d.shown().is_none());
    }

    #[test]
    fn label_changes_show_at_once_without_restarting_the_dwell() {
        let mut d = Debouncer::new();
        d.offer(Some(&a(Reading, "Reading a.rs")), 0.0);
        assert!(d.offer(Some(&a(Reading, "Reading b.rs")), 0.2));
        assert_eq!(d.shown().unwrap().label, "Reading b.rs");
        assert!(
            d.offer(Some(&a(Thinking, "Thinking…")), 0.3),
            "dwell counted from 0.0"
        );
    }

    #[test]
    fn leaving_waiting_respects_the_dwell() {
        let mut d = Debouncer::new();
        d.offer(Some(&a(Running, "Running ls")), 0.0);
        d.offer(Some(&a(Waiting, "Waiting for approval")), 1.0);
        assert!(!d.offer(Some(&a(Running, "Running ls")), 1.1));
        assert!(d.poll(1.3));
        assert_eq!(d.shown().unwrap().state, Running);
    }
}
