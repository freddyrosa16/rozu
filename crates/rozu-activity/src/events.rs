//! Agent events → activity state and label.
//!
//! [`ActivityEvent`] mirrors the parts of `rozu_core::AgentEvent` the pill needs, so the mapping
//! works (and is tested) without rozu-core. With the `core` feature, `AgentEvent`,
//! `ApprovalRequest` and the turn's error (`AgentError` / `ProviderError`) convert into it.
//!
//! Waiting comes from the event stream: `AgentEvent::ApprovalPending` is sent the moment a
//! prompt waits for the user (native loop, `rozu mcp-serve` forwards, Codex's own requests) and
//! `AgentEvent::ApprovalResolved` when it closes; decisions made without asking never open
//! one. While any prompt is open the pill shows Waiting; when the last one closes it goes back
//! to the tool that was running, or to what it showed before.

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
    /// An approval prompt about a call is open and waiting for the user.
    ApprovalPending {
        call_id: String,
        tool: String,
    },
    /// That prompt closed.
    ApprovalResolved {
        call_id: String,
        outcome: ApprovalOutcome,
    },
    /// A decision was logged (by the user, a rule or the mode). Closes a prompt still open for
    /// the call (hosts that report prompts from their gate send no `ApprovalResolved`).
    ApprovalDecided {
        call_id: String,
        approved: bool,
    },
    TurnCompleted,
    TurnCancelled,
    /// The step cap was reached.
    TurnMaxSteps,
    /// The turn ended with an error; `offline` when the provider couldn't be reached.
    TurnFailed {
        offline: bool,
    },
    /// Events that don't change the pill (usage, …).
    Other,
}

/// How an approval prompt closed (`rozu_core::ApprovalResolution`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalOutcome {
    Approved,
    Denied,
    /// Nobody answered (the turn was cancelled, or the asker went away).
    Expired,
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
    // Note-taking tools (`TodoWrite`, memory, plans) are editing too.
    if has(&["notes", "note", "todo", "todos", "memory", "plan"]) {
        Some(ActivityState::Editing)
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
fn tool_label(state: ActivityState, args: &Value) -> String {
    let arg = match state {
        ActivityState::Reading | ActivityState::Editing => path_arg(args).map(short_path),
        ActivityState::Searching => str_arg(args, &["pattern", "query", "q", "regex", "glob"])
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
    /// What the pill showed when Waiting began, restored if no tool is running.
    before_wait: Option<Activity>,
    /// Calls whose approval prompt is open.
    pending: BTreeSet<String>,
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

    /// The running tool, even while an approval prompt keeps the pill on Waiting.
    pub fn running_tool(&self) -> Option<&Activity> {
        self.tool.as_ref()
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
                let args = serde_json::from_str(args_json).unwrap_or(Value::Null);
                if state == ActivityState::Editing
                    && let Some(path) = path_arg(&args)
                {
                    self.files.insert(path.to_string());
                }
                let activity = Activity::new(state, tool_label(state, &args));
                self.tool = Some(activity.clone());
                Some(activity)
            }
            ActivityEvent::ApprovalPending { call_id, tool } => {
                self.pending.insert(call_id.clone());
                if !self.waiting() {
                    self.before_wait = self.current.clone();
                }
                let mut waiting =
                    Activity::new(ActivityState::Waiting, ActivityState::Waiting.label());
                waiting.detail = Some(
                    self.tool
                        .as_ref()
                        .map_or_else(|| tool.clone(), |t| t.label.clone()),
                );
                Some(waiting)
            }
            ActivityEvent::ApprovalResolved { call_id, outcome } => {
                if *outcome == ApprovalOutcome::Denied {
                    self.denied.insert(call_id.clone());
                }
                self.close_prompt(call_id)?
            }
            ActivityEvent::ApprovalDecided { call_id, approved } => {
                if !approved {
                    self.denied.insert(call_id.clone());
                }
                self.close_prompt(call_id)?
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
            ActivityEvent::TurnFailed { offline } => {
                self.end_turn();
                Some(if *offline {
                    Activity::new(ActivityState::Offline, ActivityState::Offline.label())
                } else {
                    Activity::new(ActivityState::Error, "Request failed")
                })
            }
            ActivityEvent::Other => return None,
        };
        // An open prompt keeps the pill on Waiting until it closes; the rest of the turn's
        // progress is remembered and shown then.
        if !self.pending.is_empty()
            && matches!(
                event,
                ActivityEvent::TextDelta
                    | ActivityEvent::ToolStarted { .. }
                    | ActivityEvent::ToolFinished { .. }
            )
        {
            self.before_wait = next;
            return None;
        }
        if next == self.current {
            return None;
        }
        self.current = next;
        self.current.as_ref()
    }

    fn waiting(&self) -> bool {
        self.current
            .as_ref()
            .is_some_and(|a| a.state == ActivityState::Waiting)
    }

    /// Closes `call_id`'s prompt. `Some(activity)` to show next when it was the last one open
    /// (the running tool, else what Waiting replaced); `None` when nothing changes.
    fn close_prompt(&mut self, call_id: &str) -> Option<Option<Activity>> {
        if !self.pending.remove(call_id) || !self.pending.is_empty() || !self.waiting() {
            return None;
        }
        let before = self.before_wait.take();
        Some(Some(self.tool.clone().or(before).unwrap_or_else(|| {
            Activity::new(ActivityState::Thinking, ActivityState::Thinking.label())
        })))
    }

    fn end_turn(&mut self) {
        self.tool = None;
        self.before_wait = None;
        self.pending.clear();
        self.denied.clear();
        self.files.clear();
    }
}

#[cfg(feature = "core")]
mod core_events {
    use rozu_core::{
        AgentError, AgentEvent, ApprovalRequest, ApprovalResolution, Decision, ProviderError,
        TurnOutcome,
    };

    use super::{Activity, ActivityEvent, ActivityTracker, ApprovalOutcome};

    impl From<ApprovalResolution> for ApprovalOutcome {
        fn from(resolution: ApprovalResolution) -> Self {
            match resolution {
                ApprovalResolution::Approved => ApprovalOutcome::Approved,
                ApprovalResolution::Denied => ApprovalOutcome::Denied,
                ApprovalResolution::Expired => ApprovalOutcome::Expired,
            }
        }
    }

    /// A turn that ended with this error: Offline when the provider couldn't be reached, else
    /// Error.
    impl From<&ProviderError> for ActivityEvent {
        fn from(err: &ProviderError) -> Self {
            ActivityEvent::TurnFailed {
                offline: matches!(err, ProviderError::Network(_)),
            }
        }
    }

    impl From<&AgentError> for ActivityEvent {
        fn from(err: &AgentError) -> Self {
            match err {
                AgentError::Provider(err) => err.into(),
            }
        }
    }

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
                AgentEvent::ApprovalPending(pending) => ActivityEvent::ApprovalPending {
                    call_id: pending.request_id.clone(),
                    tool: pending.tool.clone(),
                },
                AgentEvent::ApprovalResolved {
                    request_id,
                    resolution,
                } => ActivityEvent::ApprovalResolved {
                    call_id: request_id.clone(),
                    outcome: (*resolution).into(),
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

    fn pending(id: &str) -> ActivityEvent {
        ActivityEvent::ApprovalPending {
            call_id: id.into(),
            tool: "shell".into(),
        }
    }

    fn resolved(id: &str, outcome: ApprovalOutcome) -> ActivityEvent {
        ActivityEvent::ApprovalResolved {
            call_id: id.into(),
            outcome,
        }
    }

    fn decided(approved: bool) -> ActivityEvent {
        ActivityEvent::ApprovalDecided {
            call_id: "c1".into(),
            approved,
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
            ("notes", Editing),
            ("TodoWrite", Editing),
        ] {
            assert_eq!(tool_state(name), Some(state), "{name}");
        }
        assert_eq!(tool_state("frobnicate"), None);
        assert_eq!(tool_state("truncate"), None);
    }

    #[test]
    fn labels_use_args() {
        for (state, args, label) in [
            (Reading, r#"{"path":"README.md"}"#, "Reading README.md"),
            (
                Editing,
                r#"{"file_path":"src/term/view.rs"}"#,
                "Editing src/term/view.rs",
            ),
            (
                Searching,
                r#"{"pattern":"set_size"}"#,
                "Searching \"set_size\"",
            ),
            (
                Running,
                r#"{"command":"cargo test\necho hi"}"#,
                "Running cargo test",
            ),
            (
                Running,
                r#"{"command":["cargo","test"]}"#,
                "Running cargo test",
            ),
            (Running, "not json", "Running…"),
            (
                Reading,
                r#"{"path":"crates/rozu-activity/src/very/deep/module.rs"}"#,
                "Reading module.rs",
            ),
        ] {
            let parsed = serde_json::from_str(args).unwrap_or(Value::Null);
            assert_eq!(tool_label(state, &parsed), label, "{args}");
        }
        let long = tool_label(
            Running,
            &serde_json::json!({"command": "cargo test --workspace --all-features -- --nocapture"}),
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

        let w = t.apply(&pending("c1")).unwrap();
        assert_eq!((w.state, w.hint), (Waiting, Some("Y / N")));
        assert_eq!(t.apply(&decided(true)).unwrap().state, Running);

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
        assert!(t.running_tool().is_none());
    }

    #[test]
    fn denied_calls_are_not_errors() {
        let mut t = ActivityTracker::new();
        t.apply(&started("shell", r#"{"command":"rm -rf target"}"#));
        t.apply(&pending("c1"));
        t.apply(&decided(false));
        assert_eq!(t.apply(&finished("shell", true)).unwrap().state, Thinking);
    }

    #[test]
    fn approval_pending_waits_and_resolved_goes_back() {
        let mut t = ActivityTracker::new();
        t.apply(&started("shell", r#"{"command":"cargo test"}"#));
        let w = t.apply(&pending("c1")).unwrap();
        assert_eq!(
            (w.state, w.detail.as_deref()),
            (Waiting, Some("Running cargo test"))
        );
        let back = t.apply(&resolved("c1", ApprovalOutcome::Approved)).unwrap();
        assert_eq!(
            (back.state, back.label.as_str()),
            (Running, "Running cargo test")
        );
        assert!(t.apply(&decided(true)).is_none(), "decision record after");

        // A backend's own request, with no tool started.
        let mut t = ActivityTracker::new();
        t.apply(&ActivityEvent::TextDelta);
        let w = t.apply(&pending("x9")).unwrap();
        assert_eq!((w.state, w.detail.as_deref()), (Waiting, Some("shell")));
        assert_eq!(
            t.apply(&resolved("x9", ApprovalOutcome::Expired))
                .unwrap()
                .state,
            Thinking
        );
        assert!(
            t.apply(&resolved("nope", ApprovalOutcome::Approved))
                .is_none()
        );
    }

    #[test]
    fn waiting_holds_until_the_last_prompt_closes() {
        let mut t = ActivityTracker::new();
        t.apply(&started("shell", r#"{"command":"make"}"#));
        t.apply(&pending("a"));
        t.apply(&pending("b"));
        assert!(t.apply(&ActivityEvent::TextDelta).is_none());
        assert!(t.apply(&resolved("a", ApprovalOutcome::Approved)).is_none());
        assert_eq!(t.current().unwrap().state, Waiting);
        assert_eq!(t.running_tool().unwrap().label, "Running make");
        assert_eq!(
            t.apply(&resolved("b", ApprovalOutcome::Approved))
                .unwrap()
                .state,
            Running
        );
    }

    #[test]
    fn malformed_edits_do_not_count_as_written_files() {
        let mut tracker = ActivityTracker::new();
        for args in ["not json", "{}", r#"{"path":42}"#, r#"{"path":"a.rs"}"#] {
            tracker.apply(&started("write_file", args));
            tracker.apply(&finished("write_file", false));
        }
        let done = tracker.apply(&ActivityEvent::TurnCompleted).unwrap();
        assert_eq!(done.detail.as_deref(), Some("1 file"));
        assert!(tracker.running_tool().is_none());
    }

    #[test]
    fn denied_prompt_is_not_an_error_and_turn_end_clears_prompts() {
        let mut t = ActivityTracker::new();
        t.apply(&started("shell", r#"{"command":"rm -rf target"}"#));
        t.apply(&pending("c1"));
        t.apply(&resolved("c1", ApprovalOutcome::Denied));
        assert_eq!(t.apply(&finished("shell", true)).unwrap().state, Thinking);

        t.apply(&pending("c2"));
        let d = t.apply(&ActivityEvent::TurnCompleted).unwrap();
        assert_eq!(d.state, Done);
        assert_eq!(t.apply(&ActivityEvent::TextDelta).unwrap().state, Thinking);
    }

    #[test]
    fn turn_errors_map_to_error_or_offline() {
        let mut t = ActivityTracker::new();
        t.apply(&ActivityEvent::TextDelta);
        let e = t
            .apply(&ActivityEvent::TurnFailed { offline: false })
            .unwrap();
        assert_eq!((e.state, e.hint), (Error, Some("R retry")));
        let o = t
            .apply(&ActivityEvent::TurnFailed { offline: true })
            .unwrap();
        assert_eq!(o.state, Offline);
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
        for (name, args, want) in [
            ("read_file", r#"{"path":"README.md"}"#, Reading),
            ("list_dir", "{}", Reading),
            ("grep", r#"{"pattern":"x"}"#, Searching),
            ("write_file", "{}", Editing),
            ("apply_patch", "{}", Editing),
            ("shell", r#"{"command":"ls"}"#, Running),
            ("TodoWrite", "{}", Editing),
        ] {
            assert_eq!(state(&ev(name, args)), Some(want), "{name}");
        }
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

    #[cfg(feature = "core")]
    #[test]
    fn approval_agent_events_map() {
        use rozu_core::{
            AgentError, AgentEvent, ApprovalResolution, PendingApproval, ProviderError, TurnOutcome,
        };

        let pending = AgentEvent::ApprovalPending(PendingApproval {
            request_id: "c1".into(),
            tool: "shell".into(),
            summary: "cargo test".into(),
            provider: "codex".into(),
        });
        let resolved = |resolution| AgentEvent::ApprovalResolved {
            request_id: "c1".into(),
            resolution,
        };
        let shell = AgentEvent::ToolCallStarted {
            id: "c1".into(),
            name: "shell".into(),
            args_json: r#"{"command":"cargo test"}"#.into(),
        };
        let state = |events: &[AgentEvent]| from_agent_events(events).map(|a| a.state);

        assert_eq!(state(std::slice::from_ref(&pending)), Some(Waiting));
        assert_eq!(state(&[shell.clone(), pending.clone()]), Some(Waiting));
        for resolution in [
            ApprovalResolution::Approved,
            ApprovalResolution::Denied,
            ApprovalResolution::Expired,
        ] {
            assert_eq!(
                state(&[shell.clone(), pending.clone(), resolved(resolution)]),
                Some(Running),
                "{resolution}"
            );
        }
        assert_eq!(
            state(&[
                shell,
                pending,
                resolved(ApprovalResolution::Approved),
                AgentEvent::TurnFinished(TurnOutcome::Completed),
            ]),
            Some(Done)
        );

        let failed = |err: ProviderError| {
            let mut t = ActivityTracker::new();
            t.apply(&(&AgentEvent::TextDelta("x".into())).into());
            t.apply(&(&AgentError::Provider(err)).into())
                .map(|a| a.state)
        };
        assert_eq!(
            failed(ProviderError::Network("refused".into())),
            Some(Offline)
        );
        assert_eq!(failed(ProviderError::UnexpectedEof), Some(Error));
    }
}
