//! `rozu grok-hook`: a Grok Build `PostToolUse` command hook that can replace or truncate the
//! model's copy of an MCP tool result (`updatedToolOutput`, verified live with Grok Build
//! 1.0.52) according to rozu's masking policy.
//!
//! Grok runs the hook with one JSON object on stdin (`hookEventName`, `toolName` — the MCP name
//! such as `rozu__read_file`, not `use_tool` — `toolResult`, `toolResultTruncated`, …) and reads
//! an optional JSON object from stdout. Printing nothing and exiting 0 leaves the result
//! unchanged, which is the default policy ([`HookPolicy::Passthrough`]). The hook only ever
//! touches rozu's own tools: rewriting `search_tool` output breaks Grok's tool discovery.

use rozu_tools::truncate_head_tail;
use serde_json::{Value, json};

use super::protocol::ROZU_TOOL_PREFIX;

/// The hook's matcher (a regex on the tool name): rozu's MCP tools only.
pub const MATCHER: &str = "^rozu__";

/// What the hook does with a rozu tool's result.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum HookPolicy {
    /// Leave it as is.
    #[default]
    Passthrough,
    /// Keep the head and tail of results longer than this many bytes.
    Truncate { max_bytes: usize },
    /// Replace every result with this text (`{tool}` is replaced by the tool's name).
    Replace { text: String },
}

/// The hook's stdout for one event: `None` to leave the result unchanged (also for anything
/// that isn't a `PostToolUse` of a rozu tool, or unreadable input: hooks fail open).
pub fn respond(input: &str, policy: &HookPolicy) -> Option<String> {
    if *policy == HookPolicy::Passthrough {
        return None;
    }
    let event: Value = serde_json::from_str(input).ok()?;
    let field = |camel: &str, snake: &str| {
        event
            .get(camel)
            .filter(|v| !v.is_null())
            .or_else(|| event.get(snake))
            .cloned()
            .unwrap_or(Value::Null)
    };
    if field("hookEventName", "hook_event_name") != "PostToolUse" {
        return None;
    }
    let tool = field("toolName", "tool_name");
    let tool = tool.as_str()?;
    if !tool.starts_with(ROZU_TOOL_PREFIX) {
        return None;
    }
    let result = field("toolResult", "tool_response");
    let text = match &result {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let updated = match policy {
        HookPolicy::Passthrough => return None,
        HookPolicy::Truncate { max_bytes } => {
            if text.len() <= *max_bytes {
                return None;
            }
            truncate_head_tail(&text, *max_bytes).into_owned()
        }
        HookPolicy::Replace { text } => text.replace("{tool}", tool),
    };
    Some(
        json!({
            "hookSpecificOutput": {
                "hookEventName": "PostToolUse",
                "updatedToolOutput": updated,
            }
        })
        .to_string(),
    )
}

/// The hook configuration (Grok's hook JSON shape: event → matcher groups → command hooks)
/// that runs `command` after each rozu MCP tool.
pub fn config(command: &str) -> Value {
    json!({
        "PostToolUse": [{
            "matcher": MATCHER,
            "hooks": [{ "type": "command", "command": command, "timeout": 10 }],
        }]
    })
}

/// `'<rozu>' grok-hook …` as a shell command line.
pub fn command(rozu: &str, policy: &HookPolicy) -> String {
    let mut line = format!("{} grok-hook", shell_quote(rozu));
    match policy {
        HookPolicy::Passthrough => {}
        HookPolicy::Truncate { max_bytes } => line.push_str(&format!(" --max-bytes {max_bytes}")),
        HookPolicy::Replace { text } => {
            line.push_str(&format!(" --replace {}", shell_quote(text)));
        }
    }
    line
}

fn shell_quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', r"'\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PostToolUse event as Grok Build 1.0.52 sent it (both key spellings; values made up).
    fn event(tool: &str, result: Value) -> String {
        json!({
            "hookEventName": "PostToolUse", "hook_event_name": "PostToolUse",
            "sessionId": "s", "session_id": "s", "cwd": "/w", "workspaceRoot": "/w",
            "timestamp": "t", "permissionMode": "default", "durationMs": 3,
            "toolName": tool, "tool_name": tool, "toolUseId": "u1", "tool_use_id": "u1",
            "toolInput": {}, "tool_input": {}, "toolInputTruncated": false,
            "toolResult": result, "tool_response": result, "toolResultTruncated": false,
        })
        .to_string()
    }

    fn updated(out: &str) -> Value {
        let value: Value = serde_json::from_str(out).unwrap();
        assert_eq!(value["hookSpecificOutput"]["hookEventName"], "PostToolUse");
        value["hookSpecificOutput"]["updatedToolOutput"].clone()
    }

    #[test]
    fn passthrough_prints_nothing() {
        let input = event("rozu__read_file", json!("x".repeat(100_000)));
        assert_eq!(respond(&input, &HookPolicy::Passthrough), None);
    }

    #[test]
    fn truncates_long_rozu_results_only() {
        let policy = HookPolicy::Truncate { max_bytes: 100 };
        let long = format!("HEAD{}TAIL", "x".repeat(5000));
        let out = respond(&event("rozu__shell", json!(long)), &policy).unwrap();
        let text = updated(&out);
        let text = text.as_str().unwrap();
        assert!(text.starts_with("HEAD") && text.ends_with("TAIL"), "{text}");
        assert!(text.len() < long.len());
        // Short results, other tools and other events are left alone.
        assert_eq!(
            respond(&event("rozu__shell", json!("short")), &policy),
            None
        );
        assert_eq!(respond(&event("search_tool", json!(long)), &policy), None);
        assert_eq!(respond(&event("other__tool", json!(long)), &policy), None);
        let pre = event("rozu__shell", json!(long)).replace("PostToolUse", "PreToolUse");
        assert_eq!(respond(&pre, &policy), None);
        assert_eq!(respond("not json", &policy), None);
    }

    #[test]
    fn replaces_structured_results() {
        let policy = HookPolicy::Replace {
            text: "[masked {tool} output]".into(),
        };
        let mcp = json!({"type": "MCP", "tool_name": "read_file", "server_name": "rozu",
            "output": {"OkayOutput": "secret-ish"}});
        let out = respond(&event("rozu__read_file", mcp), &policy).unwrap();
        assert_eq!(updated(&out), "[masked rozu__read_file output]");
        // snake_case-only input works too.
        let snake = json!({"hook_event_name": "PostToolUse", "tool_name": "rozu__grep", "tool_response": "r"});
        assert!(respond(&snake.to_string(), &policy).is_some());
    }

    #[test]
    fn config_and_command_line() {
        let cfg = config("'/bin/rozu' grok-hook --max-bytes 9");
        assert_eq!(cfg["PostToolUse"][0]["matcher"], MATCHER);
        assert_eq!(cfg["PostToolUse"][0]["hooks"][0]["type"], "command");
        assert_eq!(
            command("/a b/rozu", &HookPolicy::Truncate { max_bytes: 9 }),
            "'/a b/rozu' grok-hook --max-bytes 9"
        );
        assert_eq!(
            command(
                "rozu",
                &HookPolicy::Replace {
                    text: "it's".into()
                }
            ),
            r"'rozu' grok-hook --replace 'it'\''s'"
        );
    }
}
