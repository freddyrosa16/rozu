//! Conversation state and usage accounting shared by agent turns.

use crate::{Compaction, Message, ModelInfo, Usage};

/// Tokens and estimated cost of one user turn (all of its model requests).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TurnUsage {
    pub usage: Usage,
    /// `None` when the model has no known prices or is covered by a subscription.
    pub cost_usd: Option<f64>,
}

/// The conversation so far plus running totals. A session can outlive an [`crate::Agent`], so the
/// model (and provider) can change between turns.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Session {
    pub messages: Vec<Message>,
    /// Tokens over all turns.
    pub usage: Usage,
    /// Estimated USD over all turns whose model had known prices.
    pub cost_usd: f64,
    /// The most recent turn, including one that was cancelled or failed.
    pub last_turn: TurnUsage,
    /// Bumped whenever earlier messages are rewritten (compaction), so a store knows to write
    /// the whole history again instead of appending.
    pub compactions: u32,
    /// Prompt plus output tokens of the latest model response: roughly what the next request
    /// starts from. 0 until a provider reports usage.
    pub context_tokens: u64,
    /// What compaction did during the most recent turn, if anything.
    pub last_compaction: Option<Compaction>,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_system_prompt(prompt: impl Into<String>) -> Self {
        Self {
            messages: vec![Message::system(prompt)],
            ..Self::default()
        }
    }

    /// Adds one response's usage to the totals, pricing it with `model` (`None`: unpriced).
    pub(super) fn record_usage(&mut self, usage: Usage, model: Option<&ModelInfo>) {
        self.usage += usage;
        self.last_turn.usage += usage;
        self.context_tokens = usage.input_tokens + usage.output_tokens;
        if let Some(model) = model {
            let cost = model.cost_usd(&usage);
            self.cost_usd += cost;
            *self.last_turn.cost_usd.get_or_insert(0.0) += cost;
        }
    }
}
