//! The Agent Client Protocol (ACP) as Grok Build speaks it on `grok agent stdio` (JSON-RPC 2.0,
//! one JSON object per line), and how its session updates map onto rozu's [`StreamEvent`]s.
//!
//! | ACP | rozu |
//! |---|---|
//! | `session/update` `agent_message_chunk` | text delta |
//! | `session/update` `tool_call` (`use_tool` → `rozu__<tool>`) | tool call `<tool>` (start + args + end) |
//! | `session/update` `tool_call` (anything else, e.g. `search_tool`) | tool call `grok:<name>` |
//! | `session/update` `tool_call_update` (`completed` / `failed`) | [`StreamEvent::ToolResult`] |
//! | `session/prompt` result | usage (from `_meta`), then done or cancelled |
//! | `session/prompt` error | typed error (not signed in, usage limit, other) |
//! | anything else (thoughts, plans, available commands, …) | logged at debug |
//!
//! Field names come from Grok Build 1.0.52 transcripts (ACP over `grok agent stdio` and
//! headless `streaming-json`) and xAI's user guide.

use std::collections::{HashMap, HashSet};

use rozu_core::{ProviderError, StopReason, StreamEvent, Usage};
use serde_json::{Value, json};
use tracing::{debug, info};

pub use crate::codex::protocol::Incoming;

/// The hint shown when Grok Build isn't signed in.
pub const LOGIN_HINT: &str = "run `rozu auth login grok` (it runs the official `grok login`; \
     rozu never handles Grok credentials)";
/// The only ACP auth method rozu picks: the login `grok login` cached. Never an API key.
pub const AUTH_METHOD: &str = "cached_token";
/// ACP protocol version rozu speaks.
pub const PROTOCOL_VERSION: u64 = 1;
/// Prefix Grok gives the tools of rozu's MCP server (`rozu__read_file`).
pub const ROZU_TOOL_PREFIX: &str = "rozu__";
/// Grok's meta-tool that calls an MCP tool by name.
pub const USE_TOOL: &str = "use_tool";
/// JSON-RPC "method not found".
pub const METHOD_NOT_FOUND: i64 = -32601;

pub fn request(id: u64, method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params })
}

pub fn notification(method: &str, params: Value) -> Value {
    json!({ "jsonrpc": "2.0", "method": method, "params": params })
}

pub fn response(id: &Value, result: Value) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "result": result })
}

pub fn error_response(id: &Value, code: i64, message: &str) -> Value {
    json!({ "jsonrpc": "2.0", "id": id, "error": { "code": code, "message": message } })
}

/// `initialize` params: no file system and no terminal for the agent; rozu's tools come over
/// MCP only.
pub fn initialize_params() -> Value {
    json!({
        "protocolVersion": PROTOCOL_VERSION,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
        },
        "clientInfo": { "name": "rozu", "title": "rozu", "version": env!("CARGO_PKG_VERSION") },
    })
}

/// The auth method to `authenticate` with, if `initialize` offers it: only [`AUTH_METHOD`]. Any
/// other method (an API key, a browser login) is never chosen.
pub fn auth_method(initialize: &Value) -> Option<&'static str> {
    initialize["authMethods"]
        .as_array()
        .into_iter()
        .flatten()
        .any(|m| m["id"] == AUTH_METHOD)
        .then_some(AUTH_METHOD)
}

/// One stdio MCP server for `session/new`.
pub fn mcp_server(name: &str, command: &str, args: &[String], env: &[(String, String)]) -> Value {
    json!({
        "name": name,
        "command": command,
        "args": args,
        "env": env.iter().map(|(k, v)| json!({ "name": k, "value": v })).collect::<Vec<_>>(),
    })
}

/// `session/new` params: rozu's MCP server is the only one; the frozen system profile replaces
/// Grok's prompt (Grok still adds its own priority rules and environment block).
pub fn session_new_params(cwd: &str, mcp_servers: Vec<Value>, system_prompt: &str) -> Value {
    json!({
        "cwd": cwd,
        "mcpServers": mcp_servers,
        "_meta": { "systemPromptOverride": system_prompt },
    })
}

pub fn prompt_params(session_id: &str, text: &str) -> Value {
    json!({ "sessionId": session_id, "prompt": [{ "type": "text", "text": text }] })
}

/// How a Grok usage report counts input tokens.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsageConvention {
    /// ACP `session/prompt` `_meta`: `inputTokens` is the whole prompt, cached part included.
    AcpTotal,
    /// Headless `streaming-json`: `input_tokens` is only the uncached part.
    HeadlessUncached,
}

/// Grok's token counts → rozu's [`Usage`], whose `input_tokens` is always the whole prompt
/// (cached reads included). Grok never reports cache writes on this path, so
/// `cache_write_tokens` is always 0; reported creation tokens (always 0 so far) still count as
/// input under the headless convention.
pub fn normalize_usage(
    convention: UsageConvention,
    input: u64,
    cache_read: u64,
    cache_creation: u64,
    output: u64,
) -> Usage {
    let input_tokens = match convention {
        // Never less than its cached part, even if a report is inconsistent.
        UsageConvention::AcpTotal => input.max(cache_read),
        UsageConvention::HeadlessUncached => input + cache_read + cache_creation,
    };
    Usage {
        input_tokens,
        output_tokens: output,
        cache_read_tokens: cache_read,
        cache_write_tokens: 0,
    }
}

/// The usage in a `session/prompt` result's `_meta` (top-level fields, else `_meta.usage`).
pub fn usage_from_acp_meta(meta: &Value) -> Option<Usage> {
    let source = if meta["inputTokens"].is_u64() {
        meta
    } else if meta["usage"]["inputTokens"].is_u64() {
        &meta["usage"]
    } else {
        return None;
    };
    let n = |key: &str| source[key].as_u64().unwrap_or(0);
    info!(target: "rozu::usage", provider = "grok", usage_raw = %meta,
        reasoning_tokens = n("reasoningTokens"), model = meta["modelId"].as_str().unwrap_or(""),
        "token usage");
    Some(normalize_usage(
        UsageConvention::AcpTotal,
        n("inputTokens"),
        n("cachedReadTokens"),
        n("cacheCreationTokens"),
        n("outputTokens"),
    ))
}

/// The usage of a headless `usage` or `end` event (`input_tokens` uncached).
pub fn usage_from_headless(usage: &Value) -> Usage {
    let n = |key: &str| usage[key].as_u64().unwrap_or(0);
    normalize_usage(
        UsageConvention::HeadlessUncached,
        n("input_tokens"),
        n("cache_read_input_tokens"),
        n("cache_creation_input_tokens"),
        n("output_tokens"),
    )
}

/// A `session/prompt` result's `stopReason` → how the turn ended (`Err(Cancelled)` for
/// `cancelled`).
pub fn stop_reason(reason: Option<&str>) -> Result<StopReason, ProviderError> {
    Ok(match reason {
        Some("cancelled") => return Err(ProviderError::Cancelled),
        Some("max_tokens") => StopReason::MaxTokens,
        Some("refusal") => StopReason::ContentFilter,
        // `max_turn_requests` too: the turn is over, Grok just stopped sampling.
        _ => StopReason::EndTurn,
    })
}

/// A Grok error message as a typed error.
pub fn classify_error(message: &str) -> ProviderError {
    let lower = message.to_lowercase();
    if looks_logged_out(&lower) {
        ProviderError::NotLoggedIn {
            cli: "grok".into(),
            hint: LOGIN_HINT.into(),
        }
    } else if [
        "usage limit",
        "rate limit",
        "quota",
        "429",
        "too many requests",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
    {
        ProviderError::UsageLimit {
            message: message.trim().to_owned(),
        }
    } else {
        ProviderError::Api {
            code: Some("grok".into()),
            message: message.trim().to_owned(),
        }
    }
}

/// Whether Grok output (lowercased) says it isn't signed in.
pub fn looks_logged_out(lower: &str) -> bool {
    [
        "authentication required",
        "not signed in",
        "not logged in",
        "grok login",
        "unauthorized",
        "authentication failed",
    ]
    .iter()
    .any(|needle| lower.contains(needle))
}

/// The tool name rozu shows for a Grok tool call and its arguments: `use_tool` calls of rozu's
/// MCP tools become the tool itself (`rozu__read_file` → `read_file`), anything else is
/// `grok:<name>`.
pub fn tool_call(name: &str, raw_input: &Value) -> (String, Value) {
    if name == USE_TOOL {
        if let Some(target) = raw_input["tool_name"].as_str() {
            let args = raw_input
                .get("tool_input")
                .cloned()
                .unwrap_or_else(|| json!({}));
            return match target.strip_prefix(ROZU_TOOL_PREFIX) {
                Some(tool) => (tool.to_owned(), args),
                None => (format!("grok:{target}"), args),
            };
        }
    }
    (format!("grok:{name}"), raw_input.clone())
}

/// The name of a tool call in a `tool_call` update (`toolName`, else `title`, else `kind`).
pub fn call_name(update: &Value) -> &str {
    ["toolName", "title", "kind"]
        .iter()
        .find_map(|key| update[*key].as_str().filter(|s| !s.is_empty()))
        .unwrap_or("tool")
}

/// A finished tool call's output text and whether it failed.
pub fn tool_output(update: &Value) -> (String, bool) {
    let failed = update["status"] == "failed";
    let raw = &update["rawOutput"];
    let output = &raw["output"];
    if let Some(text) = output.as_str() {
        return (text.to_owned(), failed);
    }
    // MCP output: `{"OkayOutput": "…"}` or an error variant.
    if let Some((key, text)) = output
        .as_object()
        .filter(|o| o.len() == 1)
        .and_then(|o| o.iter().next())
        .and_then(|(k, v)| v.as_str().map(|v| (k, v)))
    {
        return (text.to_owned(), failed || key.contains("Error"));
    }
    let texts: Vec<&str> = update["content"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|c| c["content"]["text"].as_str().or_else(|| c["text"].as_str()))
        .collect();
    if !texts.is_empty() {
        return (texts.join("\n"), failed);
    }
    match raw {
        Value::Null => (String::new(), failed),
        Value::String(text) => (text.clone(), failed),
        other => (other.to_string(), failed),
    }
}

/// Maps one ACP session's updates, turn by turn.
#[derive(Debug, Default)]
pub struct Mapper {
    session_id: String,
    /// Tool calls of the session by id: rozu's name and arguments.
    calls: HashMap<String, (String, Value)>,
    /// Calls whose result was already reported.
    finished: HashSet<String>,
}

impl Mapper {
    pub fn new(session_id: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            ..Self::default()
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// A tool call announced in this session, as rozu names it.
    pub fn call(&self, id: &str) -> Option<&(String, Value)> {
        self.calls.get(id)
    }

    /// The events for one `session/update` notification's params.
    pub fn update(&mut self, params: &Value) -> Vec<StreamEvent> {
        if params["sessionId"]
            .as_str()
            .is_some_and(|id| id != self.session_id)
        {
            debug!(target: "rozu::cli", "grok update for another session");
            return Vec::new();
        }
        self.map_update(&params["update"])
    }

    /// The events for one update object (the `update` of ACP, or a headless event with the same
    /// fields).
    pub fn map_update(&mut self, update: &Value) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        let kind = update["sessionUpdate"]
            .as_str()
            .or_else(|| update["type"].as_str())
            .unwrap_or_default();
        match kind {
            "agent_message_chunk" => {
                let text = update["content"]["text"].as_str().unwrap_or_default();
                if !text.is_empty() {
                    out.push(StreamEvent::TextDelta(text.to_owned()));
                }
            }
            "tool_call" => {
                let Some(id) = update["toolCallId"].as_str() else {
                    return out;
                };
                if !self.calls.contains_key(id) {
                    let (name, args) = tool_call(call_name(update), &update["rawInput"]);
                    push_call(&mut out, id, &name, &args);
                    self.calls.insert(id.to_owned(), (name, args));
                }
                self.finish(update, id, &mut out);
            }
            "tool_call_update" => {
                let Some(id) = update["toolCallId"].as_str() else {
                    return out;
                };
                if !self.calls.contains_key(id) {
                    let (name, args) = tool_call(call_name(update), &update["rawInput"]);
                    push_call(&mut out, id, &name, &args);
                    self.calls.insert(id.to_owned(), (name, args));
                }
                self.finish(update, id, &mut out);
            }
            other => debug!(target: "rozu::cli", update = other, "ignored grok update"),
        }
        out
    }

    fn finish(&mut self, update: &Value, id: &str, out: &mut Vec<StreamEvent>) {
        if !matches!(update["status"].as_str(), Some("completed" | "failed")) {
            return;
        }
        if self.finished.insert(id.to_owned()) {
            let (output, is_error) = tool_output(update);
            out.push(StreamEvent::ToolResult {
                id: id.to_owned(),
                output,
                is_error,
            });
        }
    }
}

fn push_call(out: &mut Vec<StreamEvent>, id: &str, name: &str, args: &Value) {
    out.push(StreamEvent::ToolCallStart {
        id: id.to_owned(),
        name: name.to_owned(),
    });
    out.push(StreamEvent::ToolCallArgsDelta {
        id: id.to_owned(),
        json: args.to_string(),
    });
    out.push(StreamEvent::ToolCallEnd { id: id.to_owned() });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_is_normalized_to_total_input_for_both_conventions() {
        // The same request, as ACP and headless report it.
        let acp = normalize_usage(UsageConvention::AcpTotal, 4591, 1152, 0, 111);
        let headless = normalize_usage(UsageConvention::HeadlessUncached, 3439, 1152, 0, 111);
        let want = Usage {
            input_tokens: 4591,
            output_tokens: 111,
            cache_read_tokens: 1152,
            cache_write_tokens: 0,
        };
        assert_eq!(acp, want);
        assert_eq!(headless, want);
        // Creation tokens are input on the headless path, but never cache writes.
        let created = normalize_usage(UsageConvention::HeadlessUncached, 10, 5, 7, 1);
        assert_eq!((created.input_tokens, created.cache_write_tokens), (22, 0));
        // An inconsistent ACP report never yields less input than was read from the cache.
        assert_eq!(
            normalize_usage(UsageConvention::AcpTotal, 10, 50, 0, 1).input_tokens,
            50
        );
    }

    #[test]
    fn usage_from_acp_meta_and_headless_events() {
        // From a Grok Build 1.0.52 ACP transcript (ids dropped).
        let meta = json!({"totalTokens": 4711, "modelId": "grok-4.7", "inputTokens": 4591,
            "outputTokens": 111, "cachedReadTokens": 1152, "reasoningTokens": 110,
            "usage": {"inputTokens": 4591, "outputTokens": 111, "cachedReadTokens": 1152,
                "cacheCreationTokens": 0, "costUsdTicks": 81200000}});
        let usage = usage_from_acp_meta(&meta).unwrap();
        assert_eq!(
            (
                usage.input_tokens,
                usage.cache_read_tokens,
                usage.output_tokens
            ),
            (4591, 1152, 111)
        );
        let nested =
            json!({"usage": {"inputTokens": 20, "cachedReadTokens": 5, "outputTokens": 2}});
        assert_eq!(usage_from_acp_meta(&nested).unwrap().input_tokens, 20);
        assert_eq!(usage_from_acp_meta(&json!({})), None);
        // From a headless `end` event: 4220 uncached + 6144 cached.
        let end = json!({"input_tokens": 4220, "cache_read_input_tokens": 6144,
            "cache_creation_input_tokens": 0, "output_tokens": 448, "reasoning_tokens": 376});
        assert_eq!(
            usage_from_headless(&end),
            Usage {
                input_tokens: 10364,
                output_tokens: 448,
                cache_read_tokens: 6144,
                cache_write_tokens: 0,
            }
        );
    }

    #[test]
    fn picks_only_the_cached_token_auth_method() {
        assert_eq!(
            auth_method(&json!({"authMethods": [{"id": "xai_api_key"}, {"id": "cached_token"}]})),
            Some("cached_token")
        );
        assert_eq!(
            auth_method(&json!({"authMethods": [{"id": "grok.com", "name": "Grok"}]})),
            None
        );
        assert_eq!(auth_method(&json!({})), None);
    }

    #[test]
    fn errors_and_stop_reasons_are_typed() {
        let err = classify_error("Authentication required");
        assert!(err.to_string().contains("rozu auth login grok"), "{err}");
        assert!(matches!(
            classify_error("weekly usage limit reached"),
            ProviderError::UsageLimit { .. }
        ));
        assert!(matches!(
            classify_error("boom"),
            ProviderError::Api { code: Some(c), .. } if c == "grok"
        ));
        for (reason, want) in [
            ("end_turn", Ok(StopReason::EndTurn)),
            ("max_turn_requests", Ok(StopReason::EndTurn)),
            ("max_tokens", Ok(StopReason::MaxTokens)),
            ("cancelled", Err(ProviderError::Cancelled)),
        ] {
            assert_eq!(stop_reason(Some(reason)), want, "{reason}");
        }
    }

    #[test]
    fn maps_use_tool_calls_to_rozu_tools() {
        let mut mapper = Mapper::new("s1");
        let mut events = Vec::new();
        for update in [
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "Hi"}}),
            json!({"sessionUpdate": "agent_thought_chunk", "content": {"type": "text", "text": "hmm"}}),
            json!({"sessionUpdate": "tool_call", "toolCallId": "c1", "title": "use_tool", "kind": "use_tool", "status": "pending",
                "rawInput": {"tool_name": "rozu__read_file", "tool_input": {"path": "README.md"}}}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": null}),
            json!({"sessionUpdate": "tool_call_update", "toolCallId": "c1", "status": "completed",
                "rawOutput": {"type": "MCP", "tool_name": "read_file", "server_name": "rozu", "output": {"OkayOutput": "# demo"}}}),
            json!({"sessionUpdate": "tool_call", "toolCallId": "c2", "title": "search_tool", "status": "completed",
                "rawInput": {"query": "x"}, "content": [{"type": "content", "content": {"type": "text", "text": "found"}}]}),
        ] {
            events.extend(mapper.update(&json!({"sessionId": "s1", "update": update})));
        }
        // Another session's updates are skipped.
        events.extend(mapper.update(&json!({"sessionId": "s0", "update":
            {"sessionUpdate": "agent_message_chunk", "content": {"text": "x"}}})));
        assert_eq!(
            events,
            [
                StreamEvent::TextDelta("Hi".into()),
                StreamEvent::ToolCallStart {
                    id: "c1".into(),
                    name: "read_file".into()
                },
                StreamEvent::ToolCallArgsDelta {
                    id: "c1".into(),
                    json: r#"{"path":"README.md"}"#.into()
                },
                StreamEvent::ToolCallEnd { id: "c1".into() },
                StreamEvent::ToolResult {
                    id: "c1".into(),
                    output: "# demo".into(),
                    is_error: false
                },
                StreamEvent::ToolCallStart {
                    id: "c2".into(),
                    name: "grok:search_tool".into()
                },
                StreamEvent::ToolCallArgsDelta {
                    id: "c2".into(),
                    json: r#"{"query":"x"}"#.into()
                },
                StreamEvent::ToolCallEnd { id: "c2".into() },
                StreamEvent::ToolResult {
                    id: "c2".into(),
                    output: "found".into(),
                    is_error: false
                },
            ]
        );
        assert_eq!(mapper.call("c1").unwrap().0, "read_file");
    }

    #[test]
    fn tool_outputs_and_failures() {
        let failed = json!({"status": "failed", "rawOutput": {"output": {"ErrorOutput": "nope"}}});
        assert_eq!(tool_output(&failed), ("nope".into(), true));
        let error_variant =
            json!({"status": "completed", "rawOutput": {"output": {"ErrorOutput": "bad"}}});
        assert_eq!(tool_output(&error_variant), ("bad".into(), true));
        let raw = json!({"status": "completed", "rawOutput": {"x": 1}});
        assert_eq!(tool_output(&raw), (r#"{"x":1}"#.into(), false));
        assert_eq!(
            tool_call(
                "use_tool",
                &json!({"tool_name": "other__t", "tool_input": {"a": 1}})
            ),
            ("grok:other__t".into(), json!({"a": 1}))
        );
    }
}
