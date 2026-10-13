//! Running one tool call through the approval policy: lookup, argument parsing, validation,
//! approval (mode, project rule or gate), the decision log, then the call itself under a timeout
//! and the turn's cancellation.
//!
//! Sandboxed tools run under the OS sandbox whatever the mode. An approved call may also write
//! the outside paths it names (the prompt lists them). Only the user's
//! [`Decision::ApproveUnsandboxed`] runs a call without the sandbox; when the sandbox blocks an
//! approved call, the gate is asked once whether to rerun it that way.
//!
//! The [`Agent`](crate::Agent) uses this for model providers, and `rozu mcp-serve` uses it for
//! tool calls that arrive from a CLI-backed provider, so both paths apply the same checks.

use std::sync::Arc;
use std::time::Duration;

use rozu_tools::{
    ExecPermissions, Risk, Sandbox, SessionTemp, Tool, ToolCtx, ToolOutput, ToolRegistry,
    Workspace, sandbox_blocked,
};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent::{AgentEvent, DEFAULT_TOOL_TIMEOUT};
use crate::approval::{
    ApprovalGate, ApprovalPolicy, ApprovalRecord, ApprovalRequest, ApprovalResolution, Decision,
    DecisionSource, PendingApproval,
};
use crate::audit::AuditLog;

/// The tools plus everything that decides whether and how a call runs.
#[derive(Clone)]
pub struct ToolExecutor {
    pub tools: Arc<ToolRegistry>,
    pub gate: Arc<dyn ApprovalGate>,
    pub policy: Arc<ApprovalPolicy>,
    pub sandbox: Arc<dyn Sandbox>,
    /// Limit for calls whose tool doesn't set its own ([`Tool::timeout`]).
    pub default_timeout: Duration,
    /// Where every decision is appended (`None`: only the trace log).
    pub audit: Option<Arc<AuditLog>>,
    /// The session's private temp dir, the only temp location sandboxed commands may write.
    pub temp: Option<Arc<SessionTemp>>,
    /// The CLI-backed provider whose requests this executor decides (e.g. `codex`), recorded in
    /// the audit log.
    pub provider: Option<String>,
    /// The model provider whose agent loop this executor serves (set by [`crate::Agent`]),
    /// named in [`PendingApproval`]s when `provider` isn't set. Not written to the audit log.
    pub model_provider: Option<String>,
}

impl ToolExecutor {
    /// An executor with its own session temp dir and no audit log.
    pub fn new(
        tools: Arc<ToolRegistry>,
        gate: Arc<dyn ApprovalGate>,
        policy: Arc<ApprovalPolicy>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        let temp = match SessionTemp::create("session") {
            Ok(temp) => Some(Arc::new(temp)),
            Err(err) => {
                warn!(error = %err, "could not create the session temp dir; commands get no writable temp dir");
                None
            }
        };
        Self {
            tools,
            gate,
            policy,
            sandbox,
            default_timeout: DEFAULT_TOOL_TIMEOUT,
            audit: None,
            temp,
            provider: None,
            model_provider: None,
        }
    }

    pub fn with_audit(mut self, audit: Option<Arc<AuditLog>>) -> Self {
        self.audit = audit;
        self
    }

    pub fn with_provider(mut self, provider: Option<String>) -> Self {
        self.provider = provider;
        self
    }

    /// A context for calls in `workspace` with this executor's sandbox and temp dir.
    pub fn ctx(&self, workspace: Workspace, cancel: CancellationToken) -> ToolCtx {
        let ctx = ToolCtx::new(workspace, cancel).with_sandbox(self.sandbox.clone());
        match &self.temp {
            Some(temp) => ctx.with_temp_dir(temp.path()),
            None => ctx,
        }
    }

    /// Runs one call. Every call that reaches the approval step gets exactly one
    /// [`AgentEvent::Approval`] on `events` (send errors are ignored), plus one more if the
    /// sandbox blocked it and the gate was asked about rerunning it.
    pub async fn execute(
        &self,
        call_id: &str,
        name: &str,
        args_json: &str,
        ctx: &ToolCtx,
        events: &mpsc::Sender<AgentEvent>,
    ) -> ToolOutput {
        let Some(tool) = self.tools.get(name) else {
            return ToolOutput::error(format!("unknown tool: {name}"));
        };
        let args: Value = if args_json.trim().is_empty() {
            Value::Object(Default::default())
        } else {
            match serde_json::from_str(args_json) {
                Ok(args) => args,
                Err(err) => return ToolOutput::error(format!("invalid JSON arguments: {err}")),
            }
        };
        if let Err(output) = tool.validate(&args, ctx) {
            return output;
        }

        let risk = tool.risk(&args, ctx);
        let wants_network = tool.wants_network(&args, ctx);
        let outside = tool.outside_writes(&args, ctx);
        let confinement = self.policy.confinement(
            tool.as_ref(),
            wants_network,
            &outside,
            self.sandbox.as_ref(),
        );
        let request = ApprovalRequest {
            call_id: call_id.to_owned(),
            tool: name.to_owned(),
            allow_rule: self.policy.allow_rule(tool.as_ref(), &args, risk),
            preview: tool.preview(&args),
            args: args.clone(),
            risk,
            sandbox: confinement.mode,
            network: confinement.permissions.network,
            extra_writes: confinement.permissions.extra_writable.clone(),
            outside_unknown: confinement.outside_unknown,
            blocked_retry: false,
        };
        let (decision, source) = self
            .decide(&request, tool.as_ref(), &ctx.cancel, events)
            .await;
        let permissions = match decision {
            // Only the user may lift the sandbox, and only where there is one.
            Decision::ApproveUnsandboxed
                if source == DecisionSource::Gate && request.can_run_unsandboxed() =>
            {
                ExecPermissions::unsandboxed()
            }
            Decision::Approve | Decision::AlwaysAllowPrefix | Decision::ApproveUnsandboxed => {
                confinement.permissions
            }
            Decision::Deny { reason } if source == DecisionSource::Cancelled => {
                return ToolOutput::error(reason);
            }
            Decision::Deny { reason } => {
                return ToolOutput::error(format!("denied: {reason}"));
            }
        };

        let sandboxed = permissions.sandbox && request.can_run_unsandboxed();
        let output = self.run(tool.as_ref(), &args, ctx, permissions).await;
        if !(sandboxed && sandbox_blocked(&output.content)) || ctx.cancel.is_cancelled() {
            return output;
        }

        // The sandbox refused something the call was approved to do: offer the escape once.
        let retry = ApprovalRequest {
            blocked_retry: true,
            allow_rule: None,
            ..request
        };
        let (decision, source) = self.ask_gate(&retry, &ctx.cancel, events).await;
        let _ = events
            .send(AgentEvent::Approval(
                self.record_decision(&retry, &decision, &source),
            ))
            .await;
        if decision != Decision::ApproveUnsandboxed || source != DecisionSource::Gate {
            return output;
        }
        let rerun = self
            .run(tool.as_ref(), &args, ctx, ExecPermissions::unsandboxed())
            .await;
        ToolOutput {
            content: format!(
                "[the sandbox blocked the first run; the user approved rerunning it unsandboxed]\n{}",
                rerun.content
            ),
            is_error: rerun.is_error,
        }
    }

    /// Runs an approved call with `permissions`, under its timeout and the turn's cancellation.
    async fn run(
        &self,
        tool: &dyn Tool,
        args: &Value,
        ctx: &ToolCtx,
        permissions: ExecPermissions,
    ) -> ToolOutput {
        let ctx = ctx.clone().with_permissions(permissions);
        let timeout = tool.timeout(args).unwrap_or(self.default_timeout);
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => ToolOutput::error("cancelled by the user"),
            result = tokio::time::timeout(timeout, tool.call(args.clone(), &ctx)) => match result {
                Ok(output) => output,
                Err(_) => ToolOutput::error(format!("timed out after {}s", timeout.as_secs_f64())),
            },
        }
    }

    /// Asks the gate (never the policy), unless the turn is cancelled first. When the gate asks
    /// a person, the prompt is reported on `events` as it opens
    /// ([`AgentEvent::ApprovalPending`]) and as it closes ([`AgentEvent::ApprovalResolved`]).
    async fn ask_gate(
        &self,
        request: &ApprovalRequest,
        cancel: &CancellationToken,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (Decision, DecisionSource) {
        let prompt = self.gate.asks_user() && !cancel.is_cancelled();
        if prompt {
            let _ = events
                .send(AgentEvent::ApprovalPending(PendingApproval::new(
                    request,
                    self.asker(),
                )))
                .await;
        }
        let (decision, source) = tokio::select! {
            biased;
            _ = cancel.cancelled() => (
                Decision::Deny { reason: "cancelled by the user".into() },
                DecisionSource::Cancelled,
            ),
            decision = self.gate.decide(request.clone()) => (decision, DecisionSource::Gate),
        };
        if prompt {
            let _ = events
                .send(AgentEvent::ApprovalResolved {
                    request_id: request.call_id.clone(),
                    resolution: ApprovalResolution::of(&decision, &source),
                })
                .await;
        }
        (decision, source)
    }

    /// Who is asking, for [`PendingApproval::provider`].
    fn asker(&self) -> &str {
        self.provider
            .as_deref()
            .or(self.model_provider.as_deref())
            .unwrap_or("rozu")
    }

    /// Decides `request`: auto-approved by the policy, or asked through the gate. Logs the
    /// decision, stores an "always allow" rule if the user chose one, and returns it.
    ///
    /// `tool` supplies the rule matching ([`Tool::rules_allow`]); it need not be in the
    /// registry (the MCP server decides on tools the vendor CLI runs itself).
    pub async fn decide(
        &self,
        request: &ApprovalRequest,
        tool: &dyn Tool,
        cancel: &CancellationToken,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (Decision, DecisionSource) {
        let (decision, source) = match self.policy.auto_approval(request, tool) {
            Some(source) => (Decision::Approve, source),
            None => self.ask_gate(request, cancel, events).await,
        };
        let _ = events
            .send(AgentEvent::Approval(
                self.record_decision(request, &decision, &source),
            ))
            .await;
        if decision == Decision::AlwaysAllowPrefix {
            if let Err(err) = self.policy.remember(request) {
                warn!(error = %err, "could not save the always-allow rule");
            }
        }
        (decision, source)
    }

    /// Decides `request` by asking the gate (the user) whatever the mode or rules say, e.g. for a
    /// vendor CLI's sandbox or network escalation. Logs the decision like [`Self::decide`]; an
    /// "always allow" answer is treated as a one-time approval and stores nothing.
    pub async fn ask_user(
        &self,
        request: &ApprovalRequest,
        cancel: &CancellationToken,
        events: &mpsc::Sender<AgentEvent>,
    ) -> (Decision, DecisionSource) {
        let (mut decision, source) = self.ask_gate(request, cancel, events).await;
        if decision == Decision::AlwaysAllowPrefix {
            decision = Decision::Approve;
        }
        let _ = events
            .send(AgentEvent::Approval(
                self.record_decision(request, &decision, &source),
            ))
            .await;
        (decision, source)
    }

    /// Records a decision in the trace log (target `rozu::approval`) and the audit log, and
    /// returns its log entry.
    pub fn record_decision(
        &self,
        request: &ApprovalRequest,
        decision: &Decision,
        source: &DecisionSource,
    ) -> ApprovalRecord {
        let record = log_decision(request, decision, source);
        if let Some(audit) = &self.audit {
            audit.record_for(&record, self.policy.mode(), self.provider.as_deref());
        }
        record
    }
}

/// Records a decision in the trace log (target `rozu::approval`) and returns its log entry.
/// [`ToolExecutor::record_decision`] also writes the audit log; prefer it.
pub fn log_decision(
    request: &ApprovalRequest,
    decision: &Decision,
    source: &DecisionSource,
) -> ApprovalRecord {
    let verdict = match decision {
        Decision::Approve => "approved",
        Decision::AlwaysAllowPrefix => "approved (always allow)",
        Decision::ApproveUnsandboxed => "approved unsandboxed (this call only)",
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
            extra_writes = ?request.extra_writes,
            blocked_retry = request.blocked_retry,
            decided_by = %source,
            reason,
            "{verdict}"
        );
    }
    ApprovalRecord {
        call_id: request.call_id.clone(),
        tool: request.tool.clone(),
        risk: request.risk,
        preview: request.preview.clone(),
        sandbox: request.sandbox,
        network: request.network,
        extra_writes: request.extra_writes.clone(),
        blocked_retry: request.blocked_retry,
        decision: decision.clone(),
        source: source.clone(),
    }
}
