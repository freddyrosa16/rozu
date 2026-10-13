//! The agent loop: stream a response, run the tool calls it asks for, feed the results back, and
//! repeat until the model answers, the step cap is hit, or the user cancels.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rozu_tools::{
    MAX_OUTPUT_BYTES, Risk, ToolCtx, ToolOutput, ToolRegistry, Workspace, truncate_head_tail,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info_span, warn};

use crate::approval::{ApprovalGate, ApprovalRequest, Decision, ReadOnlyGate};
use crate::model::{ChatRequest, ContentPart, Message, Role, StreamEvent, ToolSpec, Usage};
use crate::provider::{Provider, ProviderError};

/// Default cap on model requests per user turn.
pub const DEFAULT_MAX_STEPS: usize = 20;
/// Default limit for a single tool call.
pub const DEFAULT_TOOL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub model: String,
    /// Model requests per user turn before the loop stops with [`TurnOutcome::MaxSteps`].
    pub max_steps: usize,
    pub tool_timeout: Duration,
    pub max_output_tokens: Option<u32>,
    /// Tool results longer than this are cut to head + tail before going back to the model.
    pub max_tool_output_bytes: usize,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            max_steps: DEFAULT_MAX_STEPS,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            max_output_tokens: None,
            max_tool_output_bytes: MAX_OUTPUT_BYTES,
        }
    }
}

/// The conversation so far plus running totals.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Session {
    pub messages: Vec<Message>,
    pub usage: Usage,
}

impl Session {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_system_prompt(prompt: impl Into<String>) -> Self {
        Self {
            messages: vec![Message::system(prompt)],
            usage: Usage::default(),
        }
    }
}

/// Progress reported to the UI / CLI while a turn runs.
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    TextDelta(String),
    /// A tool call is about to run.
    ToolCallStarted {
        id: String,
        name: String,
        args_json: String,
    },
    /// A tool call finished; `output` is what the model will see.
    ToolCallFinished {
        id: String,
        name: String,
        output: String,
        is_error: bool,
    },
    /// Usage for one model response.
    Usage(Usage),
    TurnFinished(TurnOutcome),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TurnOutcome {
    /// The model answered without asking for more tools.
    Completed,
    /// The user cancelled; any partial text was kept in the session.
    Cancelled,
    /// The step cap was reached while the model still wanted tools.
    MaxSteps,
}

#[derive(Debug, thiserror::Error)]
pub enum AgentError {
    #[error(transparent)]
    Provider(#[from] ProviderError),
}

pub struct Agent {
    provider: Arc<dyn Provider>,
    tools: Arc<ToolRegistry>,
    workspace: Workspace,
    approvals: Arc<dyn ApprovalGate>,
    config: AgentConfig,
}

/// A tool call being assembled from stream events.
#[derive(Debug, Default)]
struct PendingCall {
    id: String,
    name: String,
    args_json: String,
}

/// Everything one model response produced.
#[derive(Debug, Default)]
struct StepOutput {
    text: String,
    calls: Vec<PendingCall>,
}

enum StreamEnd {
    Done,
    Cancelled,
    Failed(ProviderError),
}

impl Agent {
    /// An agent whose non-read tool calls are denied (see [`Agent::with_approval_gate`]).
    pub fn new(
        provider: Arc<dyn Provider>,
        tools: Arc<ToolRegistry>,
        workspace: Workspace,
        config: AgentConfig,
    ) -> Self {
        Self {
            provider,
            tools,
            workspace,
            approvals: Arc::new(ReadOnlyGate),
            config,
        }
    }

    pub fn with_approval_gate(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approvals = gate;
        self
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// The registry's tools in the provider-neutral form sent with each request.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.tools
            .iter()
            .map(|tool| ToolSpec {
                name: tool.name().to_owned(),
                description: tool.description().to_owned(),
                json_schema: tool.schema(),
            })
            .collect()
    }

    /// Runs one user turn to completion, appending everything to `session`.
    ///
    /// Progress goes to `events` (send errors are ignored, so a dropped receiver doesn't stop the
    /// turn). Cancelling `cancel` stops the stream or the running tool; partial assistant text
    /// is kept, and every tool call in the session still gets a result. A provider error ends
    /// the turn with `Err`, also keeping partial text.
    pub async fn run_turn(
        &self,
        user_input: impl Into<String>,
        session: &mut Session,
        events: &mpsc::Sender<AgentEvent>,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, AgentError> {
        let span = info_span!("turn", model = %self.config.model);
        self.turn(user_input.into(), session, events, cancel)
            .instrument(span)
            .await
    }

    async fn turn(
        &self,
        user_input: String,
        session: &mut Session,
        events: &mpsc::Sender<AgentEvent>,
        cancel: CancellationToken,
    ) -> Result<TurnOutcome, AgentError> {
        session.messages.push(Message::user(user_input));
        let specs = self.tool_specs();
        let ctx = ToolCtx::new(self.workspace.clone(), cancel.clone());

        for step in 1..=self.config.max_steps {
            debug!(step, "requesting model response");
            let request = ChatRequest {
                model: self.config.model.clone(),
                messages: session.messages.clone(),
                tools: specs.clone(),
                max_output_tokens: self.config.max_output_tokens,
            };
            let mut output = StepOutput::default();
            match self
                .stream_step(request, session, events, &cancel, &mut output)
                .await
            {
                StreamEnd::Done => {}
                StreamEnd::Cancelled => {
                    keep_partial_text(session, output.text);
                    return Ok(finish(events, TurnOutcome::Cancelled).await);
                }
                StreamEnd::Failed(err) => {
                    warn!(error = %err, "model response failed");
                    keep_partial_text(session, output.text);
                    return Err(err.into());
                }
            }

            let mut content = Vec::new();
            if !output.text.is_empty() {
                content.push(ContentPart::Text { text: output.text });
            }
            content.extend(output.calls.iter().map(|call| ContentPart::ToolCall {
                id: call.id.clone(),
                name: call.name.clone(),
                args_json: call.args_json.clone(),
            }));
            if !content.is_empty() {
                session
                    .messages
                    .push(Message::new(Role::Assistant, content));
            }
            if output.calls.is_empty() {
                return Ok(finish(events, TurnOutcome::Completed).await);
            }

            for call in &output.calls {
                let result = if cancel.is_cancelled() {
                    ToolOutput::error("cancelled by the user")
                } else {
                    let _ = events
                        .send(AgentEvent::ToolCallStarted {
                            id: call.id.clone(),
                            name: call.name.clone(),
                            args_json: call.args_json.clone(),
                        })
                        .await;
                    self.execute(call, &ctx).await
                };
                let text = truncate_head_tail(&result.content, self.config.max_tool_output_bytes)
                    .into_owned();
                let _ = events
                    .send(AgentEvent::ToolCallFinished {
                        id: call.id.clone(),
                        name: call.name.clone(),
                        output: text.clone(),
                        is_error: result.is_error,
                    })
                    .await;
                session
                    .messages
                    .push(Message::tool_result(&call.id, text, result.is_error));
            }
            if cancel.is_cancelled() {
                return Ok(finish(events, TurnOutcome::Cancelled).await);
            }
        }
        Ok(finish(events, TurnOutcome::MaxSteps).await)
    }

    /// Streams one model response into `output`, forwarding text and usage as it arrives.
    async fn stream_step(
        &self,
        request: ChatRequest,
        session: &mut Session,
        events: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
        output: &mut StepOutput,
    ) -> StreamEnd {
        let stream = tokio::select! {
            biased;
            _ = cancel.cancelled() => return StreamEnd::Cancelled,
            stream = self.provider.stream(request, cancel.clone()) => stream,
        };
        let mut stream = match stream {
            Ok(stream) => stream,
            Err(ProviderError::Cancelled) => return StreamEnd::Cancelled,
            Err(err) => return StreamEnd::Failed(err),
        };

        loop {
            let event = tokio::select! {
                biased;
                _ = cancel.cancelled() => return StreamEnd::Cancelled,
                event = stream.next() => event,
            };
            match event {
                None if cancel.is_cancelled() => return StreamEnd::Cancelled,
                None => return StreamEnd::Failed(ProviderError::UnexpectedEof),
                Some(Err(ProviderError::Cancelled)) => return StreamEnd::Cancelled,
                Some(Err(err)) => return StreamEnd::Failed(err),
                Some(Ok(event)) => match event {
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
                        session.usage += usage;
                        let _ = events.send(AgentEvent::Usage(usage)).await;
                    }
                    StreamEvent::Done(reason) => {
                        debug!(?reason, "model response done");
                        return StreamEnd::Done;
                    }
                },
            }
        }
    }

    /// Runs one tool call: lookup, argument parsing, approval, then the call itself under the
    /// timeout and the turn's cancellation.
    async fn execute(&self, call: &PendingCall, ctx: &ToolCtx) -> ToolOutput {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolOutput::error(format!("unknown tool: {}", call.name));
        };
        let args: Value = if call.args_json.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            match serde_json::from_str(&call.args_json) {
                Ok(args) => args,
                Err(err) => return ToolOutput::error(format!("invalid JSON arguments: {err}")),
            }
        };

        let risk = tool.risk(&args);
        if risk != Risk::Read {
            let request = ApprovalRequest {
                tool: call.name.clone(),
                args: args.clone(),
                risk,
            };
            if let Decision::Deny { reason } = self.approvals.decide(request).await {
                return ToolOutput::error(format!("denied: {reason}"));
            }
        }

        let timeout = self.config.tool_timeout;
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => ToolOutput::error("cancelled by the user"),
            result = tokio::time::timeout(timeout, tool.call(args, ctx)) => match result {
                Ok(output) => output,
                Err(_) => ToolOutput::error(format!("timed out after {}s", timeout.as_secs_f64())),
            },
        }
    }
}

fn keep_partial_text(session: &mut Session, text: String) {
    if !text.is_empty() {
        session.messages.push(Message::assistant(text));
    }
}

async fn finish(events: &mpsc::Sender<AgentEvent>, outcome: TurnOutcome) -> TurnOutcome {
    let _ = events.send(AgentEvent::TurnFinished(outcome)).await;
    outcome
}
