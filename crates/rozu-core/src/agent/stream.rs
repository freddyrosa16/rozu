//! Provider streams normalized into model steps or vendor-agent turn events.

use futures::StreamExt;
use rozu_tools::truncate_head_tail;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use super::{Agent, AgentError, AgentEvent, Session, TurnOutcome, finish, keep_partial_text};
use crate::{ChatRequest, EventStream, PendingApproval, ProviderError, StreamEvent};

/// A tool call being assembled from stream events.
#[derive(Debug, Default)]
pub(super) struct PendingCall {
    pub(super) id: String,
    pub(super) name: String,
    pub(super) args_json: String,
}

/// Everything one model response produced.
#[derive(Debug, Default)]
pub(super) struct StepOutput {
    pub(super) text: String,
    pub(super) calls: Vec<PendingCall>,
}

pub(super) enum StreamEnd {
    Done,
    Cancelled,
    Failed(ProviderError),
}

impl Agent {
    async fn open_stream(
        &self,
        request: ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<EventStream, StreamEnd> {
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(StreamEnd::Cancelled),
            stream = self.provider.stream(request, cancel.clone()) => stream.map_err(StreamEnd::from),
        }
    }

    /// Streams one model response into `output`, forwarding text and usage as it arrives.
    pub(super) async fn stream_step(
        &self,
        request: ChatRequest,
        session: &mut Session,
        events: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
        output: &mut StepOutput,
    ) -> StreamEnd {
        let mut stream = match self.open_stream(request, cancel).await {
            Ok(stream) => stream,
            Err(end) => return end,
        };

        loop {
            let event = next_event(&mut stream, cancel).await;
            match event {
                Err(end) => return end,
                Ok(event) => match event {
                    StreamEvent::TextDelta(delta) => {
                        output.text.push_str(&delta);
                        let _ = events.send(AgentEvent::TextDelta(delta)).await;
                    }
                    StreamEvent::ToolCallStart { id, name } => {
                        output.calls.push(PendingCall {
                            id,
                            name,
                            args_json: String::new(),
                        });
                    }
                    StreamEvent::ToolCallArgsDelta { id, json } => {
                        if let Some(call) = output.calls.iter_mut().rev().find(|c| c.id == id) {
                            call.args_json.push_str(&json);
                        } else {
                            warn!(%id, "arguments for an unknown tool call");
                        }
                    }
                    StreamEvent::ToolCallEnd { .. } => {}
                    StreamEvent::Usage(usage) => {
                        session.record_usage(usage, self.config.priced_model());
                        let _ = events.send(AgentEvent::Usage(usage)).await;
                    }
                    StreamEvent::ToolResult { id, .. } => {
                        warn!(%id, "a model provider sent a tool result; ignored");
                    }
                    StreamEvent::Approval(record) => {
                        warn!(call_id = %record.call_id, "a model provider sent an approval; ignored");
                    }
                    StreamEvent::ApprovalPending(PendingApproval { request_id, .. })
                    | StreamEvent::ApprovalResolved { request_id, .. } => {
                        warn!(%request_id, "a model provider sent an approval prompt; ignored");
                    }
                    StreamEvent::Done(reason) => {
                        debug!(?reason, "model response done");
                        return StreamEnd::Done;
                    }
                },
            }
        }
    }

    /// One turn with an agent backend ([`crate::ProviderKind::AgentBackend`]): a single stream for the
    /// whole turn, whose tool calls, results and approvals are forwarded as [`AgentEvent`]s. The
    /// backend runs the tools (rozu's own, through MCP), so nothing executes here.
    ///
    /// Only the assistant's text is kept in the session; the backend keeps its own record of
    /// the tool calls.
    pub(super) async fn backend_turn(
        &self,
        session: &mut Session,
        events: &mpsc::Sender<AgentEvent>,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, AgentError> {
        let request = ChatRequest {
            model: self.config.model.clone(),
            messages: session.messages.clone(),
            tools: Vec::new(),
            max_output_tokens: self.config.max_output_tokens,
        };
        let mut output = StepOutput::default();
        let end = self
            .stream_backend(request, session, events, &cancel, &mut output)
            .await;
        keep_partial_text(session, output.text);
        match end {
            StreamEnd::Done => Ok(finish(events, TurnOutcome::Completed).await),
            StreamEnd::Cancelled => Ok(finish(events, TurnOutcome::Cancelled).await),
            StreamEnd::Failed(err) => {
                warn!(error = %err, "agent backend turn failed");
                Err(err.into())
            }
        }
    }

    async fn stream_backend(
        &self,
        request: ChatRequest,
        session: &mut Session,
        events: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
        output: &mut StepOutput,
    ) -> StreamEnd {
        let mut stream = match self.open_stream(request, cancel).await {
            Ok(stream) => stream,
            Err(end) => return end,
        };
        loop {
            let event = next_event(&mut stream, cancel).await;
            let event = match event {
                Err(end) => return end,
                Ok(event) => event,
            };
            let forward = match event {
                StreamEvent::TextDelta(delta) => {
                    output.text.push_str(&delta);
                    AgentEvent::TextDelta(delta)
                }
                StreamEvent::ToolCallStart { id, name } => {
                    output.calls.push(PendingCall {
                        id,
                        name,
                        args_json: String::new(),
                    });
                    continue;
                }
                StreamEvent::ToolCallArgsDelta { id, json } => {
                    if let Some(call) = output.calls.iter_mut().rev().find(|c| c.id == id) {
                        call.args_json.push_str(&json);
                    }
                    continue;
                }
                StreamEvent::ToolCallEnd { id } => {
                    let Some(call) = output.calls.iter().rev().find(|c| c.id == id) else {
                        warn!(%id, "end of an unknown tool call");
                        continue;
                    };
                    AgentEvent::ToolCallStarted {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        args_json: call.args_json.clone(),
                    }
                }
                StreamEvent::ToolResult {
                    id,
                    output: text,
                    is_error,
                } => {
                    let name = output
                        .calls
                        .iter()
                        .rev()
                        .find(|c| c.id == id)
                        .map(|c| c.name.clone())
                        .unwrap_or_default();
                    AgentEvent::ToolCallFinished {
                        id,
                        name,
                        output: truncate_head_tail(&text, self.config.max_tool_output_bytes)
                            .into_owned(),
                        is_error,
                    }
                }
                StreamEvent::Approval(record) => AgentEvent::Approval(record),
                StreamEvent::ApprovalPending(pending) => AgentEvent::ApprovalPending(pending),
                StreamEvent::ApprovalResolved {
                    request_id,
                    resolution,
                } => AgentEvent::ApprovalResolved {
                    request_id,
                    resolution,
                },
                StreamEvent::Usage(usage) => {
                    session.record_usage(usage, self.config.priced_model());
                    AgentEvent::Usage(usage)
                }
                StreamEvent::Done(reason) => {
                    debug!(?reason, "agent backend turn done");
                    return StreamEnd::Done;
                }
            };
            let _ = events.send(forward).await;
        }
    }
}

impl From<ProviderError> for StreamEnd {
    fn from(error: ProviderError) -> Self {
        match error {
            ProviderError::Cancelled => Self::Cancelled,
            error => Self::Failed(error),
        }
    }
}

async fn next_event(
    stream: &mut EventStream,
    cancel: &CancellationToken,
) -> Result<StreamEvent, StreamEnd> {
    let event = tokio::select! {
        biased;
        _ = cancel.cancelled() => return Err(StreamEnd::Cancelled),
        event = stream.next() => event,
    };
    match event {
        Some(event) => event.map_err(StreamEnd::from),
        None if cancel.is_cancelled() => Err(StreamEnd::Cancelled),
        None => Err(StreamEnd::Failed(ProviderError::UnexpectedEof)),
    }
}
