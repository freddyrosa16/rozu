//! The agent loop: stream a response, run the tool calls it asks for, feed the results back, and
//! repeat until the model answers, the step cap is hit, or the user cancels.

use std::sync::Arc;
use std::time::Duration;

use rozu_tools::{
    MAX_OUTPUT_BYTES, Sandbox, SessionTemp, ToolOutput, ToolRegistry, Workspace, platform_sandbox,
    truncate_head_tail,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{Instrument, debug, info_span, warn};

pub use crate::approval::ApprovalRecord;
use crate::approval::{
    ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalResolution, PendingApproval, ReadOnlyGate,
};
use crate::audit::AuditLog;
use crate::compact::MemoryPolicy;
use crate::exec::ToolExecutor;
use crate::model::{ChatRequest, ContentPart, Message, ModelInfo, Role, ToolSpec, Usage};
use crate::provider::{Provider, ProviderError, ProviderKind};

mod compaction;
mod session;
mod stream;

use stream::{StepOutput, StreamEnd};

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
    /// is unknown; so does a subscription model ([`ModelInfo::is_metered`] is false).
    pub model_info: Option<ModelInfo>,
    /// Compaction thresholds, as shares of `model_info`'s context window (no window: never).
    pub memory: MemoryPolicy,
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
            memory: MemoryPolicy::DEFAULT,
        }
    }

    pub fn with_model_info(mut self, info: Option<ModelInfo>) -> Self {
        self.model_info = info;
        self
    }

    /// The model info, if its usage has a USD cost.
    fn priced_model(&self) -> Option<&ModelInfo> {
        self.model_info.as_ref().filter(|info| info.is_metered())
    }
}

pub use session::{Session, TurnUsage};

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
    /// An approval prompt is open and waiting for the user. Never sent for decisions made
    /// without asking (mode, rule, policy, a non-interactive session).
    ApprovalPending(PendingApproval),
    /// The prompt opened by the [`AgentEvent::ApprovalPending`] with this id closed. Comes
    /// before that call's [`AgentEvent::Approval`] record.
    ApprovalResolved {
        request_id: String,
        resolution: ApprovalResolution,
    },
    /// Usage for one model response (for an agent backend: for the whole turn).
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
    exec: ToolExecutor,
    workspace: Workspace,
    config: AgentConfig,
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
        let mut exec = ToolExecutor::new(
            tools,
            Arc::new(ReadOnlyGate),
            Arc::new(ApprovalPolicy::new(ApprovalMode::ReadOnly)),
            platform_sandbox(),
        );
        exec.default_timeout = config.tool_timeout;
        exec.model_provider = Some(provider.id().to_owned());
        Self {
            provider,
            exec,
            workspace,
            config,
        }
    }

    pub fn with_approval_gate(mut self, gate: Arc<dyn ApprovalGate>) -> Self {
        self.exec.gate = gate;
        self
    }

    pub fn with_approval_policy(mut self, policy: Arc<ApprovalPolicy>) -> Self {
        self.exec.policy = policy;
        self
    }

    pub fn with_sandbox(mut self, sandbox: Arc<dyn Sandbox>) -> Self {
        self.exec.sandbox = sandbox;
        self
    }

    /// Appends every approval decision to `audit`.
    pub fn with_audit(mut self, audit: Option<Arc<AuditLog>>) -> Self {
        self.exec.audit = audit;
        self
    }

    /// Shares one session temp dir across agents (e.g. after a `/model` switch).
    pub fn with_session_temp(mut self, temp: Option<Arc<SessionTemp>>) -> Self {
        self.exec.temp = temp;
        self
    }

    pub fn approval_policy(&self) -> &ApprovalPolicy {
        &self.exec.policy
    }

    pub fn config(&self) -> &AgentConfig {
        &self.config
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// The registry's tools in the provider-neutral form sent with each request.
    pub fn tool_specs(&self) -> Vec<ToolSpec> {
        self.exec
            .tools
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
        session.last_compaction = None;
        session.last_turn = TurnUsage {
            usage: Usage::default(),
            cost_usd: self.config.priced_model().map(|_| 0.0),
        };
        if self.provider.kind() == ProviderKind::AgentBackend {
            return self.backend_turn(session, events, cancel).await;
        }
        let specs = self.tool_specs();
        let ctx = self.exec.ctx(self.workspace.clone(), cancel.clone());

        for step in 1..=self.config.max_steps {
            if let Some(done) = self.compact_with(session, &cancel, false).await {
                session.last_compaction = Some(done);
            }
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
                    self.exec
                        .execute(&call.id, &call.name, &call.args_json, &ctx, events)
                        .await
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
