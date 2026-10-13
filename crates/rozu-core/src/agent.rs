//! The agent loop: stream a response, run the tool calls it asks for, feed the results back, and
//! repeat until the model answers, the step cap is hit, or the user cancels.

use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rozu_tools::{
    MAX_OUTPUT_BYTES, Risk, Sandbox, ToolCtx, ToolOutput, ToolRegistry, Workspace,
    platform_sandbox, truncate_head_tail,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info, info_span, warn};

use crate::approval::{
    ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalRequest, Decision, DecisionSource,
    ReadOnlyGate, SandboxMode,
};
use crate::model::{
    ChatRequest, ContentPart, Message, ModelInfo, Role, StreamEvent, ToolSpec, Usage,
};
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
    /// Prices for cost accounting. `None` (an unlisted model) means tokens are counted but cost
    /// is unknown.
    pub model_info: Option<ModelInfo>,
}

impl AgentConfig {
    pub fn new(model: impl Into<String>) -> Self {
        Self {
            model: model.into(),
            max_steps: DEFAULT_MAX_STEPS,
            tool_timeout: DEFAULT_TOOL_TIMEOUT,
            max_output_tokens: None,
            max_tool_output_bytes: MAX_OUTPUT_BYTES,
            model_info: None,
        }
    }

    pub fn with_model_info(mut self, info: Option<ModelInfo>) -> Self {
        self.model_info = info;
        self
    }
}

/// Tokens and estimated cost of one user turn (all of its model requests).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TurnUsage {
    pub usage: Usage,
    /// `None` when the model has no known prices.
    pub cost_usd: Option<f64>,
}

/// The conversation so far plus running totals. A session can outlive an [`Agent`], so the
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

    /// Adds one response's usage to the totals, pricing it with `model`.
    fn record_usage(&mut self, usage: Usage, model: Option<&ModelInfo>) {
        self.usage += usage;
        self.last_turn.usage += usage;
        if let Some(model) = model {
            let cost = model.cost_usd(&usage);
            self.cost_usd += cost;
            *self.last_turn.cost_usd.get_or_insert(0.0) += cost;
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
    /// An approval decision for a tool call (every call gets one, auto-approved or not).
    Approval(ApprovalRecord),
    /// Usage for one model response.
    Usage(Usage),
    TurnFinished(TurnOutcome),
}

/// The decision log entry for one tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRecord {
    pub call_id: String,
    pub tool: String,
    pub risk: Risk,
    pub preview: String,
    pub sandbox: SandboxMode,
    pub network: bool,
    pub decision: Decision,
    pub source: DecisionSource,
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
    policy: Arc<ApprovalPolicy>,
    sandbox: Arc<dyn Sandbox>,
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
    /// An agent in [`ApprovalMode::ReadOnly`] whose gate denies everything, so only reads run
    /// until [`Agent::with_approval_policy`] / [`Agent::with_approval_gate`] say otherwise.
    /// Shell commands use [`platform_sandbox`].
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
            policy: Arc::new(ApprovalPolicy::new(ApprovalMode::ReadOnly)),
            sandbox: platform_sandbox(),
            config,
        }
    }

    pub fn with_approval_gate(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.approvals = gate;
        self
    }

    pub fn with_approval_policy(mut self, policy: Arc<ApprovalPolicy>) -> Self {
        self.policy = policy;
        self
    }

    pub fn with_sandbox(mut self, sandbox: Arc<dyn Sandbox>) -> Self {
        self.sandbox = sandbox;
        self
    }

    pub fn approval_policy(&self) -> &ApprovalPolicy {
        &self.policy
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
        session.last_turn = TurnUsage {
            usage: Usage::default(),
            cost_usd: self.config.model_info.as_ref().map(|_| 0.0),
        };
        let specs = self.tool_specs();
        let ctx =
            ToolCtx::new(self.workspace.clone(), cancel.clone()).with_sandbox(self.sandbox.clone());

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
                    self.execute(call, &ctx, events).await
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
                        session.record_usage(usage, self.config.model_info.as_ref());
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

    /// Runs one tool call: lookup, argument parsing, validation, approval, then the call itself
    /// under the timeout and the turn's cancellation.
    async fn execute(
        &self,
        call: &PendingCall,
        ctx: &ToolCtx,
        events: &mpsc::Sender<AgentEvent>,
    ) -> ToolOutput {
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
        if let Err(output) = tool.validate(&args, ctx) {
            return output;
        }

        let risk = tool.risk(&args, ctx);
        let wants_network = tool.wants_network(&args, ctx);
        let (sandbox, permissions) =
            self.policy
                .confinement(tool.as_ref(), risk, wants_network, self.sandbox.as_ref());
        let request = ApprovalRequest {
            call_id: call.id.clone(),
            tool: call.name.clone(),
            allow_prefix: self.policy.allow_prefix(tool.as_ref(), &args, risk),
            preview: tool.preview(&args),
            args: args.clone(),
            risk,
            sandbox,
            network: permissions.network,
        };
        let (decision, source) = match self.policy.auto_approval(&request, tool.as_ref()) {
            Some(source) => (Decision::Approve, source),
            None => tokio::select! {
                biased;
                _ = ctx.cancel.cancelled() => (
                    Decision::Deny { reason: "cancelled by the user".into() },
                    DecisionSource::Cancelled,
                ),
                decision = self.approvals.decide(request.clone()) => (decision, DecisionSource::Gate),
            },
        };
        self.log_decision(&request, &decision, &source, events)
            .await;

        match decision {
            Decision::Approve => {}
            Decision::AlwaysAllowPrefix => {
                if let Err(err) = self.policy.remember(&request) {
                    warn!(error = %err, "could not save the always-allow rule");
                }
            }
            Decision::Deny { reason } if source == DecisionSource::Cancelled => {
                return ToolOutput::error(reason);
            }
            Decision::Deny { reason } => {
                return ToolOutput::error(format!("denied: {reason}"));
            }
        }

        let ctx = ctx.clone().with_permissions(permissions);
        let timeout = tool.timeout(&args).unwrap_or(self.config.tool_timeout);
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => ToolOutput::error("cancelled by the user"),
            result = tokio::time::timeout(timeout, tool.call(args, &ctx)) => match result {
                Ok(output) => output,
                Err(_) => ToolOutput::error(format!("timed out after {}s", timeout.as_secs_f64())),
            },
        }
    }

    /// Records a decision in the trace log and as an [`AgentEvent::Approval`].
    async fn log_decision(
        &self,
        request: &ApprovalRequest,
        decision: &Decision,
        source: &DecisionSource,
        events: &mpsc::Sender<AgentEvent>,
    ) {
        let verdict = match decision {
            Decision::Approve => "approved",
            Decision::AlwaysAllowPrefix => "approved (always allow)",
            Decision::Deny { .. } => "denied",
        };
        let reason = match decision {
            Decision::Deny { reason } => reason.as_str(),
            _ => "",
        };
        if request.risk == Risk::Read && matches!(source, DecisionSource::Mode(_)) {
            debug!(tool = %request.tool, call_id = %request.call_id, "read auto-approved");
        } else {
            info!(
                target: "rozu::approval",
                tool = %request.tool,
                call_id = %request.call_id,
                risk = %request.risk,
                sandbox = %request.sandbox,
                network = request.network,
                decided_by = %source,
                reason,
                "{verdict}"
            );
        }
        let _ = events
            .send(AgentEvent::Approval(ApprovalRecord {
                call_id: request.call_id.clone(),
                tool: request.tool.clone(),
                risk: request.risk,
                preview: request.preview.clone(),
                sandbox: request.sandbox,
                network: request.network,
                decision: decision.clone(),
                source: source.clone(),
            }))
            .await;
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
