//! Agent events → activity state and label.
//!
//! [`ActivityEvent`] mirrors the parts of `rozu_core::AgentEvent` the pill needs, so the mapping
//! works (and is tested) without rozu-core. With the `core` feature, `AgentEvent` and
//! `ApprovalRequest` convert into it.
//!
//! `AgentEvent::Approval` is sent after the decision, so nothing in the event stream says "a
//! prompt is open". Hosts get Waiting from their `ApprovalGate`: send
//! [`ActivityEvent::ApprovalPending`] (`From<&ApprovalRequest>`) when the gate is asked.

use std::collections::BTreeSet;

use serde_json::Value;

use crate::state::ActivityState;

/// Longest file / query / command shown in a label.
const MAX_ARG_CHARS: usize = 32;

/// The agent activity the pill reacts to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActivityEvent {
    /// Model text is streaming.
    TextDelta,
    ToolStarted {
        id: String,
        name: String,
        args_json: String,
    },
    ToolFinished {
        id: String,
        name: String,
        is_error: bool,
    },
    /// The approval gate was asked about a call and is waiting for the user.
    ApprovalPending {
        call_id: String,
        tool: String,
    },
    /// A decision was made (by the user, a rule or the mode).
    ApprovalDecided {
        call_id: String,
        approved: bool,
    },
    TurnCompleted,
    TurnCancelled,
    /// The step cap was reached.
    TurnMaxSteps,
    /// Events that don't change the pill (usage, …).
    Other,
}

/// What the pill shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Activity {
    pub state: ActivityState,
    /// e.g. `Reading README.md`, `Running cargo test`.
    pub label: String,
    /// Extra `orbMeta` text, e.g. `2 files` after Done.
    pub detail: Option<String>,
    pub hint: Option<&'static str>,
}

impl Activity {
    pub fn new(state: ActivityState, label: impl Into<String>) -> Self {
        Activity {
            state,
            label: label.into(),
            detail: None,
            hint: state.hint(),
        }
    }
}

/// The state for a tool, by name. Matches whole words of the name (snake, kebab and camel case),
/// so `read_file`, `Grep`, `TodoWrite`, `MultiEdit` and `mcp__rozu__shell` all map. Unknown
/// tools are `None`.
pub fn tool_state(name: &str) -> Option<ActivityState> {
    let words = words(name);
    let has = |list: &[&str]| words.iter().any(|w| list.contains(&w.as_str()));
    // Order matters: `TodoWrite` is notes, not editing.
    if has(&["notes", "note", "todo", "todos", "memory", "plan"]) {
        Some(ActivityState::Notes)
    } else if has(&["grep", "search", "glob", "find", "rg"]) {
        Some(ActivityState::Searching)
    } else if has(&["write", "edit", "multiedit", "patch", "replace", "create"]) {
        Some(ActivityState::Editing)
    } else if has(&["shell", "bash", "exec", "run", "command", "terminal", "sh"]) {
        Some(ActivityState::Running)
    } else if has(&["read", "list", "ls", "view", "cat", "open", "fetch", "dir"]) {
        Some(ActivityState::Reading)
    } else {
        None
    }
}

/// Lower-case words of a tool name: split on non-alphanumerics and lower→upper case changes.
fn words(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_lower = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            prev_lower = false;
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if c.is_uppercase() && prev_lower && !cur.is_empty() {
            out.push(std::mem::take(&mut cur));
        }
        prev_lower = c.is_lowercase() || c.is_ascii_digit();
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Shortens `s` to its first line and at most [`MAX_ARG_CHARS`] chars.
fn clip(s: &str) -> String {
    let line = s.lines().next().unwrap_or("").trim();
    if line.chars().count() <= MAX_ARG_CHARS {
        line.to_string()
    } else {
        let head: String = line.chars().take(MAX_ARG_CHARS - 1).collect();
        format!("{}…", head.trim_end())
    }
}

/// A path for a label: as given if short, else its file name.
fn short_path(path: &str) -> String {
    if path.chars().count() <= MAX_ARG_CHARS {
        return path.to_string();
    }
    let name = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path);
    clip(name)
}

fn str_arg<'a>(args: &'a Value, keys: &[&str]) -> Option<&'a str> {
    keys.iter().find_map(|k| match args.get(*k)? {
        Value::String(s) if !s.trim().is_empty() => Some(s.as_str()),
        Value::Array(items) => items.first()?.as_str(),
        _ => None,
    })
}

fn path_arg(args: &Value) -> Option<&str> {
    str_arg(
        args,
        &[
            "path",
            "file_path",
            "file",
            "filename",
            "paths",
            "notebook_path",
        ],
    )
}

/// The pill label for a tool call: the state's verb plus the file, query or command when the
/// arguments have one.
fn tool_label(state: ActivityState, args_json: &str) -> String {
    let args: Value = serde_json::from_str(args_json).unwrap_or(Value::Null);
    let arg = match state {
        ActivityState::Reading | ActivityState::Editing => path_arg(&args).map(short_path),
        ActivityState::Searching => str_arg(&args, &["pattern", "query", "q", "regex", "glob"])
            .map(|q| format!("\"{}\"", clip(q))),
        ActivityState::Running => match args.get("command").or_else(|| args.get("cmd")) {
            Some(Value::String(s)) => Some(clip(s)),
            Some(Value::Array(parts)) => {
                let words: Vec<&str> = parts.iter().filter_map(Value::as_str).collect();
                Some(clip(&words.join(" ")))
            }
            _ => None,
        }
        .filter(|s| !s.is_empty()),
        _ => None,
    };
    match (state, arg) {
        (ActivityState::Reading, Some(a)) => format!("Reading {a}"),
        (ActivityState::Editing, Some(a)) => format!("Editing {a}"),
        (ActivityState::Searching, Some(a)) => format!("Searching {a}"),
        (ActivityState::Running, Some(a)) => format!("Running {a}"),
        (state, _) => state.label().to_string(),
    }
}

fn failure_label(state: Option<ActivityState>) -> &'static str {
    match state {
        Some(ActivityState::Running) => "Command failed",
        Some(ActivityState::Editing) => "Edit failed",
        Some(ActivityState::Reading) => "Read failed",
        Some(ActivityState::Searching) => "Search failed",
        _ => "Tool failed",
    }
}

/// Folds events into the pill's current [`Activity`].
#[derive(Debug, Clone, Default)]
pub struct ActivityTracker {
    current: Option<Activity>,
    /// The running tool's activity, restored when an approval prompt closes.
    tool: Option<Activity>,
    /// Calls the user (or a rule) denied: their error result isn't a failure.
    denied: BTreeSet<String>,
    /// Paths written this turn, for Done's "2 files".
    files: BTreeSet<String>,
}

impl ActivityTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// What the pill shows now; `None` when idle (no turn, or the turn was cancelled).
    pub fn current(&self) -> Option<&Activity> {
        self.current.as_ref()
    }

    /// Applies one event. Returns the new activity if the pill changed (state or label).
    pub fn apply(&mut self, event: &ActivityEvent) -> Option<&Activity> {
        let next = match event {
            ActivityEvent::TextDelta => {
                if self
                    .current
                    .as_ref()
                    .is_some_and(|a| a.state == ActivityState::Thinking)
                {
                    return None;
                }
                Some(Activity::new(
                    ActivityState::Thinking,
                    ActivityState::Thinking.label(),
                ))
            }
            ActivityEvent::ToolStarted {
                name, args_json, ..
            } => {
                let state = tool_state(name).unwrap_or(ActivityState::Thinking);
                if state == ActivityState::Editing
                    && let Ok(args) = serde_json::from_str::<Value>(args_json)
                    && let Some(path) = path_arg(&args)
                {
                    self.files.insert(path.to_string());
                }
                let activity = Activity::new(state, tool_label(state, args_json));
                self.tool = Some(activity.clone());
                Some(activity)
            }
            ActivityEvent::ApprovalPending { .. } => {
                let mut waiting =
                    Activity::new(ActivityState::Waiting, ActivityState::Waiting.label());
                waiting.detail = self.tool.as_ref().map(|t| t.label.clone());
                Some(waiting)
            }
            ActivityEvent::ApprovalDecided { call_id, approved } => {
                if !approved {
                    self.denied.insert(call_id.clone());
                }
                if self
                    .current
                    .as_ref()
                    .is_some_and(|a| a.state == ActivityState::Waiting)
                {
                    self.tool.clone()
                } else {
                    return None;
                }
            }
            ActivityEvent::ToolFinished { id, name, is_error } => {
                self.tool = None;
                if *is_error && !self.denied.remove(id) {
                    let mut err =
                        Activity::new(ActivityState::Error, failure_label(tool_state(name)));
                    err.detail = Some(name.clone());
                    Some(err)
                } else {
                    // The model reads the result next.
                    Some(Activity::new(
                        ActivityState::Thinking,
                        ActivityState::Thinking.label(),
                    ))
                }
            }
            ActivityEvent::TurnCompleted => {
                let mut done = Activity::new(ActivityState::Done, ActivityState::Done.label());
                done.detail = match self.files.len() {
                    0 => None,
                    1 => Some("1 file".into()),
                    n => Some(format!("{n} files")),
                };
                self.end_turn();
                Some(done)
            }
            ActivityEvent::TurnMaxSteps => {
                self.end_turn();
                Some(Activity::new(ActivityState::Error, "Step limit reached"))
            }
            ActivityEvent::TurnCancelled => {
                self.end_turn();
                self.current = None;
                return None;
            }
            ActivityEvent::Other => return None,
        };
        if next == self.current {
            return None;
        }
        self.current = next;
        self.current.as_ref()
    }

    fn end_turn(&mut self) {
        self.tool = None;
        self.denied.clear();
        self.files.clear();
    }
}

#[cfg(feature = "core")]
mod core_events {
    use rozu_core::{AgentEvent, ApprovalRequest, Decision, TurnOutcome};

    use super::{Activity, ActivityEvent, ActivityTracker};

    impl From<&AgentEvent> for ActivityEvent {
        fn from(event: &AgentEvent) -> Self {
            match event {
                AgentEvent::TextDelta(_) => ActivityEvent::TextDelta,
                AgentEvent::ToolCallStarted {
                    id,
                    name,
                    args_json,
                } => ActivityEvent::ToolStarted {
                    id: id.clone(),
                    name: name.clone(),
                    args_json: args_json.clone(),
                },
                AgentEvent::ToolCallFinished {
                    id, name, is_error, ..
                } => ActivityEvent::ToolFinished {
                    id: id.clone(),
                    name: name.clone(),
                    is_error: *is_error,
                },
                AgentEvent::Approval(record) => ActivityEvent::ApprovalDecided {
                    call_id: record.call_id.clone(),
                    approved: !matches!(record.decision, Decision::Deny { .. }),
                },
                AgentEvent::TurnFinished(TurnOutcome::Completed) => ActivityEvent::TurnCompleted,
                AgentEvent::TurnFinished(TurnOutcome::Cancelled) => ActivityEvent::TurnCancelled,
                AgentEvent::TurnFinished(TurnOutcome::MaxSteps) => ActivityEvent::TurnMaxSteps,
                AgentEvent::Usage(_) => ActivityEvent::Other,
            }
        }
    }

    impl From<&ApprovalRequest> for ActivityEvent {
        fn from(req: &ApprovalRequest) -> Self {
            ActivityEvent::ApprovalPending {
                call_id: req.call_id.clone(),
                tool: req.tool.clone(),
            }
        }
    }

    /// The pill for a single event, without history (e.g. Done has no file count).
    pub fn from_agent_event(event: &AgentEvent) -> Option<Activity> {
        ActivityTracker::new().apply(&event.into()).cloned()
    }

    /// The pill after `events`, in order; `None` when idle.
    pub fn from_agent_events<'a>(
        events: impl IntoIterator<Item = &'a AgentEvent>,
    ) -> Option<Activity> {
        let mut tracker = ActivityTracker::new();
        for event in events {
            tracker.apply(&event.into());
        }
        tracker.current().cloned()
    }
}

#[cfg(feature = "core")]
pub use core_events::{from_agent_event, from_agent_events};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::ActivityState::*;

    fn started(name: &str, args: &str) -> ActivityEvent {
        ActivityEvent::ToolStarted {
            id: "c1".into(),
            name: name.into(),
            args_json: args.into(),
        }
    }

    fn finished(name: &str, is_error: bool) -> ActivityEvent {
        ActivityEvent::ToolFinished {
            id: "c1".into(),
            name: name.into(),
            is_error,
        }
    }

    #[test]
    fn tool_names_map_to_states() {
        for (name, state) in [
            ("read_file", Reading),
            ("list_dir", Reading),
            ("Read", Reading),
            ("grep", Searching),
            ("Glob", Searching),
            ("web_search", Searching),
            ("write_file", Editing),
            ("apply_patch", Editing),
            ("Edit", Editing),
            ("MultiEdit", Editing),
            ("shell", Running),
            ("Bash", Running),
            ("mcp__rozu__shell", Running),
            ("notes", Notes),
            ("TodoWrite", Notes),
        ] {
            assert_eq!(tool_state(name), Some(state), "{name}");
        }
        assert_eq!(tool_state("frobnicate"), None);
        assert_eq!(tool_state("truncate"), None);
    }

    #[test]
    fn labels_use_args() {
        assert_eq!(
            tool_label(Reading, r#"{"path":"README.md"}"#),
            "Reading README.md"
        );
        assert_eq!(
            tool_label(Editing, r#"{"file_path":"src/term/view.rs"}"#),
            "Editing src/term/view.rs"
        );
        assert_eq!(
            tool_label(Searching, r#"{"pattern":"set_size"}"#),
            "Searching \"set_size\""
        );
        assert_eq!(
            tool_label(Running, r#"{"command":"cargo test\necho hi"}"#),
            "Running cargo test"
        );
        assert_eq!(
            tool_label(Running, r#"{"command":["cargo","test"]}"#),
            "Running cargo test"
        );
        assert_eq!(tool_label(Running, "not json"), "Running…");
        assert_eq!(
            tool_label(
                Reading,
                r#"{"path":"crates/rozu-activity/src/very/deep/module.rs"}"#
            ),
            "Reading module.rs"
        );
        let long = tool_label(
            Running,
            r#"{"command":"cargo test --workspace --all-features -- --nocapture"}"#,
        );
        assert!(long.ends_with('…') && long.chars().count() <= "Running ".len() + MAX_ARG_CHARS);
    }

    #[test]
    fn a_turn_maps_through_the_states() {
        let mut t = ActivityTracker::new();
        assert_eq!(t.apply(&ActivityEvent::TextDelta).unwrap().state, Thinking);
        assert!(t.apply(&ActivityEvent::TextDelta).is_none(), "no change");

        let a = t
            .apply(&started("shell", r#"{"command":"cargo test"}"#))
            .unwrap();
        assert_eq!((a.state, a.label.as_str()), (Running, "Running cargo test"));

        let w = t
            .apply(&ActivityEvent::ApprovalPending {
                call_id: "c1".into(),
                tool: "shell".into(),
            })
            .unwrap();
        assert_eq!((w.state, w.hint), (Waiting, Some("Y / N")));
        let back = t
            .apply(&ActivityEvent::ApprovalDecided {
                call_id: "c1".into(),
                approved: true,
            })
            .unwrap();
        assert_eq!(back.state, Running);

        let e = t.apply(&finished("shell", true)).unwrap();
        assert_eq!(
            (e.state, e.label.as_str(), e.hint),
            (Error, "Command failed", Some("R retry"))
        );

        t.apply(&started("write_file", r#"{"path":"a.rs"}"#));
        t.apply(&finished("write_file", false));
        t.apply(&started("apply_patch", r#"{"path":"b.rs"}"#));
        assert_eq!(
            t.apply(&finished("apply_patch", false)).unwrap().state,
            Thinking
        );
        let d = t.apply(&ActivityEvent::TurnCompleted).unwrap();
        assert_eq!((d.state, d.detail.as_deref()), (Done, Some("2 files")));
    }

    #[test]
    fn denied_calls_are_not_errors() {
        let mut t = ActivityTracker::new();
        t.apply(&started("shell", r#"{"command":"rm -rf target"}"#));
        t.apply(&ActivityEvent::ApprovalPending {
            call_id: "c1".into(),
            tool: "shell".into(),
        });
        t.apply(&ActivityEvent::ApprovalDecided {
            call_id: "c1".into(),
            approved: false,
        });
        assert_eq!(t.apply(&finished("shell", true)).unwrap().state, Thinking);
    }

    #[test]
    fn cancel_and_step_cap() {
        let mut t = ActivityTracker::new();
        t.apply(&ActivityEvent::TextDelta);
        assert!(t.apply(&ActivityEvent::TurnCancelled).is_none());
        assert!(t.current().is_none());
        assert_eq!(t.apply(&ActivityEvent::TurnMaxSteps).unwrap().state, Error);
    }

    #[test]
    fn words_split_cases() {
        assert_eq!(words("TodoWrite"), ["todo", "write"]);
        assert_eq!(
            words("mcp__rozu__read_file"),
            ["mcp", "rozu", "read", "file"]
        );
        assert_eq!(words("list-dir"), ["list", "dir"]);
    }

    #[cfg(feature = "core")]
    #[test]
    fn agent_events_map() {
        use rozu_core::{AgentEvent, TurnOutcome};

        let ev = |name: &str, args: &str| AgentEvent::ToolCallStarted {
            id: "c1".into(),
            name: name.into(),
            args_json: args.into(),
        };
        let state = |e: &AgentEvent| from_agent_event(e).map(|a| a.state);
        assert_eq!(state(&AgentEvent::TextDelta("hi".into())), Some(Thinking));
        assert_eq!(
            state(&ev("read_file", r#"{"path":"README.md"}"#)),
            Some(Reading)
        );
        assert_eq!(state(&ev("list_dir", "{}")), Some(Reading));
        assert_eq!(state(&ev("grep", r#"{"pattern":"x"}"#)), Some(Searching));
        assert_eq!(state(&ev("write_file", "{}")), Some(Editing));
        assert_eq!(state(&ev("apply_patch", "{}")), Some(Editing));
        assert_eq!(state(&ev("shell", r#"{"command":"ls"}"#)), Some(Running));
        assert_eq!(state(&ev("TodoWrite", "{}")), Some(Notes));
        assert_eq!(
            state(&AgentEvent::ToolCallFinished {
                id: "c1".into(),
                name: "shell".into(),
                output: "boom".into(),
                is_error: true,
            }),
            Some(Error)
        );
        assert_eq!(
            state(&AgentEvent::TurnFinished(TurnOutcome::Completed)),
            Some(Done)
        );
        assert_eq!(
            state(&AgentEvent::TurnFinished(TurnOutcome::Cancelled)),
            None
        );

        let turn = [
            AgentEvent::TextDelta("Looking. ".into()),
            ev("read_file", r#"{"path":"README.md"}"#),
        ];
        let a = from_agent_events(&turn).unwrap();
        assert_eq!((a.state, a.label.as_str()), (Reading, "Reading README.md"));
    }
}
