//! Claude Code's stream-json protocol (`--input-format stream-json --output-format stream-json
//! --include-partial-messages --verbose`), mapped onto rozu's [`StreamEvent`]s.
//!
//! Output lines (one JSON object each) and what rozu does with them:
//!
//! | `type` | rozu |
//! |---|---|
//! | `system` / `init` | session id (for `--resume`), model, MCP server status: logged |
//! | `stream_event` | partial Anthropic SSE events: `text_delta` → text, `tool_use` blocks + `input_json_delta` → tool call start/args/end |
//! | `assistant` | full messages: text and `tool_use` blocks not already seen as partials |
//! | `user` | `tool_result` blocks → [`StreamEvent::ToolResult`] |
//! | `result` | `usage` → [`StreamEvent::Usage`], then done; `is_error` → a typed error |
//! | anything else | logged at debug and ignored |
//!
//! Events of sub-agents (non-null `parent_tool_use_id`) are skipped; rozu disables `Task` anyway.

use std::collections::{HashMap, HashSet};

use rozu_core::{ProviderError, StopReason, StreamEvent, Usage};
use rozu_mcp::claude::ROZU_TOOL_PREFIX;
use serde_json::{Value, json};
use tracing::{debug, info, warn};

/// What one output line meant.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub events: Vec<StreamEvent>,
    /// From `system`/`init`.
    pub session_id: Option<String>,
    /// The turn is over (a `result` arrived).
    pub done: bool,
}

/// The stdin line for one user turn.
pub fn user_message(text: &str) -> Value {
    json!({
        "type": "user",
        "message": { "role": "user", "content": [{ "type": "text", "text": text }] },
        "parent_tool_use_id": null,
        "session_id": "",
    })
}

/// rozu's tool name for a Claude tool name (`mcp__rozu__read_file` → `read_file`).
pub fn display_name(name: &str) -> &str {
    name.strip_prefix(ROZU_TOOL_PREFIX).unwrap_or(name)
}

/// Maps the output lines of one turn. Keep one per turn.
#[derive(Debug, Default)]
pub struct Mapper {
    /// Message ids whose text arrived as partial deltas (the full message repeats it).
    streamed_text: HashSet<String>,
    /// Tool calls already started (from partials), so the full message doesn't repeat them.
    started_tools: HashSet<String>,
    /// Content-block index → tool_use id, for the partial message being streamed.
    open_tools: HashMap<u64, String>,
    current_message: Option<String>,
}

impl Mapper {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn map(&mut self, line: &Value) -> Result<Step, ProviderError> {
        let mut step = Step::default();
        if !line["parent_tool_use_id"].is_null() {
            return Ok(step);
        }
        match line["type"].as_str().unwrap_or_default() {
            "system" if line["subtype"] == "init" => {
                let rozu = line["mcp_servers"]
                    .as_array()
                    .and_then(|servers| servers.iter().find(|s| s["name"] == "rozu"))
                    .and_then(|s| s["status"].as_str())
                    .unwrap_or("missing");
                info!(
                    target: "rozu::cli",
                    session_id = line["session_id"].as_str().unwrap_or(""),
                    model = line["model"].as_str().unwrap_or(""),
                    permission_mode = line["permissionMode"].as_str().unwrap_or(""),
                    api_key_source = line["apiKeySource"].as_str().unwrap_or(""),
                    rozu_mcp = rozu,
                    "claude session"
                );
                if rozu != "connected" {
                    warn!(target: "rozu::cli", status = rozu,
                        "rozu's MCP server isn't connected; Claude can't use rozu's tools");
                }
                step.session_id = line["session_id"].as_str().map(str::to_owned);
            }
            "stream_event" => self.partial(&line["event"], &mut step.events),
            "assistant" => self.assistant(&line["message"], &mut step.events),
            "user" => tool_results(&line["message"], &mut step.events),
            "result" => {
                let usage = &line["usage"];
                info!(
                    target: "rozu::usage",
                    provider = "claude-cli",
                    usage_raw = %usage,
                    // Notional on a subscription: kept in the log, never reported as cost.
                    total_cost_usd_notional = line["total_cost_usd"].as_f64().unwrap_or(0.0),
                    num_turns = line["num_turns"].as_u64().unwrap_or(0),
                    duration_ms = line["duration_ms"].as_u64().unwrap_or(0),
                    "turn usage"
                );
                if usage.is_object() {
                    step.events
                        .push(StreamEvent::Usage(usage_from_claude(usage)));
                }
                let subtype = line["subtype"].as_str().unwrap_or("");
                if line["is_error"].as_bool() == Some(true) || subtype != "success" {
                    let message = line["result"]
                        .as_str()
                        .or_else(|| line["error"].as_str())
                        .unwrap_or(subtype);
                    return Err(classify_error(subtype, message));
                }
                step.events.push(StreamEvent::Done(StopReason::EndTurn));
                step.done = true;
            }
            other => debug!(target: "rozu::cli", kind = other, "ignored claude event"),
        }
        Ok(step)
    }

    fn partial(&mut self, event: &Value, out: &mut Vec<StreamEvent>) {
        let index = event["index"].as_u64();
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.current_message = event["message"]["id"].as_str().map(str::to_owned);
                self.open_tools.clear();
            }
            "content_block_start" => {
                let block = &event["content_block"];
                match block["type"].as_str() {
                    Some("text") => {
                        let text = block["text"].as_str().unwrap_or_default();
                        if !text.is_empty() {
                            self.mark_streamed();
                            out.push(StreamEvent::TextDelta(text.to_owned()));
                        }
                    }
                    Some("tool_use") => {
                        let (Some(id), Some(index)) = (block["id"].as_str(), index) else {
                            return;
                        };
                        self.open_tools.insert(index, id.to_owned());
                        self.started_tools.insert(id.to_owned());
                        out.push(StreamEvent::ToolCallStart {
                            id: id.to_owned(),
                            name: display_name(block["name"].as_str().unwrap_or("")).to_owned(),
                        });
                    }
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                match delta["type"].as_str() {
                    Some("text_delta") => {
                        let text = delta["text"].as_str().unwrap_or_default();
                        if !text.is_empty() {
                            self.mark_streamed();
                            out.push(StreamEvent::TextDelta(text.to_owned()));
                        }
                    }
                    Some("input_json_delta") => {
                        if let Some(id) = index.and_then(|i| self.open_tools.get(&i)) {
                            out.push(StreamEvent::ToolCallArgsDelta {
                                id: id.clone(),
                                json: delta["partial_json"].as_str().unwrap_or("").to_owned(),
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(id) = index.and_then(|i| self.open_tools.remove(&i)) {
                    out.push(StreamEvent::ToolCallEnd { id });
                }
            }
            _ => {}
        }
    }

    fn mark_streamed(&mut self) {
        if let Some(id) = &self.current_message {
            self.streamed_text.insert(id.clone());
        }
    }

    /// A full assistant message: emit only what the partials didn't.
    fn assistant(&mut self, message: &Value, out: &mut Vec<StreamEvent>) {
        let streamed = message["id"]
            .as_str()
            .is_some_and(|id| self.streamed_text.contains(id));
        for block in message["content"].as_array().into_iter().flatten() {
            match block["type"].as_str() {
                Some("text") if !streamed => {
                    let text = block["text"].as_str().unwrap_or_default();
                    if !text.is_empty() {
                        out.push(StreamEvent::TextDelta(text.to_owned()));
                    }
                }
                Some("tool_use") => {
                    let Some(id) = block["id"].as_str() else {
                        continue;
                    };
                    if !self.started_tools.insert(id.to_owned()) {
                        continue;
                    }
                    out.push(StreamEvent::ToolCallStart {
                        id: id.to_owned(),
                        name: display_name(block["name"].as_str().unwrap_or("")).to_owned(),
                    });
                    out.push(StreamEvent::ToolCallArgsDelta {
                        id: id.to_owned(),
                        json: block["input"].to_string(),
                    });
                    out.push(StreamEvent::ToolCallEnd { id: id.to_owned() });
                }
                _ => {}
            }
        }
    }
}

fn tool_results(message: &Value, out: &mut Vec<StreamEvent>) {
    for block in message["content"].as_array().into_iter().flatten() {
        if block["type"] != "tool_result" {
            continue;
        }
        let Some(id) = block["tool_use_id"].as_str() else {
            continue;
        };
        let output = match &block["content"] {
            Value::String(text) => text.clone(),
            Value::Array(parts) => parts
                .iter()
                .filter_map(|p| p["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n"),
            Value::Null => String::new(),
            other => other.to_string(),
        };
        out.push(StreamEvent::ToolResult {
            id: id.to_owned(),
            output,
            is_error: block["is_error"].as_bool().unwrap_or(false),
        });
    }
}

/// Claude's `usage` → rozu's [`Usage`].
///
/// Claude reports uncached `input_tokens` separately from `cache_creation_input_tokens` (cache
/// writes) and `cache_read_input_tokens` (cache reads). rozu's `input_tokens` is **all** prompt
/// tokens, so it is the sum of the three; reads and writes are also kept separately.
pub fn usage_from_claude(usage: &Value) -> Usage {
    let n = |key: &str| usage[key].as_u64().unwrap_or(0);
    let read = n("cache_read_input_tokens");
    let write = n("cache_creation_input_tokens");
    Usage {
        input_tokens: n("input_tokens") + read + write,
        output_tokens: n("output_tokens"),
        cache_read_tokens: read,
        cache_write_tokens: write,
    }
}

/// The hint shown when Claude Code isn't logged in.
pub const LOGIN_HINT: &str =
    "run `claude` in a terminal and log in with /login (rozu never handles Claude credentials)";

/// A failed turn (`result` with `is_error`, or the process dying) as a typed error.
pub fn classify_error(subtype: &str, message: &str) -> ProviderError {
    let lower = message.to_lowercase();
    if looks_logged_out(&lower) {
        ProviderError::NotLoggedIn {
            cli: "claude".into(),
            hint: LOGIN_HINT.into(),
        }
    } else if [
        "usage limit",
        "limit reached",
        "rate limit",
        "rate_limit",
        "429",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        ProviderError::UsageLimit {
            message: message.trim().to_owned(),
        }
    } else {
        ProviderError::Api {
            code: (!subtype.is_empty()).then(|| subtype.to_owned()),
            message: message.trim().to_owned(),
        }
    }
}

/// Whether CLI output (lowercased) says it isn't logged in.
pub fn looks_logged_out(lower: &str) -> bool {
    [
        "not logged in",
        "/login",
        "invalid api key",
        "please log in",
        "oauth token has expired",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_sums_uncached_and_cached_input() {
        let usage = usage_from_claude(&json!({
            "input_tokens": 12,
            "cache_creation_input_tokens": 3000,
            "cache_read_input_tokens": 45000,
            "output_tokens": 321,
            "service_tier": "standard",
        }));
        assert_eq!(
            usage,
            Usage {
                input_tokens: 48_012,
                output_tokens: 321,
                cache_read_tokens: 45_000,
                cache_write_tokens: 3_000,
            }
        );
        assert_eq!(usage_from_claude(&json!({})), Usage::default());
    }

    #[test]
    fn classifies_errors() {
        assert!(matches!(
            classify_error("success", "Invalid API key · Please run /login"),
            ProviderError::NotLoggedIn { .. }
        ));
        assert!(matches!(
            classify_error("success", "Claude AI usage limit reached|1760400000"),
            ProviderError::UsageLimit { .. }
        ));
        assert!(matches!(
            classify_error("error_max_turns", "max turns"),
            ProviderError::Api { code: Some(code), .. } if code == "error_max_turns"
        ));
        let text = classify_error("", "x").to_string();
        assert!(!text.contains("()"), "{text}");
    }

    #[test]
    fn partial_and_full_messages_are_not_doubled() {
        let mut mapper = Mapper::new();
        let lines = [
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "message_start", "message": {"id": "msg_1"}}}),
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}}),
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Hi"}}}),
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "mcp__rozu__list_dir", "input": {}}}}),
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{}"}}}),
            json!({"type": "stream_event", "parent_tool_use_id": null, "event": {"type": "content_block_stop", "index": 1}}),
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": "msg_1", "content": [
                {"type": "text", "text": "Hi"},
                {"type": "tool_use", "id": "toolu_1", "name": "mcp__rozu__list_dir", "input": {}}]}}),
            // A message that only arrived in full (no partials).
            json!({"type": "assistant", "parent_tool_use_id": null, "message": {"id": "msg_2", "content": [
                {"type": "text", "text": "Bye"}]}}),
            // Sub-agent chatter is skipped.
            json!({"type": "assistant", "parent_tool_use_id": "toolu_9", "message": {"id": "msg_3", "content": [
                {"type": "text", "text": "nested"}]}}),
        ];
        let events: Vec<_> = lines
            .iter()
            .flat_map(|line| mapper.map(line).unwrap().events)
            .collect();
        assert_eq!(
            events,
            [
                StreamEvent::TextDelta("Hi".into()),
                StreamEvent::ToolCallStart {
                    id: "toolu_1".into(),
                    name: "list_dir".into()
                },
                StreamEvent::ToolCallArgsDelta {
                    id: "toolu_1".into(),
                    json: "{}".into()
                },
                StreamEvent::ToolCallEnd {
                    id: "toolu_1".into()
                },
                StreamEvent::TextDelta("Bye".into()),
            ]
        );
    }

    #[test]
    fn tool_results_accept_string_or_parts() {
        let mut mapper = Mapper::new();
        let step = mapper
            .map(&json!({"type": "user", "parent_tool_use_id": null, "message": {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "a", "content": "plain"},
                {"type": "tool_result", "tool_use_id": "b", "content": [{"type": "text", "text": "x"}, {"type": "text", "text": "y"}], "is_error": true},
            ]}}))
            .unwrap();
        assert_eq!(
            step.events,
            [
                StreamEvent::ToolResult {
                    id: "a".into(),
                    output: "plain".into(),
                    is_error: false
                },
                StreamEvent::ToolResult {
                    id: "b".into(),
                    output: "x\ny".into(),
                    is_error: true
                },
            ]
        );
    }

    #[test]
    fn result_errors_are_typed() {
        let mut mapper = Mapper::new();
        let err = mapper
            .map(
                &json!({"type": "result", "subtype": "success", "is_error": true,
                "result": "Not logged in · Please run /login"}),
            )
            .unwrap_err();
        assert!(matches!(err, ProviderError::NotLoggedIn { .. }), "{err}");
        assert!(err.to_string().contains("/login"), "{err}");
    }
}
