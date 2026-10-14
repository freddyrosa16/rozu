//! A provider that plays back assistant messages as model responses: dry-run evals and
//! recorded-session replays.

use std::collections::VecDeque;
use std::sync::Mutex;

use async_trait::async_trait;
use futures::StreamExt;
use futures::stream;
use tokio_util::sync::CancellationToken;

use crate::compact::estimate_tokens;
use crate::model::{ChatRequest, ContentPart, Message, Role, StopReason, StreamEvent, Usage};
use crate::provider::{EventStream, Provider, ProviderError, ProviderResult};

pub struct ReplayProvider {
    steps: Mutex<VecDeque<Message>>,
    requests: Mutex<Vec<ChatRequest>>,
}

impl ReplayProvider {
    pub fn new(steps: impl IntoIterator<Item = Message>) -> Self {
        Self {
            steps: Mutex::new(steps.into_iter().collect()),
            requests: Mutex::default(),
        }
    }

    /// Replays a recorded conversation's assistant messages.
    pub fn from_session(messages: &[Message]) -> Self {
        Self::new(
            messages
                .iter()
                .filter(|m| m.role == Role::Assistant)
                .cloned(),
        )
    }

    pub fn remaining(&self) -> usize {
        self.steps.lock().expect("replay lock").len()
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().expect("replay lock").clone()
    }
}

#[async_trait]
impl Provider for ReplayProvider {
    fn id(&self) -> &str {
        "replay"
    }

    async fn stream(&self, req: ChatRequest, _: CancellationToken) -> ProviderResult<EventStream> {
        let step = self.steps.lock().expect("replay lock").pop_front();
        let Some(step) = step else {
            return Err(ProviderError::Api {
                code: Some("replay_exhausted".into()),
                message: "the recording has no more model responses".into(),
            });
        };
        let usage = {
            let mut requests = self.requests.lock().expect("replay lock");
            let usage = simulated_usage(requests.last(), &req, &step);
            requests.push(req);
            usage
        };
        let mut events = Vec::new();
        let mut calls = false;
        for part in step.content {
            match part {
                ContentPart::Text { text } => events.push(StreamEvent::TextDelta(text)),
                ContentPart::ToolCall {
                    id,
                    name,
                    args_json,
                } => {
                    calls = true;
                    events.push(StreamEvent::ToolCallStart {
                        id: id.clone(),
                        name,
                    });
                    events.push(StreamEvent::ToolCallArgsDelta {
                        id: id.clone(),
                        json: args_json,
                    });
                    events.push(StreamEvent::ToolCallEnd { id });
                }
                ContentPart::ToolResult { .. } => {}
            }
        }
        events.push(StreamEvent::Usage(usage));
        events.push(StreamEvent::Done(if calls {
            StopReason::ToolUse
        } else {
            StopReason::EndTurn
        }));
        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

/// Estimated usage of answering `req` with `answer`, counting the messages it shares with
/// `previous` from the start as cache reads (what a provider's prefix cache would serve).
pub fn simulated_usage(
    previous: Option<&ChatRequest>,
    req: &ChatRequest,
    answer: &Message,
) -> Usage {
    let shared = previous.map_or(0, |prev| {
        prev.messages
            .iter()
            .zip(&req.messages)
            .take_while(|(a, b)| a == b)
            .count()
    });
    Usage {
        input_tokens: estimate_tokens(&req.messages),
        output_tokens: estimate_tokens(std::slice::from_ref(answer)).max(1),
        cache_read_tokens: estimate_tokens(&req.messages[..shared]),
        cache_write_tokens: 0,
    }
}
