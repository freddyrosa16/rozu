//! The provider-neutral message, tool and model description types.
//!
//! Every provider adapter maps to and from these, so the agent loop and the UI never see a
//! provider's wire format.

use std::ops::AddAssign;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    /// Carries [`ContentPart::ToolResult`]s back to the model.
    Tool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    /// A tool call the model made. `args_json` is the raw JSON arguments string as streamed.
    ToolCall {
        id: String,
        name: String,
        args_json: String,
    },
    ToolResult {
        call_id: String,
        output: String,
        is_error: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
}

impl Message {
    pub fn new(role: Role, content: Vec<ContentPart>) -> Self {
        Self { role, content }
    }

    pub fn system(text: impl Into<String>) -> Self {
        Self::text(Role::System, text)
    }

    pub fn user(text: impl Into<String>) -> Self {
        Self::text(Role::User, text)
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Self::text(Role::Assistant, text)
    }

    pub fn tool_result(
        call_id: impl Into<String>,
        output: impl Into<String>,
        is_error: bool,
    ) -> Self {
        Self::new(
            Role::Tool,
            vec![ContentPart::ToolResult {
                call_id: call_id.into(),
                output: output.into(),
                is_error,
            }],
        )
    }

    fn text(role: Role, text: impl Into<String>) -> Self {
        Self::new(role, vec![ContentPart::Text { text: text.into() }])
    }

    /// All text parts concatenated.
    pub fn text_content(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect()
    }

    /// The tool calls in this message as `(id, name, args_json)`.
    pub fn tool_calls(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        self.content.iter().filter_map(|part| match part {
            ContentPart::ToolCall {
                id,
                name,
                args_json,
            } => Some((id.as_str(), name.as_str(), args_json.as_str())),
            _ => None,
        })
    }
}

/// A tool as advertised to the model.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema of the arguments object.
    pub json_schema: Value,
}

/// One model request: the whole conversation so far plus the tools on offer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_output_tokens: Option<u32>,
}

/// Token counts for one response (or, summed, for a session).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Part of `input_tokens` served from the provider's prompt cache.
    pub cached_input_tokens: u64,
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cached_input_tokens += other.cached_input_tokens;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StopReason {
    /// The model finished its answer.
    EndTurn,
    /// The model wants tool results before continuing.
    ToolUse,
    /// The output token limit was hit.
    MaxTokens,
    /// The provider filtered the output.
    ContentFilter,
}

/// One normalized event from a provider's response stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    TextDelta(String),
    ToolCallStart {
        id: String,
        name: String,
    },
    ToolCallArgsDelta {
        id: String,
        json: String,
    },
    ToolCallEnd {
        id: String,
    },
    Usage(Usage),
    /// Always the last event of a successful stream.
    Done(StopReason),
}

/// What the UI needs to know about a model: limits, capabilities and list prices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    pub provider: String,
    /// Maximum input + output tokens.
    pub context_window: u32,
    pub supports_tools: bool,
    /// USD per million input tokens.
    pub price_in_per_mtok: f64,
    /// USD per million output tokens.
    pub price_out_per_mtok: f64,
}

impl ModelInfo {
    /// Estimated USD cost of `usage` at list prices (prompt-cache discounts are ignored).
    pub fn cost_usd(&self, usage: &Usage) -> f64 {
        (usage.input_tokens as f64 * self.price_in_per_mtok
            + usage.output_tokens as f64 * self.price_out_per_mtok)
            / 1_000_000.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_helpers() {
        let msg = Message::new(
            Role::Assistant,
            vec![
                ContentPart::Text { text: "a".into() },
                ContentPart::ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    args_json: "{}".into(),
                },
                ContentPart::Text { text: "b".into() },
            ],
        );
        assert_eq!(msg.text_content(), "ab");
        assert_eq!(
            msg.tool_calls().collect::<Vec<_>>(),
            [("c1", "read_file", "{}")]
        );
    }

    #[test]
    fn content_parts_serialize_tagged() {
        let json = serde_json::to_value(Message::tool_result("c1", "ok", false)).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "role": "tool",
                "content": [{"type": "tool_result", "call_id": "c1", "output": "ok", "is_error": false}]
            })
        );
    }

    #[test]
    fn usage_adds_and_costs() {
        let mut total = Usage::default();
        total += Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cached_input_tokens: 0,
        };
        total += Usage {
            input_tokens: 0,
            output_tokens: 100_000,
            cached_input_tokens: 0,
        };
        let model = ModelInfo {
            id: "m".into(),
            provider: "p".into(),
            context_window: 1000,
            supports_tools: true,
            price_in_per_mtok: 1.0,
            price_out_per_mtok: 10.0,
        };
        assert!((model.cost_usd(&total) - 3.0).abs() < 1e-9);
    }
}
