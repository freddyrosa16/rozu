//! OpenAI Responses API adapter (`POST {base_url}/responses` with `stream: true`).
//!
//! Requests are stateless (`store: false`): the whole conversation is sent each time, with tool
//! calls as `function_call` items and tool results as `function_call_output` items. The base URL
//! is configurable so tests can point at wiremock and xAI can reuse the adapter later.

use std::collections::HashMap;
use std::time::Duration;

use async_trait::async_trait;
use eventsource_stream::Eventsource;
use futures::{Stream, StreamExt};
use reqwest::header::{ACCEPT, AUTHORIZATION, HeaderValue};
use reqwest::{Response, StatusCode};
use rozu_core::{
    ChatRequest, ContentPart, EventStream, ModelInfo, Provider, ProviderError, ProviderResult,
    Role, StopReason, StreamEvent, Usage,
};
use secrecy::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::retry::RetryPolicy;

pub const DEFAULT_BASE_URL: &str = "https://api.openai.com/v1";
pub const PROVIDER_ID: &str = "openai";

#[derive(Debug)]
pub struct OpenAiConfig {
    /// Never logged: `SecretString`'s `Debug` prints a placeholder.
    pub api_key: SecretString,
    /// API root without a trailing slash, e.g. `https://api.openai.com/v1`.
    pub base_url: String,
    pub provider_id: String,
    pub retry: RetryPolicy,
    pub connect_timeout: Duration,
    /// Longest silence tolerated between two chunks of a response.
    pub read_timeout: Duration,
}

impl OpenAiConfig {
    pub fn new(api_key: SecretString) -> Self {
        Self {
            api_key,
            base_url: DEFAULT_BASE_URL.to_owned(),
            provider_id: PROVIDER_ID.to_owned(),
            retry: RetryPolicy::default(),
            connect_timeout: Duration::from_secs(15),
            read_timeout: Duration::from_secs(300),
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
pub struct OpenAiProvider {
    client: reqwest::Client,
    config: OpenAiConfig,
    endpoint: String,
}

impl OpenAiProvider {
    pub fn new(config: OpenAiConfig) -> ProviderResult<Self> {
        let client = reqwest::Client::builder()
            .user_agent(concat!("rozu/", env!("CARGO_PKG_VERSION")))
            .connect_timeout(config.connect_timeout)
            .read_timeout(config.read_timeout)
            .build()
            .map_err(|err| ProviderError::Transport(err.to_string()))?;
        let endpoint = format!("{}/responses", config.base_url);
        Ok(Self {
            client,
            config,
            endpoint,
        })
    }

    fn auth_header(&self) -> ProviderResult<HeaderValue> {
        let mut value =
            HeaderValue::from_str(&format!("Bearer {}", self.config.api_key.expose_secret()))
                .map_err(|_| ProviderError::Auth {
                    status: 0,
                    message: "the API key contains characters that can't go in a header".into(),
                })?;
        value.set_sensitive(true);
        Ok(value)
    }

    /// Sends the request, retrying 429 / 5xx / connection failures per the retry policy.
    async fn send(&self, body: &Value, cancel: &CancellationToken) -> ProviderResult<Response> {
        let policy = self.config.retry;
        let mut attempt = 0;
        loop {
            let request = self
                .client
                .post(&self.endpoint)
                .header(AUTHORIZATION, self.auth_header()?)
                .header(ACCEPT, "text/event-stream")
                .json(body)
                .send();
            let result = tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                result = request => result,
            };

            let delay = match result {
                Ok(response) if response.status().is_success() => return Ok(response),
                Ok(response) => {
                    let status = response.status();
                    let retryable =
                        status == StatusCode::TOO_MANY_REQUESTS || status.is_server_error();
                    if !retryable || attempt >= policy.max_retries {
                        return Err(error_from_response(response, attempt + 1).await);
                    }
                    let delay = policy
                        .retry_after(response.headers())
                        .unwrap_or_else(|| policy.backoff(attempt));
                    warn!(%status, attempt = attempt + 1, ?delay, "retrying OpenAI request");
                    delay
                }
                Err(err)
                    if (err.is_connect() || err.is_timeout()) && attempt < policy.max_retries =>
                {
                    let delay = policy.backoff(attempt);
                    warn!(error = %err, attempt = attempt + 1, ?delay, "retrying OpenAI request");
                    delay
                }
                Err(err) => return Err(ProviderError::Transport(err.to_string())),
            };
            attempt += 1;
            tokio::select! {
                biased;
                _ = cancel.cancelled() => return Err(ProviderError::Cancelled),
                _ = tokio::time::sleep(delay) => {}
            }
        }
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn id(&self) -> &str {
        &self.config.provider_id
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> ProviderResult<EventStream> {
        let body = request_body(&req);
        debug!(model = %req.model, messages = req.messages.len(), "POST {}", self.endpoint);
        let response = self.send(&body, &cancel).await?;
        Ok(parse_sse(response, cancel).boxed())
    }
}

/// Builds the Responses API request body for `req`.
pub fn request_body(req: &ChatRequest) -> Value {
    let mut input = Vec::new();
    for message in &req.messages {
        match message.role {
            Role::System | Role::User => {
                let role = if message.role == Role::System {
                    "system"
                } else {
                    "user"
                };
                input.push(json!({"role": role, "content": message.text_content()}));
            }
            Role::Assistant => {
                for part in &message.content {
                    match part {
                        ContentPart::Text { text } => {
                            input.push(json!({"role": "assistant", "content": text}));
                        }
                        ContentPart::ToolCall {
                            id,
                            name,
                            args_json,
                        } => {
                            let arguments = if args_json.trim().is_empty() {
                                "{}"
                            } else {
                                args_json
                            };
                            input.push(json!({
                                "type": "function_call",
                                "call_id": id,
                                "name": name,
                                "arguments": arguments,
                            }));
                        }
                        ContentPart::ToolResult { .. } => {}
                    }
                }
            }
            Role::Tool => {
                for part in &message.content {
                    if let ContentPart::ToolResult {
                        call_id, output, ..
                    } = part
                    {
                        input.push(json!({
                            "type": "function_call_output",
                            "call_id": call_id,
                            "output": output,
                        }));
                    }
                }
            }
        }
    }

    let mut body = json!({
        "model": req.model,
        "input": input,
        "stream": true,
        "store": false,
    });
    if !req.tools.is_empty() {
        body["tools"] = req
            .tools
            .iter()
            .map(|tool| {
                json!({
                    "type": "function",
                    "name": tool.name,
                    "description": tool.description,
                    "parameters": tool.json_schema,
                    "strict": false,
                })
            })
            .collect();
    }
    if let Some(max) = req.max_output_tokens {
        body["max_output_tokens"] = max.into();
    }
    body
}

async fn error_from_response(response: Response, attempts: u32) -> ProviderError {
    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    let message = serde_json::from_str::<Value>(&text)
        .ok()
        .and_then(|v| v["error"]["message"].as_str().map(str::to_owned))
        .unwrap_or_else(|| {
            if text.is_empty() {
                status
                    .canonical_reason()
                    .unwrap_or("request failed")
                    .to_owned()
            } else {
                text
            }
        });
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => ProviderError::Auth {
            status: status.as_u16(),
            message,
        },
        StatusCode::TOO_MANY_REQUESTS => ProviderError::RateLimited { attempts, message },
        _ => ProviderError::Http {
            status: status.as_u16(),
            message,
        },
    }
}

fn parse_sse(
    response: Response,
    cancel: CancellationToken,
) -> impl Stream<Item = ProviderResult<StreamEvent>> + Send + 'static {
    async_stream::stream! {
        let mut events = response.bytes_stream().eventsource();
        let mut state = SseState::default();
        loop {
            let next = tokio::select! {
                biased;
                _ = cancel.cancelled() => return,
                next = events.next() => next,
            };
            match next {
                None => {
                    yield Err(ProviderError::UnexpectedEof);
                    return;
                }
                Some(Err(err)) => {
                    yield Err(ProviderError::Transport(err.to_string()));
                    return;
                }
                Some(Ok(event)) => {
                    if event.data.trim() == "[DONE]" {
                        continue;
                    }
                    match state.handle(&event.data) {
                        Ok(out) => {
                            for item in out {
                                yield Ok(item);
                            }
                            if state.done {
                                return;
                            }
                        }
                        Err(err) => {
                            yield Err(err);
                            return;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug)]
struct CallState {
    call_id: String,
    saw_delta: bool,
    ended: bool,
}

/// Turns Responses API stream events into [`StreamEvent`]s.
#[derive(Debug, Default)]
struct SseState {
    /// Function calls by output item id (`fc_…`); the model-facing id is the `call_id`.
    calls: HashMap<String, CallState>,
    saw_tool_call: bool,
    done: bool,
}

impl SseState {
    fn handle(&mut self, data: &str) -> ProviderResult<Vec<StreamEvent>> {
        let event: Value = serde_json::from_str(data)
            .map_err(|err| ProviderError::Decode(format!("{err} in event: {data}")))?;
        let str_field = |v: &Value, key: &str| v[key].as_str().unwrap_or_default().to_owned();
        let mut out = Vec::new();

        match event["type"].as_str().unwrap_or_default() {
            "response.output_text.delta" => {
                out.push(StreamEvent::TextDelta(str_field(&event, "delta")));
            }
            "response.output_item.added" if event["item"]["type"] == "function_call" => {
                let item = &event["item"];
                let item_id = str_field(item, "id");
                let call_id = item["call_id"]
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| item_id.clone());
                self.saw_tool_call = true;
                out.push(StreamEvent::ToolCallStart {
                    id: call_id.clone(),
                    name: str_field(item, "name"),
                });
                let arguments = str_field(item, "arguments");
                let saw_delta = !arguments.is_empty();
                if saw_delta {
                    out.push(StreamEvent::ToolCallArgsDelta {
                        id: call_id.clone(),
                        json: arguments,
                    });
                }
                self.calls.insert(
                    item_id,
                    CallState {
                        call_id,
                        saw_delta,
                        ended: false,
                    },
                );
            }
            "response.function_call_arguments.delta" => {
                if let Some(call) = self
                    .calls
                    .get_mut(event["item_id"].as_str().unwrap_or_default())
                {
                    call.saw_delta = true;
                    out.push(StreamEvent::ToolCallArgsDelta {
                        id: call.call_id.clone(),
                        json: str_field(&event, "delta"),
                    });
                }
            }
            "response.function_call_arguments.done" => {
                let item_id = str_field(&event, "item_id");
                self.end_call(&item_id, &str_field(&event, "arguments"), &mut out);
            }
            "response.output_item.done" if event["item"]["type"] == "function_call" => {
                let item = &event["item"];
                self.end_call(
                    &str_field(item, "id"),
                    &str_field(item, "arguments"),
                    &mut out,
                );
            }
            "response.completed" | "response.incomplete" => {
                let response = &event["response"];
                let usage = &response["usage"];
                if usage.is_object() {
                    out.push(StreamEvent::Usage(Usage {
                        input_tokens: usage["input_tokens"].as_u64().unwrap_or(0),
                        output_tokens: usage["output_tokens"].as_u64().unwrap_or(0),
                        cached_input_tokens: usage["input_tokens_details"]["cached_tokens"]
                            .as_u64()
                            .unwrap_or(0),
                    }));
                }
                let reason = match response["incomplete_details"]["reason"].as_str() {
                    Some("max_output_tokens") => StopReason::MaxTokens,
                    Some("content_filter") => StopReason::ContentFilter,
                    _ if self.saw_tool_call => StopReason::ToolUse,
                    _ => StopReason::EndTurn,
                };
                out.push(StreamEvent::Done(reason));
                self.done = true;
            }
            "response.failed" => {
                let error = &event["response"]["error"];
                return Err(ProviderError::Api {
                    code: error["code"].as_str().map(str::to_owned),
                    message: error["message"]
                        .as_str()
                        .unwrap_or("the response failed")
                        .to_owned(),
                });
            }
            "error" => {
                return Err(ProviderError::Api {
                    code: event["code"].as_str().map(str::to_owned),
                    message: event["message"]
                        .as_str()
                        .unwrap_or("unknown stream error")
                        .to_owned(),
                });
            }
            _ => {}
        }
        Ok(out)
    }

    /// Ends a function call once; if no argument deltas arrived, sends the full arguments first.
    fn end_call(&mut self, item_id: &str, arguments: &str, out: &mut Vec<StreamEvent>) {
        let Some(call) = self.calls.get_mut(item_id) else {
            return;
        };
        if call.ended {
            return;
        }
        if !call.saw_delta && !arguments.is_empty() {
            out.push(StreamEvent::ToolCallArgsDelta {
                id: call.call_id.clone(),
                json: arguments.to_owned(),
            });
        }
        call.ended = true;
        out.push(StreamEvent::ToolCallEnd {
            id: call.call_id.clone(),
        });
    }
}

/// Known OpenAI models with list prices (USD per 1M tokens) as of 2026-10.
pub fn models() -> Vec<ModelInfo> {
    [
        ("gpt-5.1", 400_000, 1.25, 10.0),
        ("gpt-5", 400_000, 1.25, 10.0),
        ("gpt-5-mini", 400_000, 0.25, 2.0),
        ("gpt-5-nano", 400_000, 0.05, 0.40),
        ("gpt-4.1", 1_047_576, 2.0, 8.0),
    ]
    .into_iter()
    .map(|(id, context_window, price_in, price_out)| ModelInfo {
        id: id.to_owned(),
        provider: PROVIDER_ID.to_owned(),
        context_window,
        supports_tools: true,
        price_in_per_mtok: price_in,
        price_out_per_mtok: price_out,
    })
    .collect()
}

/// Looks up `id` in [`models`].
pub fn model_info(id: &str) -> Option<ModelInfo> {
    models().into_iter().find(|model| model.id == id)
}

#[cfg(test)]
mod tests {
    use rozu_core::{Message, ToolSpec};

    use super::*;

    #[test]
    fn builds_request_body() {
        let req = ChatRequest {
            model: "gpt-5.1".into(),
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
                            id: "call_1".into(),
                            name: "list_dir".into(),
                            args_json: String::new(),
                        },
                    ],
                ),
                Message::tool_result("call_1", "README.md", false),
            ],
            tools: vec![ToolSpec {
                name: "list_dir".into(),
                description: "List".into(),
                json_schema: json!({"type": "object"}),
            }],
            max_output_tokens: Some(1000),
        };
        assert_eq!(
            request_body(&req),
            json!({
                "model": "gpt-5.1",
                "stream": true,
                "store": false,
                "max_output_tokens": 1000,
                "input": [
                    {"role": "system", "content": "be brief"},
                    {"role": "user", "content": "hi"},
                    {"role": "assistant", "content": "let me look"},
                    {"type": "function_call", "call_id": "call_1", "name": "list_dir", "arguments": "{}"},
                    {"type": "function_call_output", "call_id": "call_1", "output": "README.md"},
                ],
                "tools": [{
                    "type": "function",
                    "name": "list_dir",
                    "description": "List",
                    "parameters": {"type": "object"},
                    "strict": false,
                }],
            })
        );
    }

    fn feed(state: &mut SseState, events: &[Value]) -> Vec<StreamEvent> {
        events
            .iter()
            .flat_map(|e| state.handle(&e.to_string()).unwrap())
            .collect()
    }

    #[test]
    fn maps_function_call_events() {
        let mut state = SseState::default();
        let out = feed(
            &mut state,
            &[
                json!({"type": "response.output_item.added", "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "grep", "arguments": ""}}),
                json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{\"pattern\":"}),
                json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "\"x\"}"}),
                json!({"type": "response.function_call_arguments.done", "item_id": "fc_1", "arguments": "{\"pattern\":\"x\"}"}),
                json!({"type": "response.output_item.done", "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "grep", "arguments": "{\"pattern\":\"x\"}"}}),
                json!({"type": "response.completed", "response": {"usage": {"input_tokens": 5, "output_tokens": 7, "input_tokens_details": {"cached_tokens": 2}}}}),
            ],
        );
        assert_eq!(
            out,
            [
                StreamEvent::ToolCallStart {
                    id: "call_1".into(),
                    name: "grep".into()
                },
                StreamEvent::ToolCallArgsDelta {
                    id: "call_1".into(),
                    json: "{\"pattern\":".into()
                },
                StreamEvent::ToolCallArgsDelta {
                    id: "call_1".into(),
                    json: "\"x\"}".into()
                },
                StreamEvent::ToolCallEnd {
                    id: "call_1".into()
                },
                StreamEvent::Usage(Usage {
                    input_tokens: 5,
                    output_tokens: 7,
                    cached_input_tokens: 2
                }),
                StreamEvent::Done(StopReason::ToolUse),
            ]
        );
        assert!(state.done);
    }

    #[test]
    fn arguments_without_deltas_come_from_done() {
        let mut state = SseState::default();
        let out = feed(
            &mut state,
            &[
                json!({"type": "response.output_item.added", "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "grep"}}),
                json!({"type": "response.output_item.done", "item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "arguments": "{}"}}),
            ],
        );
        assert_eq!(
            out[1..],
            [
                StreamEvent::ToolCallArgsDelta {
                    id: "call_1".into(),
                    json: "{}".into()
                },
                StreamEvent::ToolCallEnd {
                    id: "call_1".into()
                },
            ]
        );
    }

    #[test]
    fn maps_incomplete_and_errors() {
        let mut state = SseState::default();
        let out = feed(
            &mut state,
            &[
                json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}}}),
            ],
        );
        assert_eq!(out, [StreamEvent::Done(StopReason::MaxTokens)]);

        let err = SseState::default()
            .handle(&json!({"type": "response.failed", "response": {"error": {"code": "server_error", "message": "boom"}}}).to_string())
            .unwrap_err();
        assert_eq!(
            err,
            ProviderError::Api {
                code: Some("server_error".into()),
                message: "boom".into()
            }
        );
        assert!(matches!(
            SseState::default().handle("not json"),
            Err(ProviderError::Decode(_))
        ));
    }

    #[test]
    fn knows_default_model() {
        let model = model_info("gpt-5.1").unwrap();
        assert_eq!(model.provider, "openai");
        assert!(model.supports_tools);
        assert!(model_info("nope").is_none());
    }

    #[test]
    fn debug_does_not_leak_key() {
        let config = OpenAiConfig::new(SecretString::from("sk-test-supersecretvalue123"));
        let provider = OpenAiProvider::new(config).unwrap();
        let debug = format!("{provider:?}");
        assert!(!debug.contains("supersecret"), "{debug}");
    }
}
