//! A scripted agent turn for demos: the [`AgentEvent`]s of a turn with one approval prompt, and
//! the pill it produces through [`ActivityTracker`] and [`Debouncer`].
//!
//! A renderer can replay [`demo_turn`] on its clock (feed each event at its time, then show
//! [`Debouncer::shown`]); `cargo run -p rozu-activity --example replay` prints the timeline.

use rozu_core::{AgentEvent, ApprovalResolution, PendingApproval, TurnOutcome};

use crate::debounce::Debouncer;
use crate::events::{Activity, ActivityTracker};

/// One turn: think, read a file, run `cargo test` behind an approval prompt (approved after two
/// seconds), patch a file, then finish.
pub fn demo_turn() -> Vec<(f64, AgentEvent)> {
    let started = |id: &str, name: &str, args: &str| AgentEvent::ToolCallStarted {
        id: id.into(),
        name: name.into(),
        args_json: args.into(),
    };
    let finished = |id: &str, name: &str| AgentEvent::ToolCallFinished {
        id: id.into(),
        name: name.into(),
        output: "ok".into(),
        is_error: false,
    };
    vec![
        (0.0, AgentEvent::TextDelta("Let me look. ".into())),
        (0.5, started("c1", "read_file", r#"{"path":"README.md"}"#)),
        (0.6, finished("c1", "read_file")),
        (0.7, AgentEvent::TextDelta("Running the tests. ".into())),
        (1.2, started("c2", "shell", r#"{"command":"cargo test"}"#)),
        (
            1.25,
            AgentEvent::ApprovalPending(PendingApproval {
                request_id: "c2".into(),
                tool: "shell".into(),
                summary: "cargo test".into(),
                provider: "openai".into(),
            }),
        ),
        (
            3.25,
            AgentEvent::ApprovalResolved {
                request_id: "c2".into(),
                resolution: ApprovalResolution::Approved,
            },
        ),
        (5.0, finished("c2", "shell")),
        (
            5.4,
            started("c3", "apply_patch", r#"{"path":"src/lib.rs"}"#),
        ),
        (5.5, finished("c3", "apply_patch")),
        (5.6, AgentEvent::TextDelta("Fixed.".into())),
        (6.2, AgentEvent::TurnFinished(TurnOutcome::Completed)),
    ]
}

/// The pill over `script`: each time it changes and what it shows then (`None`: idle).
pub fn replay(script: &[(f64, AgentEvent)]) -> Vec<(f64, Option<Activity>)> {
    let mut tracker = ActivityTracker::new();
    let mut pill = Debouncer::new();
    let mut out = Vec::new();
    for (t, event) in script {
        if let Some(due) = pill.deadline().filter(|due| due <= t)
            && pill.poll(due)
        {
            out.push((due, pill.shown().cloned()));
        }
        tracker.apply(&event.into());
        if pill.offer(tracker.current(), *t) {
            out.push((*t, pill.shown().cloned()));
        }
    }
    if let Some(due) = pill.deadline()
        && pill.poll(due)
    {
        out.push((due, pill.shown().cloned()));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ActivityState::*;

    #[test]
    fn the_demo_turn_waits_for_approval_and_ends_done() {
        let timeline = replay(&demo_turn());
        let states: Vec<_> = timeline
            .iter()
            .map(|(t, a)| ((t * 1000.0).round() as u32, a.as_ref().map(|a| a.state)))
            .collect();
        assert_eq!(
            states,
            [
                (0, Some(Thinking)),
                (500, Some(Reading)),
                // The read's result came back after 0.1s: Thinking waits out the dwell.
                (800, Some(Thinking)),
                (1200, Some(Running)),
                (1250, Some(Waiting)),
                (3250, Some(Running)),
                (5000, Some(Thinking)),
                (5400, Some(Editing)),
                // Editing's result and the text that follows are coalesced into one Thinking.
                (5700, Some(Thinking)),
                (6200, Some(Done)),
            ]
        );
        let waiting = timeline[4].1.as_ref().unwrap();
        assert_eq!(waiting.detail.as_deref(), Some("Running cargo test"));
        assert_eq!(waiting.hint, Some("Y / N"));
        assert_eq!(
            timeline[9].1.as_ref().unwrap().detail.as_deref(),
            Some("1 file")
        );
    }
}
