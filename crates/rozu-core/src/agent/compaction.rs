//! Masking and summarization of model-managed conversation context.

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{Agent, AgentError, Session, StepOutput, StreamEnd};
use crate::compact::{
    Compaction, SUMMARY_HEADER, SUMMARY_PROMPT, estimate_tokens, mask_tool_outputs, pinned_len,
    recent_start, transcript,
};
use crate::{ChatRequest, Message, ProviderKind};

impl Agent {
    /// Whether the provider is a vendor CLI that keeps (and compacts) its own context.
    pub fn manages_own_context(&self) -> bool {
        self.provider.kind() == ProviderKind::AgentBackend
    }

    /// Masks and summarizes everything but the pinned prefix and the recent turns now, whatever
    /// the context size (`/compact`). Does nothing for an agent backend.
    pub async fn compact(&self, session: &mut Session, cancel: CancellationToken) -> Compaction {
        self.compact_with(session, &cancel, true)
            .await
            .unwrap_or_default()
    }

    /// Applies [`crate::MemoryPolicy`]: masking, then summarizing, each when its threshold is passed
    /// (`force`: regardless). `None` when nothing changed.
    pub(super) async fn compact_with(
        &self,
        session: &mut Session,
        cancel: &CancellationToken,
        force: bool,
    ) -> Option<Compaction> {
        if self.manages_own_context() {
            return None;
        }
        let policy = self.config.memory;
        let window = self.config.model_info.as_ref().map(|i| i.context_window);
        let used = |s: &Session| match s.context_tokens {
            0 => estimate_tokens(&s.messages),
            n => n,
        };
        let over = |share: Option<f32>, tokens: u64| {
            force
                || matches!((share, window), (Some(share), Some(window))
                    if tokens as f64 >= f64::from(window) * f64::from(share))
        };
        let mut done = Compaction {
            tokens_before: used(session),
            ..Compaction::default()
        };
        let end = recent_start(&session.messages, policy.keep_turns);
        if over(policy.mask_at, done.tokens_before) {
            done.masked = mask_tool_outputs(&mut session.messages, end);
            if done.masked > 0 {
                session.context_tokens = estimate_tokens(&session.messages);
            }
        }
        if over(policy.summarize_at, used(session)) {
            match self.summarize(session, cancel).await {
                Ok(n) => done.summarized = n,
                Err(err) => warn!(error = %err, "could not summarize older turns"),
            }
        }
        if done.masked + done.summarized == 0 {
            return None;
        }
        session.compactions += 1;
        session.context_tokens = estimate_tokens(&session.messages);
        done.tokens_after = session.context_tokens;
        debug!(?done, "compacted the conversation");
        Some(done)
    }

    /// Replaces the messages between the pinned prefix and the recent turns with one summary.
    /// Returns how many messages it replaced.
    async fn summarize(
        &self,
        session: &mut Session,
        cancel: &CancellationToken,
    ) -> Result<usize, AgentError> {
        let start = pinned_len(&session.messages);
        let end = recent_start(&session.messages, self.config.memory.keep_turns).max(start);
        if end - start < 2 {
            return Ok(0);
        }
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: vec![
                Message::system(SUMMARY_PROMPT),
                Message::user(transcript(&session.messages[start..end])),
            ],
            tools: Vec::new(),
            max_output_tokens: self.config.max_output_tokens,
        };
        // The summary isn't shown to the user.
        let (quiet, closed) = mpsc::channel(1);
        drop(closed);
        let mut output = StepOutput::default();
        match self
            .stream_step(request, session, &quiet, cancel, &mut output)
            .await
        {
            StreamEnd::Done if !output.text.trim().is_empty() => {}
            StreamEnd::Failed(err) => return Err(err.into()),
            _ => return Ok(0),
        }
        let summary = Message::system(format!("{SUMMARY_HEADER}\n{}", output.text.trim()));
        session.messages.splice(start..end, [summary]);
        Ok(end - start)
    }
}
