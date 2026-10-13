//! Anthropic Messages API adapter (`POST {base_url}/v1/messages` with `stream: true`).
//!
//! System messages become the top-level `system` field, tool calls are `tool_use` content blocks
//! and tool results go back as `tool_result` blocks in a user message. Consecutive messages with
//! the same wire role are merged, since the API expects user and assistant turns to alternate.
//! Prompt caching marks the system prompt and the last tool definition with
//! `cache_control: {type: ephemeral}`, so the stable prefix of every request is cached.
//!
//! Auth is an API key (`x-api-key`) only; Claude subscription logins are out of scope.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use reqwest::header::{ACCEPT, HeaderValue};
use rozu_core::{
    ChatRequest, ContentPart, EventStream, ModelInfo, Provider, ProviderError, ProviderResult,
    Role, StopReason, StreamEvent, Usage,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Map, Value, json};
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::retry::{RetryPolicy, send_with_retry};
use crate::sse::{self, Decoder};

pub const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
pub const PROVIDER_ID: &str = "anthropic";
/// Value of the `anthropic-version` header.
pub const API_VERSION: &str = "2023-06-01";
/// `max_tokens` is required by the Messages API; used when the request doesn't set one.
pub const DEFAULT_MAX_TOKENS: u32 = 8192;

#[derive(Debug)]
pub struct AnthropicConfig {
    /// Never logged: `SecretString`'s `Debug` prints a placeholder.
    pub api_key: SecretString,
    /// API root without `/v1` or a trailing slash, e.g. `https://api.anthropic.com`.
    pub base_url: String,
    pub retry: RetryPolicy,
    pub connect_timeout: Duration,
    /// Longest silence tolerated between two chunks of a response.
    pub read_timeout: Duration,
    pub default_max_tokens: u32,
}

impl AnthropicConfig {
    pub fn new(api_key: SecretString) -> Self {
        Self {
            api_key,
            base_url: DEFAULT_BASE_URL.to_owned(),
            retry: RetryPolicy::default(),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(300),
            default_max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    pub fn with_base_url(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').to_owned();
        self
    }

    pub fn with_retry(mut self, retry: RetryPolicy) -> Self {
        self.retry = retry;
        self
    }
}

#[derive(Debug)]
pub struct AnthropicProvider {
    client: reqwest::Client,
    config: AnthropicConfig,
    endpoint: String,
}

impl AnthropicProvider {
    pub fn new(config: AnthropicConfig) -> ProviderResult<Self> {
        let client = sse::http_client(config.connect_timeout, config.read_timeout)?;
        let endpoint = format!("{}/v1/messages", config.base_url);
        Ok(Self {
            client,
            config,
            endpoint,
        })
    }

    fn key_header(&self) -> ProviderResult<HeaderValue> {
        let mut value =
            HeaderValue::from_str(self.config.api_key.expose_secret()).map_err(|_| {
                ProviderError::Auth {
                    status: 0,
                    message: "the API key contains characters that can't go in a header".into(),
                }
            })?;
        value.set_sensitive(true);
        Ok(value)
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> ProviderResult<EventStream> {
        let body = request_body(&req, self.config.default_max_tokens);
        debug!(model = %req.model, messages = req.messages.len(), "POST {}", self.endpoint);
        let response = send_with_retry(self.config.retry, PROVIDER_ID, &cancel, || {
            Ok(self
                .client
                .post(&self.endpoint)
                .header("x-api-key", self.key_header()?)
                .header("anthropic-version", API_VERSION)
                .header(ACCEPT, "text/event-stream")
                .json(&body))
        })
        .await?;
        Ok(sse::decode(response, SseState::default(), cancel).boxed())
    }
}

/// Builds the Messages API request body for `req`.
pub fn request_body(req: &ChatRequest, default_max_tokens: u32) -> Value {
    let mut system = Vec::new();
    let mut messages: Vec<(&str, Vec<Value>)> = Vec::new();
    for message in &req.messages {
        let role = match message.role {
            Role::System => {
                let text = message.text_content();
                if !text.is_empty() {
                    system.push(json!({"type": "text", "text": text}));
                }
                continue;
            }
            Role::Assistant => "assistant",
            Role::User | Role::Tool => "user",
        };
        let blocks: Vec<Value> = message.content.iter().filter_map(content_block).collect();
        if blocks.is_empty() {
            continue;
        }
        match messages.last_mut() {
            Some((last_role, content)) if *last_role == role => content.extend(blocks),
            _ => messages.push((role, blocks)),
        }
    }

    let mut body = json!({
        "model": req.model,
        "max_tokens": req.max_output_tokens.unwrap_or(default_max_tokens),
        "stream": true,
        "messages": messages
            .into_iter()
            .map(|(role, content)| json!({"role": role, "content": content}))
            .collect::<Vec<_>>(),
    });
    if let Some(last) = system.last_mut() {
        last["cache_control"] = json!({"type": "ephemeral"});
        body["system"] = system.into();
    }
    if !req.tools.is_empty() {
        let mut tools: Vec<Value> = req
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "name": tool.name,
                    "description": tool.description,
                    "input_schema": tool.json_schema,
                })
            })
            .collect();
        if let Some(last) = tools.last_mut() {
            last["cache_control"] = json!({"type": "ephemeral"});
        }
        body["tools"] = tools.into();
    }
    body
}

fn content_block(part: &ContentPart) -> Option<Value> {
    Some(match part {
        // The API rejects empty text blocks.
        ContentPart::Text { text } if text.is_empty() => return None,
        ContentPart::Text { text } => json!({"type": "text", "text": text}),
        ContentPart::ToolCall {
            id,
            name,
            args_json,
        } => {
            // `input` must be an object; anything else (empty or broken JSON) is sent as `{}`.
            let input = serde_json::from_str::<Value>(args_json)
                .ok()
                .filter(Value::is_object)
                .unwrap_or_else(|| Value::Object(Map::new()));
            json!({"type": "tool_use", "id": id, "name": name, "input": input})
        }
        ContentPart::ToolResult {
            call_id,
            output,
            is_error,
        } => {
            let mut block = json!({"type": "tool_result", "tool_use_id": call_id});
            if !output.is_empty() {
                block["content"] = output.as_str().into();
            }
            if *is_error {
                block["is_error"] = true.into();
            }
            block
        }
    })
}

/// Raw token counts as Anthropic reports them: `input_tokens` excludes cache reads and writes.
#[derive(Debug, Default)]
struct RawUsage {
    input: u64,
    cache_read: u64,
    cache_write: u64,
    output: u64,
}

impl RawUsage {
    /// Takes every count present in `usage`. `message_delta` counts are cumulative, so a later
    /// value replaces an earlier one.
    fn update(&mut self, usage: &Value) {
        let set = |field: &mut u64, key: &str| {
            if let Some(n) = usage[key].as_u64() {
                *field = n;
            }
        };
        set(&mut self.input, "input_tokens");
        set(&mut self.cache_read, "cache_read_input_tokens");
        set(&mut self.cache_write, "cache_creation_input_tokens");
        set(&mut self.output, "output_tokens");
    }

    /// The normalized form, where `input_tokens` includes cache reads and writes.
    fn normalized(&self) -> Usage {
        Usage {
            input_tokens: self.input + self.cache_read + self.cache_write,
            output_tokens: self.output,
            cache_read_tokens: self.cache_read,
            cache_write_tokens: self.cache_write,
        }
    }
}

/// Turns Messages API stream events into [`StreamEvent`]s.
#[derive(Debug, Default)]
struct SseState {
    /// Tool-use ids by content block index.
    tool_blocks: HashMap<u64, String>,
    saw_tool_use: bool,
    usage: RawUsage,
    stop_reason: Option<StopReason>,
    done: bool,
}

impl Decoder for SseState {
    fn handle(&mut self, data: &str) -> ProviderResult<Vec<StreamEvent>> {
        let event: Value = serde_json::from_str(data)
            .map_err(|err| ProviderError::Decode(format!("{err} in event: {data}")))?;
        let str_field = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
        let index = event["index"].as_u64().unwrap_or(0);
        let mut out = Vec::new();

        match event["type"].as_str().unwrap_or_default() {
            "message_start" => self.usage.update(&event["message"]["usage"]),
            "content_block_start" => {
                let block = &event["content_block"];
                match block["type"].as_str().unwrap_or_default() {
                    "text" => {
                        let text = str_field(block, "text");
                        if !text.is_empty() {
                            out.push(StreamEvent::TextDelta(text));
                        }
                    }
                    "tool_use" => {
                        let id = str_field(block, "id");
                        self.saw_tool_use = true;
                        out.push(StreamEvent::ToolCallStart {
                            id: id.clone(),
                            name: str_field(block, "name"),
                        });
                        // Input arrives as `input_json_delta`s; a non-empty start object (not
                        // seen in practice) is sent whole.
                        if block["input"].as_object().is_some_and(|o| !o.is_empty()) {
                            out.push(StreamEvent::ToolCallArgsDelta {
                                id: id.clone(),
                                json: block["input"].to_string(),
                            });
                        }
                        self.tool_blocks.insert(index, id);
                    }
                    // Thinking and server-tool blocks aren't surfaced yet.
                    _ => {}
                }
            }
            "content_block_delta" => {
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => out.push(StreamEvent::TextDelta(str_field(delta, "text"))),
                    "input_json_delta" => {
                        let json = str_field(delta, "partial_json");
                        if let (Some(id), false) = (self.tool_blocks.get(&index), json.is_empty()) {
                            out.push(StreamEvent::ToolCallArgsDelta {
                                id: id.clone(),
                                json,
                            });
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                if let Some(id) = self.tool_blocks.remove(&index) {
                    out.push(StreamEvent::ToolCallEnd { id });
                }
            }
            "message_delta" => {
                self.usage.update(&event["usage"]);
                if let Some(reason) = event["delta"]["stop_reason"].as_str() {
                    self.stop_reason = Some(match reason {
                        "tool_use" => StopReason::ToolUse,
                        "max_tokens" | "model_context_window_exceeded" => StopReason::MaxTokens,
                        "refusal" => StopReason::ContentFilter,
                        // end_turn, stop_sequence, pause_turn
                        _ => StopReason::EndTurn,
                    });
                }
            }
            "message_stop" => {
                out.push(StreamEvent::Usage(self.usage.normalized()));
                let reason = self.stop_reason.unwrap_or(if self.saw_tool_use {
                    StopReason::ToolUse
                } else {
                    StopReason::EndTurn
                });
                out.push(StreamEvent::Done(reason));
                self.done = true;
            }
            "error" => {
                let error = &event["error"];
                return Err(ProviderError::Api {
                    code: error["type"].as_str().map(str::to_owned),
                    message: error["message"]
                        .as_str()
                        .unwrap_or("unknown stream error")
                        .to_owned(),
                });
            }
            // "ping" and anything new.
            _ => {}
        }
        Ok(out)
    }

    fn is_done(&self) -> bool {
        self.done
    }
}

/// Known Anthropic models: (id, context window, USD per 1M input / output / cache-read tokens).
/// Cache writes cost 1.25x input (the 5-minute TTL price).
// Prices NEED VERIFICATION against https://www.anthropic.com/pricing before relying on cost
// totals; they are list prices as last checked by hand and change without notice.
pub fn models() -> Vec<ModelInfo> {
    crate::registry::model_table(
        PROVIDER_ID,
        1.25,
        &[
            ("claude-sonnet-4-5", 200_000, 3.0, 15.0, 0.30),
            ("claude-opus-4-1", 200_000, 15.0, 75.0, 1.50),
            ("claude-opus-4-5", 200_000, 5.0, 25.0, 0.50),
            ("claude-haiku-4-5", 200_000, 1.0, 5.0, 0.10),
        ],
    )
}

#[cfg(test)]
mod tests {
    use rozu_core::{Message, ToolSpec};

    use super::*;

    #[test]
    fn builds_request_body() {
        let req = ChatRequest {
            model: "claude-sonnet-4-5".into(),
            messages: vec![
                Message::system("be brief"),
                Message::user("hi"),
                Message::new(
                    Role::Assistant,
                    vec![
                        ContentPart::Text {
                            text: "let me look".into(),
                        },
                        ContentPart::ToolCall {
                            id: "toolu_1".into(),
                            name: "list_dir".into(),
                            args_json: String::new(),
                        },
                        ContentPart::ToolCall {
                            id: "toolu_2".into(),
                            name: "grep".into(),
                            args_json: r#"{"pattern":"x"}"#.into(),
                        },
                    ],
                ),
                Message::tool_result("toolu_1", "README.md", false),
                Message::tool_result("toolu_2", "no such file", true),
                Message::user("thanks"),
            ],
            tools: vec![
                ToolSpec {
                    name: "list_dir".into(),
                    description: "List".into(),
                    json_schema: json!({"type": "object"}),
                },
                ToolSpec {
                    name: "grep".into(),
                    description: "Search".into(),
                    json_schema: json!({"type": "object"}),
                },
            ],
            max_output_tokens: None,
        };
        assert_eq!(
            request_body(&req, 8192),
            json!({
                "model": "claude-sonnet-4-5",
                "max_tokens": 8192,
                "stream": true,
                "system": [{"type": "text", "text": "be brief", "cache_control": {"type": "ephemeral"}}],
                "messages": [
                    {"role": "user", "content": [{"type": "text", "text": "hi"}]},
                    {"role": "assistant", "content": [
                        {"type": "text", "text": "let me look"},
                        {"type": "tool_use", "id": "toolu_1", "name": "list_dir", "input": {}},
                        {"type": "tool_use", "id": "toolu_2", "name": "grep", "input": {"pattern": "x"}},
                    ]},
                    // Both tool results and the next user text share one user turn.
                    {"role": "user", "content": [
                        {"type": "tool_result", "tool_use_id": "toolu_1", "content": "README.md"},
                        {"type": "tool_result", "tool_use_id": "toolu_2", "content": "no such file", "is_error": true},
                        {"type": "text", "text": "thanks"},
                    ]},
                ],
                "tools": [
                    {"name": "list_dir", "description": "List", "input_schema": {"type": "object"}},
                    {"name": "grep", "description": "Search", "input_schema": {"type": "object"},
                     "cache_control": {"type": "ephemeral"}},
                ],
            })
        );
    }

    #[test]
    fn request_without_system_or_tools_has_no_cache_markers() {
        let req = ChatRequest {
            model: "m".into(),
            messages: vec![Message::user("hi")],
            tools: vec![],
            max_output_tokens: Some(100),
        };
        let body = request_body(&req, 8192);
        assert_eq!(body["max_tokens"], 100);
        assert!(body.get("system").is_none());
        assert!(body.get("tools").is_none());
    }

    fn feed(state: &mut SseState, events: &[Value]) -> Vec<StreamEvent> {
        events
            .iter()
            .flat_map(|e| state.handle(&e.to_string()).unwrap())
            .collect()
    }

    #[test]
    fn maps_tool_use_and_usage() {
        let mut state = SseState::default();
        let out = feed(
            &mut state,
            &[
                json!({"type": "message_start", "message": {"usage": {"input_tokens": 10, "cache_read_input_tokens": 90, "cache_creation_input_tokens": 5, "output_tokens": 1}}}),
                json!({"type": "content_block_start", "index": 0, "content_block": {"type": "text", "text": ""}}),
                json!({"type": "content_block_delta", "index": 0, "delta": {"type": "text_delta", "text": "Looking"}}),
                json!({"type": "content_block_stop", "index": 0}),
                json!({"type": "ping"}),
                json!({"type": "content_block_start", "index": 1, "content_block": {"type": "tool_use", "id": "toolu_1", "name": "grep", "input": {}}}),
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": ""}}),
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "{\"pattern\":"}}),
                json!({"type": "content_block_delta", "index": 1, "delta": {"type": "input_json_delta", "partial_json": "\"x\"}"}}),
                json!({"type": "content_block_stop", "index": 1}),
                json!({"type": "message_delta", "delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 30}}),
                json!({"type": "message_stop"}),
            ],
        );
        assert_eq!(
            out,
            [
                StreamEvent::TextDelta("Looking".into()),
                StreamEvent::ToolCallStart {
                    id: "toolu_1".into(),
                    name: "grep".into()
                },
                StreamEvent::ToolCallArgsDelta {
                    id: "toolu_1".into(),
                    json: "{\"pattern\":".into()
                },
                StreamEvent::ToolCallArgsDelta {
                    id: "toolu_1".into(),
                    json: "\"x\"}".into()
                },
                StreamEvent::ToolCallEnd {
                    id: "toolu_1".into()
                },
                StreamEvent::Usage(Usage {
                    input_tokens: 105,
                    output_tokens: 30,
                    cache_read_tokens: 90,
                    cache_write_tokens: 5
                }),
                StreamEvent::Done(StopReason::ToolUse),
            ]
        );
        assert!(state.done);
    }

    #[test]
    fn maps_stop_reasons_and_errors() {
        for (reason, expected) in [
            ("end_turn", StopReason::EndTurn),
            ("stop_sequence", StopReason::EndTurn),
            ("max_tokens", StopReason::MaxTokens),
            ("refusal", StopReason::ContentFilter),
        ] {
            let out = feed(
                &mut SseState::default(),
                &[
                    json!({"type": "message_delta", "delta": {"stop_reason": reason}}),
                    json!({"type": "message_stop"}),
                ],
            );
            assert_eq!(out.last(), Some(&StreamEvent::Done(expected)), "{reason}");
        }

        let err = SseState::default()
            .handle(&json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}).to_string())
            .unwrap_err();
        assert_eq!(
            err,
            ProviderError::Api {
                code: Some("overloaded_error".into()),
                message: "Overloaded".into()
            }
        );
        assert!(matches!(
            SseState::default().handle("not json"),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn debug_does_not_leak_key() {
        let config = AnthropicConfig::new(SecretString::from("sk-ant-supersecretvalue123"));
        let provider = AnthropicProvider::new(config).unwrap();
        let debug = format!("{provider:?}");
        assert!(!debug.contains("supersecret"), "{debug}");
    }
}
