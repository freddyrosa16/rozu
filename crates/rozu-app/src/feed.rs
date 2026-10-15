//! One chat as the app shows it: the user's messages, the agent's text, a step per tool call and
//! a card per approval prompt, folded from [`AgentEvent`]s.

use std::time::Instant;

use rozu_activity::{ActivityEvent, ActivityState, ActivityTracker, from_agent_event};
use rozu_core::{
    AgentEvent, ApprovalRequest, ApprovalResolution, ContentPart, Role, Session, TurnOutcome,
};

pub enum Item {
    User(String),
    Text(String),
    Step(Step),
    Ask(Ask),
    Note(String),
}

pub struct Step {
    pub id: String,
    pub state: ActivityState,
    pub label: String,
    /// Set when the step ends: its confirmed result or an interruption.
    pub done: Option<String>,
    pub failed: bool,
    started: Instant,
}

pub struct Ask {
    pub request: ApprovalRequest,
    pub answer: Option<&'static str>,
}

#[derive(Default)]
pub struct Feed {
    pub items: Vec<Item>,
    pub running: bool,
    tracker: ActivityTracker,
    thinking: Option<usize>,
}

impl Feed {
    /// A saved chat as it was: the user's messages, the replies and a finished step per tool
    /// call. Compaction summaries and the system prompt are not shown.
    pub fn from_session(session: &Session) -> Self {
        let mut feed = Self::default();
        for message in &session.messages {
            for part in &message.content {
                match (message.role, part) {
                    (Role::User, ContentPart::Text { text }) => {
                        feed.items.push(Item::User(text.clone()));
                    }
                    (Role::Assistant, ContentPart::Text { text }) if !text.trim().is_empty() => {
                        match feed.items.last_mut() {
                            Some(Item::Text(t)) => {
                                t.push_str("\n\n");
                                t.push_str(text);
                            }
                            _ => feed.items.push(Item::Text(text.clone())),
                        }
                    }
                    (
                        Role::Assistant,
                        ContentPart::ToolCall {
                            id,
                            name,
                            args_json,
                        },
                    ) => {
                        let ev = AgentEvent::ToolCallStarted {
                            id: id.clone(),
                            name: name.clone(),
                            args_json: args_json.clone(),
                        };
                        let activity = from_agent_event(&ev);
                        let state = activity
                            .as_ref()
                            .map_or(ActivityState::Thinking, |a| a.state);
                        let label = activity.map_or(name.clone(), |a| a.label);
                        feed.items.push(Item::Step(Step::new(id, state, &label)));
                    }
                    (
                        _,
                        ContentPart::ToolResult {
                            call_id,
                            output,
                            is_error,
                        },
                    ) => {
                        if let Some(step) = feed.items.iter_mut().rev().find_map(|i| match i {
                            Item::Step(s) if s.id == *call_id => Some(s),
                            _ => None,
                        }) {
                            step.finish(output, *is_error);
                        }
                    }
                    _ => {}
                }
            }
        }
        feed.end_turn();
        if !feed.items.is_empty() {
            feed.tracker.apply(&ActivityEvent::TurnCompleted);
        }
        feed
    }

    pub fn send(&mut self, text: String) {
        self.items.push(Item::User(text));
        self.thinking = Some(self.items.len());
        self.items.push(Item::Step(Step::new(
            "",
            ActivityState::Thinking,
            "Thinking",
        )));
        self.tracker.apply(&ActivityEvent::TextDelta);
        self.running = true;
    }

    /// The thread's orb: what the agent is doing, Done after a turn, Offline before the first.
    pub fn state(&self) -> ActivityState {
        self.tracker
            .current()
            .map_or(ActivityState::Offline, |a| a.state)
    }

    pub fn apply(&mut self, event: AgentEvent) {
        self.tracker.apply(&(&event).into());
        match event {
            AgentEvent::TextDelta(text) => {
                self.end_thinking();
                match self.items.last_mut() {
                    Some(Item::Text(t)) => t.push_str(&text),
                    _ => self.items.push(Item::Text(text)),
                }
            }
            AgentEvent::ToolCallStarted { id, .. } => {
                self.end_thinking();
                let activity = self.tracker.running_tool().expect("tool just started");
                self.items
                    .push(Item::Step(Step::new(&id, activity.state, &activity.label)));
            }
            AgentEvent::ToolCallFinished {
                id,
                is_error,
                output,
                ..
            } => {
                if let Some(step) = self.step(&id) {
                    step.finish(&output, is_error);
                }
            }
            AgentEvent::ApprovalResolved {
                request_id,
                resolution,
            } => {
                for item in &mut self.items {
                    if let Item::Ask(ask) = item
                        && ask.request.call_id == request_id
                        && ask.answer.is_none()
                    {
                        ask.answer = Some(match resolution {
                            ApprovalResolution::Approved => "Allowed",
                            ApprovalResolution::Denied => "Denied",
                            ApprovalResolution::Expired => "Expired",
                        });
                    }
                }
            }
            AgentEvent::TurnFinished(outcome) => {
                self.end_turn();
                match outcome {
                    TurnOutcome::Completed => {}
                    TurnOutcome::Cancelled => self.items.push(Item::Note("Stopped".into())),
                    TurnOutcome::MaxSteps => self
                        .items
                        .push(Item::Note("Stopped at the step limit for this turn".into())),
                }
            }
            _ => {}
        }
    }

    pub fn ask(&mut self, request: ApprovalRequest) {
        self.items.push(Item::Ask(Ask {
            request,
            answer: None,
        }));
    }

    pub fn fail(&mut self, message: String, offline: bool) {
        self.tracker.apply(&ActivityEvent::TurnFailed { offline });
        self.end_turn();
        self.items.push(Item::Note(message));
    }

    fn step(&mut self, id: &str) -> Option<&mut Step> {
        self.items.iter_mut().rev().find_map(|item| match item {
            Item::Step(s) if s.id == id && s.done.is_none() => Some(s),
            _ => None,
        })
    }

    fn end_thinking(&mut self) {
        if let Some(index) = self.thinking.take()
            && let Some(Item::Step(step)) = self.items.get_mut(index)
        {
            step.done = Some(format!(
                "Thought for {}s",
                step.started.elapsed().as_secs().max(1)
            ));
        }
    }

    fn end_turn(&mut self) {
        self.end_thinking();
        self.running = false;
        for item in &mut self.items {
            match item {
                Item::Step(s) if s.done.is_none() => {
                    s.failed = true;
                    s.done = Some(format!("Interrupted: {}", s.label));
                }
                Item::Ask(a) if a.answer.is_none() => a.answer = Some("Expired"),
                _ => {}
            }
        }
    }
}

impl Step {
    fn finish(&mut self, output: &str, is_error: bool) {
        self.failed = is_error;
        self.done = Some(if is_error {
            output.lines().next().unwrap_or("Failed").to_owned()
        } else {
            past(&self.label)
        });
    }

    fn new(id: &str, state: ActivityState, label: &str) -> Self {
        let (id, label, done) = (id.to_owned(), label.to_owned(), None);
        Self {
            id,
            state,
            label,
            done,
            failed: false,
            started: Instant::now(),
        }
    }
}

/// "Reading PLAN.md" → "Read PLAN.md".
fn past(label: &str) -> String {
    let label = label.trim_end_matches('…');
    [
        ("Reading", "Read"),
        ("Searching", "Searched"),
        ("Editing", "Edited"),
        ("Running", "Ran"),
    ]
    .iter()
    .find_map(|(now, then)| label.strip_prefix(now).map(|rest| format!("{then}{rest}")))
    .unwrap_or_else(|| label.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tool(id: &str, name: &str, args: &str) -> AgentEvent {
        AgentEvent::ToolCallStarted {
            id: id.into(),
            name: name.into(),
            args_json: args.into(),
        }
    }

    fn finished(id: &str, is_error: bool) -> AgentEvent {
        AgentEvent::ToolCallFinished {
            id: id.into(),
            name: "x".into(),
            output: "boom\nmore".into(),
            is_error,
        }
    }

    fn steps(feed: &Feed) -> Vec<(ActivityState, Option<&str>)> {
        feed.items
            .iter()
            .filter_map(|i| match i {
                Item::Step(s) => Some((s.state, s.done.as_deref())),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_turn_becomes_steps_and_text() {
        let mut feed = Feed::default();
        assert_eq!(feed.state(), ActivityState::Offline);
        feed.send("hi".into());
        assert_eq!(feed.state(), ActivityState::Thinking);
        feed.apply(tool("a", "read_file", r#"{"path":"PLAN.md"}"#));
        assert_eq!(feed.state(), ActivityState::Reading);
        feed.apply(finished("a", false));
        feed.apply(tool("b", "shell", r#"{"command":"cargo test"}"#));
        feed.apply(finished("b", true));
        feed.apply(AgentEvent::TextDelta("All ".into()));
        feed.apply(AgentEvent::TextDelta("good".into()));
        feed.apply(AgentEvent::TurnFinished(TurnOutcome::Completed));
        let s = steps(&feed);
        assert!(s[0].1.unwrap().starts_with("Thought for"));
        assert_eq!(s[1], (ActivityState::Reading, Some("Read PLAN.md")));
        assert_eq!(s[2], (ActivityState::Running, Some("boom")));
        assert!(matches!(feed.items.last(), Some(Item::Text(t)) if t == "All good"));
        assert!(!feed.running);
        assert_eq!(feed.state(), ActivityState::Done);
    }

    #[test]
    fn ending_a_turn_never_claims_an_unfinished_tool_succeeded() {
        for outcome in [
            TurnOutcome::Cancelled,
            TurnOutcome::Completed,
            TurnOutcome::MaxSteps,
        ] {
            let mut feed = Feed::default();
            feed.send("hi".into());
            feed.apply(tool("read", "read_file", r#"{"path":"README.md"}"#));
            feed.apply(finished("read", false));
            feed.apply(tool("write", "write_file", r#"{"path":"cancelled.txt"}"#));
            feed.apply(AgentEvent::TurnFinished(outcome));

            assert_eq!(steps(&feed)[1].1, Some("Read README.md"));
            assert_eq!(
                steps(&feed)[2].1,
                Some("Interrupted: Editing cancelled.txt")
            );
            assert!(feed.items.iter().any(|item| matches!(item,
                Item::Step(step) if step.id == "write" && step.failed)));
            assert!(!feed.running);
            if outcome == TurnOutcome::Cancelled {
                assert!(matches!(feed.items.last(), Some(Item::Note(n)) if n == "Stopped"));
            }
        }
    }

    #[test]
    fn a_failed_turn_marks_its_unfinished_tool_interrupted() {
        let mut feed = Feed::default();
        feed.send("hi".into());
        feed.apply(tool("a", "shell", r#"{"command":"sleep 10"}"#));
        feed.fail("connection lost".into(), true);
        assert_eq!(steps(&feed)[1].1, Some("Interrupted: Running sleep 10"));
        assert!(matches!(&feed.items[2], Item::Step(step) if step.failed));
        assert!(!feed.running);
    }

    #[test]
    fn a_saved_chat_is_shown_as_it_was() {
        use rozu_core::Message;
        let mut session = Session::with_system_prompt("sys");
        session.messages.push(Message::user("fix it"));
        session.messages.push(Message::new(
            Role::Assistant,
            vec![
                ContentPart::Text {
                    text: "Looking.".into(),
                },
                ContentPart::ToolCall {
                    id: "a".into(),
                    name: "read_file".into(),
                    args_json: r#"{"path":"PLAN.md"}"#.into(),
                },
                ContentPart::ToolCall {
                    id: "b".into(),
                    name: "shell".into(),
                    args_json: r#"{"command":"cargo test"}"#.into(),
                },
                ContentPart::ToolCall {
                    id: "c".into(),
                    name: "write_file".into(),
                    args_json: r#"{"path":"cancelled.txt"}"#.into(),
                },
            ],
        ));
        session
            .messages
            .push(Message::tool_result("a", "text", false));
        session
            .messages
            .push(Message::tool_result("b", "boom\nmore", true));
        session.messages.push(Message::assistant("Done."));
        let feed = Feed::from_session(&session);
        assert!(matches!(&feed.items[0], Item::User(t) if t == "fix it"));
        assert!(matches!(&feed.items[1], Item::Text(t) if t == "Looking."));
        assert_eq!(
            steps(&feed),
            [
                (ActivityState::Reading, Some("Read PLAN.md")),
                (ActivityState::Running, Some("boom")),
                (
                    ActivityState::Editing,
                    Some("Interrupted: Editing cancelled.txt")
                ),
            ]
        );
        assert!(matches!(&feed.items[4], Item::Step(step) if step.failed));
        assert!(matches!(feed.items.last(), Some(Item::Text(t)) if t == "Done."));
        assert!(!feed.running);
        assert_eq!(feed.state(), ActivityState::Done);
        assert_eq!(
            Feed::from_session(&Session::new()).state(),
            ActivityState::Offline
        );
    }

    #[test]
    fn live_and_reloaded_tools_keep_the_same_results() {
        use rozu_core::Message;

        let mut live = Feed::default();
        let mut session = Session::new();
        live.send("go".into());
        session.messages.push(Message::user("go"));
        for (id, name, args, result) in [
            (
                "read",
                "read_file",
                r#"{"path":"PLAN.md"}"#,
                Some(("contents", false)),
            ),
            (
                "run",
                "shell",
                r#"{"command":"cargo test"}"#,
                Some(("boom\nmore", true)),
            ),
            ("empty-error", "shell", "{}", Some(("", true))),
            ("unknown", "mystery", "{}", Some(("ok", false))),
            ("bad-args", "write_file", "not json", Some(("ok", false))),
            (
                "unfinished",
                "write_file",
                r#"{"path":"cancelled.txt"}"#,
                None,
            ),
        ] {
            live.apply(tool(id, name, args));
            session.messages.push(Message::new(
                Role::Assistant,
                vec![ContentPart::ToolCall {
                    id: id.into(),
                    name: name.into(),
                    args_json: args.into(),
                }],
            ));
            if let Some((output, is_error)) = result {
                live.apply(AgentEvent::ToolCallFinished {
                    id: id.into(),
                    name: name.into(),
                    output: output.into(),
                    is_error,
                });
                session
                    .messages
                    .push(Message::tool_result(id, output, is_error));
            }
        }
        live.apply(AgentEvent::TurnFinished(TurnOutcome::Completed));
        let reloaded = Feed::from_session(&session);
        let results = |feed: &Feed| {
            feed.items
                .iter()
                .filter_map(|item| match item {
                    Item::Step(step) if !step.id.is_empty() => Some((
                        step.id.clone(),
                        step.state,
                        step.label.clone(),
                        step.done.clone(),
                        step.failed,
                    )),
                    _ => None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(results(&live), results(&reloaded));
        assert_eq!(
            results(&reloaded)
                .iter()
                .map(|row| (row.3.as_deref().unwrap(), row.4))
                .collect::<Vec<_>>(),
            [
                ("Read PLAN.md", false),
                ("boom", true),
                ("Failed", true),
                ("Thinking", false),
                ("Edited", false),
                ("Interrupted: Editing cancelled.txt", true),
            ]
        );
    }

    #[test]
    fn tool_rows_keep_their_activity_while_approvals_overlap() {
        use rozu_core::PendingApproval;

        let mut feed = Feed::default();
        feed.send("read it".into());
        for id in ["a", "b"] {
            feed.apply(AgentEvent::ApprovalPending(PendingApproval {
                request_id: id.into(),
                tool: "read_file".into(),
                summary: "read".into(),
                provider: "test".into(),
            }));
        }
        feed.apply(tool("file", "read_file", r#"{"path":"README.md"}"#));
        assert_eq!(feed.state(), ActivityState::Waiting);
        assert!(matches!(feed.items.last(), Some(Item::Step(step))
            if step.state == ActivityState::Reading && step.label == "Reading README.md"));
        feed.apply(AgentEvent::ApprovalResolved {
            request_id: "a".into(),
            resolution: ApprovalResolution::Approved,
        });
        assert_eq!(feed.state(), ActivityState::Waiting);
        feed.apply(AgentEvent::ApprovalResolved {
            request_id: "b".into(),
            resolution: ApprovalResolution::Expired,
        });
        assert_eq!(feed.state(), ActivityState::Reading);
    }

    #[test]
    fn thinking_finishes_once_per_turn_after_resume_cancel_and_failure() {
        use rozu_core::Message;

        let mut session = Session::new();
        session.messages.push(Message::user("old question"));
        session.messages.push(Message::assistant("old answer"));
        let mut feed = Feed::from_session(&session);
        feed.send("cancel".into());
        feed.apply(AgentEvent::TurnFinished(TurnOutcome::Cancelled));
        feed.send("fail".into());
        feed.fail("offline".into(), true);
        assert_eq!(feed.state(), ActivityState::Offline);
        feed.send("retry".into());
        feed.apply(AgentEvent::TextDelta("one".into()));
        let completed: Vec<_> = steps(&feed)
            .into_iter()
            .map(|(_, done)| done.unwrap().to_owned())
            .collect();
        assert_eq!(completed.len(), 3);
        feed.apply(AgentEvent::TextDelta("two".into()));
        assert_eq!(
            steps(&feed)
                .into_iter()
                .map(|(_, done)| done.unwrap())
                .collect::<Vec<_>>(),
            completed
        );
        assert!(matches!(&feed.items[0], Item::User(text) if text == "old question"));
        assert!(matches!(&feed.items[1], Item::Text(text) if text == "old answer"));
        assert!(matches!(feed.items.last(), Some(Item::Text(text)) if text == "onetwo"));
    }

    #[test]
    fn unknown_tools_and_malformed_arguments_keep_their_fallback_labels() {
        for (name, args, state, label) in [
            ("mystery", "{}", ActivityState::Thinking, "Thinking…"),
            ("write_file", "not json", ActivityState::Editing, "Editing…"),
            ("read_file", "[]", ActivityState::Reading, "Reading…"),
            ("shell", "{", ActivityState::Running, "Running…"),
        ] {
            let mut feed = Feed::default();
            feed.send("go".into());
            feed.apply(tool("call", name, args));
            assert!(
                matches!(feed.items.last(), Some(Item::Step(step))
                if step.state == state && step.label == label),
                "{name}: {args}"
            );
        }
    }

    #[test]
    fn past_tense() {
        assert_eq!(past("Editing src/a.rs"), "Edited src/a.rs");
        assert_eq!(past("Running…"), "Ran");
        assert_eq!(past("mcp_tool"), "mcp_tool");
    }
}
