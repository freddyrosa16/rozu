//! The provider-neutral message, tool and model description types.
//!
//! Every provider adapter maps to and from these, so the agent loop and the UI never see a
//! provider's wire format.

use std::ops::AddAssign;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::approval::{ApprovalRecord, ApprovalResolution, PendingApproval};

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
    /// Every prompt token the request was billed for, including the cache reads and cache writes
    /// below. Uncached input is `input_tokens - cache_read_tokens - cache_write_tokens`.
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// Part of `input_tokens` served from the provider's prompt cache.
    pub cache_read_tokens: u64,
    /// Part of `input_tokens` written to the prompt cache (Anthropic bills these at a premium;
    /// providers that cache implicitly report 0).
    pub cache_write_tokens: u64,
}

impl AddAssign for Usage {
    fn add_assign(&mut self, other: Self) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
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
    /// The result of a tool call the provider ran itself. Only agent-backend providers (see
    /// [`crate::ProviderKind::AgentBackend`]) send this; model providers leave tools to rozu.
    ToolResult {
        id: String,
        output: String,
        is_error: bool,
    },
    /// An approval decision rozu made for a tool an agent backend asked to run.
    Approval(ApprovalRecord),
    /// An approval prompt for an agent backend's request is open and waiting for the user
    /// (forwarded as [`crate::AgentEvent::ApprovalPending`]).
    ApprovalPending(PendingApproval),
    /// That prompt closed (forwarded as [`crate::AgentEvent::ApprovalResolved`]).
    ApprovalResolved {
        request_id: String,
        resolution: ApprovalResolution,
    },
    /// Always the last event of a successful stream.
    Done(StopReason),
}

/// How a model's usage is paid for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Billing {
    /// Per token at the listed prices (API key).
    #[default]
    Metered,
    /// Covered by the user's subscription (a CLI-backed provider): tokens are counted, but there
    /// is no per-request cost, so cost is reported as `None`.
    Subscription,
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
    /// USD per million input tokens served from the prompt cache.
    pub price_cached_in_per_mtok: f64,
    /// USD per million input tokens written to the prompt cache. Equal to the input price for
    /// providers without a write premium.
    pub price_cache_write_per_mtok: f64,
    /// Higher prices for requests with a long prompt, if the model has them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub long_context: Option<LongContextPrices>,
    #[serde(default)]
    pub billing: Billing,
    /// What rozu's memory layer can do with this model's provider.
    #[serde(default)]
    pub caps: ProviderCaps,
}

/// How much of rozu's memory policy (masking, compaction, retrieval) a provider supports.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryFeatures {
    /// Every memory/eval condition can run.
    #[default]
    Full,
    /// Only conditions that don't need per-turn control of the prompt prefix (rozu can still
    /// rewrite history, but only by starting a new provider session, which loses its cache).
    Limited,
    /// None.
    Unavailable,
}

/// What a provider lets rozu control and measure, for the UI and the memory eval.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderCaps {
    /// rozu decides the exact prompt bytes of every request (system prompt, tool schemas,
    /// history), so per-turn masking and cache breakpoints are possible.
    pub prompt_prefix_control: bool,
    /// The provider reports cache-write tokens (otherwise `cache_write_tokens` is always 0 and
    /// eval rows should record it as unknown).
    pub cache_write_reported: bool,
    pub memory_features: MemoryFeatures,
}

impl Default for ProviderCaps {
    fn default() -> Self {
        Self::FULL
    }
}

impl ProviderCaps {
    /// rozu builds every request itself (the HTTP adapters).
    pub const FULL: Self = Self {
        prompt_prefix_control: true,
        cache_write_reported: true,
        memory_features: MemoryFeatures::Full,
    };
}

/// A long-context price tier: a request whose prompt (all input tokens, including cache reads and
/// writes) has at least `threshold_tokens` tokens pays these prices for **all** of its tokens,
/// output included. Each request is priced on its own.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LongContextPrices {
    /// Smallest prompt size, in tokens, billed at the long-context prices.
    pub threshold_tokens: u64,
    pub price_in_per_mtok: f64,
    pub price_out_per_mtok: f64,
    pub price_cached_in_per_mtok: f64,
    pub price_cache_write_per_mtok: f64,
}

impl ModelInfo {
    pub fn with_long_context(mut self, prices: LongContextPrices) -> Self {
        self.long_context = Some(prices);
        self
    }

    /// A model covered by a subscription: no prices, and cost is always `None`.
    pub fn subscription(
        provider: impl Into<String>,
        id: impl Into<String>,
        context_window: u32,
    ) -> Self {
        Self {
            id: id.into(),
            provider: provider.into(),
            context_window,
            supports_tools: true,
            price_in_per_mtok: 0.0,
            price_out_per_mtok: 0.0,
            price_cached_in_per_mtok: 0.0,
            price_cache_write_per_mtok: 0.0,
            long_context: None,
            billing: Billing::Subscription,
            caps: ProviderCaps::default(),
        }
    }

    pub fn with_caps(mut self, caps: ProviderCaps) -> Self {
        self.caps = caps;
        self
    }

    /// Whether this model's usage has a USD cost, i.e. [`ModelInfo::cost_usd`] means something.
    pub fn is_metered(&self) -> bool {
        self.billing == Billing::Metered
    }

    /// Estimated USD cost of **one response's** `usage` at list prices: cache reads at the
    /// cache-read price, cache writes at the cache-write price, and the rest of the input at the
    /// normal price, all at the long-context tier when the prompt reaches its threshold. Price
    /// each response separately; summed usage would cross the threshold too early.
    pub fn cost_usd(&self, usage: &Usage) -> f64 {
        let (p_in, p_out, p_read, p_write) = match self.long_context {
            Some(long) if usage.input_tokens >= long.threshold_tokens => (
                long.price_in_per_mtok,
                long.price_out_per_mtok,
                long.price_cached_in_per_mtok,
                long.price_cache_write_per_mtok,
            ),
            _ => (
                self.price_in_per_mtok,
                self.price_out_per_mtok,
                self.price_cached_in_per_mtok,
                self.price_cache_write_per_mtok,
            ),
        };
        let read = usage.cache_read_tokens.min(usage.input_tokens);
        let write = usage.cache_write_tokens.min(usage.input_tokens - read);
        let uncached = usage.input_tokens - read - write;
        (uncached as f64 * p_in
            + read as f64 * p_read
            + write as f64 * p_write
            + usage.output_tokens as f64 * p_out)
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

    fn flat_model() -> ModelInfo {
        ModelInfo {
            id: "m".into(),
            provider: "p".into(),
            context_window: 1_000_000,
            supports_tools: true,
            price_in_per_mtok: 1.0,
            price_out_per_mtok: 10.0,
            price_cached_in_per_mtok: 0.1,
            price_cache_write_per_mtok: 1.25,
            long_context: None,
            billing: Billing::Metered,
            caps: ProviderCaps::default(),
        }
    }

    fn tiered_model() -> ModelInfo {
        flat_model().with_long_context(LongContextPrices {
            threshold_tokens: 200_000,
            price_in_per_mtok: 2.0,
            price_out_per_mtok: 20.0,
            price_cached_in_per_mtok: 0.2,
            price_cache_write_per_mtok: 2.5,
        })
    }

    fn usage(input: u64, read: u64, write: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: read,
            cache_write_tokens: write,
        }
    }

    #[track_caller]
    fn assert_close(actual: f64, expected: f64) {
        assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
    }

    #[test]
    fn usage_adds_and_costs() {
        let mut total = Usage::default();
        total += usage(1_000_000, 0, 0, 100_000);
        total += usage(0, 0, 0, 100_000);
        let model = flat_model();
        assert_close(model.cost_usd(&total), 3.0);

        // Half the input from the cache: 0.5 * 1.0 + 0.5 * 0.1 + 0.2 * 10.0.
        total.cache_read_tokens = 500_000;
        assert_close(model.cost_usd(&total), 2.55);

        // Plus a fifth written to the cache: 0.3 * 1.0 + 0.5 * 0.1 + 0.2 * 1.25 + 0.2 * 10.0.
        total.cache_write_tokens = 200_000;
        assert_close(model.cost_usd(&total), 2.6);

        // Over-reported cache counts never price more input than was billed.
        total.cache_write_tokens = 900_000;
        assert_close(model.cost_usd(&total), 0.5 * 0.1 + 0.5 * 1.25 + 2.0);
    }

    #[test]
    fn long_context_tier_starts_at_the_threshold() {
        let model = tiered_model();
        assert_close(
            model.cost_usd(&usage(199_999, 0, 0, 1_000)),
            0.199_999 + 0.01,
        );
        // At the threshold every token, output included, pays the long-context price.
        assert_close(model.cost_usd(&usage(200_000, 0, 0, 1_000)), 0.4 + 0.02);
    }

    #[test]
    fn long_context_threshold_counts_cached_input() {
        let model = tiered_model();
        // 150k cache reads + 40k writes + 10k uncached = 200k prompt tokens: long tier.
        assert_close(
            model.cost_usd(&usage(200_000, 150_000, 40_000, 0)),
            (10_000.0 * 2.0 + 150_000.0 * 0.2 + 40_000.0 * 2.5) / 1e6,
        );
    }

    #[test]
    fn model_info_without_long_context_round_trips() {
        let model = flat_model();
        let json = serde_json::to_value(&model).unwrap();
        assert!(json.get("long_context").is_none());
        assert_eq!(serde_json::from_value::<ModelInfo>(json).unwrap(), model);
        let tiered = tiered_model();
        let json = serde_json::to_string(&tiered).unwrap();
        assert_eq!(serde_json::from_str::<ModelInfo>(&json).unwrap(), tiered);
    }

    #[test]
    fn billing_defaults_to_metered() {
        let mut json = serde_json::to_value(tiered_model()).unwrap();
        json.as_object_mut().unwrap().remove("billing");
        let model: ModelInfo = serde_json::from_value(json).unwrap();
        assert!(model.is_metered());
        let sub = ModelInfo::subscription("claude-cli", "sonnet", 200_000);
        assert!(!sub.is_metered());
        assert_eq!(
            serde_json::to_value(&sub).unwrap()["billing"],
            "subscription"
        );
    }
}
