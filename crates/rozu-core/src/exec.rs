//! Running one tool call through the approval policy: lookup, argument parsing, validation,
//! approval (mode, project rule or gate), the decision log, then the call itself under a timeout
//! and the turn's cancellation.
//!
//! The [`Agent`](crate::Agent) uses this for model providers, and `rozu mcp-serve` uses it for
//! tool calls that arrive from a CLI-backed provider, so both paths apply the same checks.

use std::sync::Arc;
use std::time::Duration;

use rozu_tools::{Risk, Sandbox, Tool, ToolCtx, ToolOutput, ToolRegistry, Workspace};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::agent::{AgentEvent, DEFAULT_TOOL_TIMEOUT};
use crate::approval::{
    ApprovalGate, ApprovalPolicy, ApprovalRecord, ApprovalRequest, Decision, DecisionSource,
};

/// The tools plus everything that decides whether and how a call runs.
#[derive(Clone)]
pub struct ToolExecutor {
    pub tools: Arc<ToolRegistry>,
    pub gate: Arc<dyn ApprovalGate>,
    pub policy: Arc<ApprovalPolicy>,
    pub sandbox: Arc<dyn Sandbox>,
    /// Limit for calls whose tool doesn't set its own ([`Tool::timeout`]).
    pub default_timeout: Duration,
}

impl ToolExecutor {
    pub fn new(
        tools: Arc<ToolRegistry>,
        gate: Arc<dyn ApprovalGate>,
        policy: Arc<ApprovalPolicy>,
        sandbox: Arc<dyn Sandbox>,
    ) -> Self {
        Self {
            tools,
            gate,
            policy,
            sandbox,
            default_timeout: DEFAULT_TOOL_TIMEOUT,
        }
    }

    /// A context for calls in `workspace` with this executor's sandbox.
    pub fn ctx(&self, workspace: Workspace, cancel: CancellationToken) -> ToolCtx {
        ToolCtx::new(workspace, cancel).with_sandbox(self.sandbox.clone())
    }

    /// Runs one call. Every call that reaches the approval step gets exactly one
    /// [`AgentEvent::Approval`] on `events` (send errors are ignored).
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
        let (sandbox, permissions) =
            self.policy
                .confinement(tool.as_ref(), risk, wants_network, self.sandbox.as_ref());
        let request = ApprovalRequest {
            call_id: call_id.to_owned(),
            tool: name.to_owned(),
            allow_prefix: self.policy.allow_prefix(tool.as_ref(), &args, risk),
            preview: tool.preview(&args),
            args: args.clone(),
            risk,
            sandbox,
            network: permissions.network,
        };
        let (decision, source) = self
            .decide(&request, tool.as_ref(), &ctx.cancel, events)
            .await;
        match decision {
            Decision::Approve | Decision::AlwaysAllowPrefix => {}
            Decision::Deny { reason } if source == DecisionSource::Cancelled => {
                return ToolOutput::error(reason);
            }
            Decision::Deny { reason } => {
                return ToolOutput::error(format!("denied: {reason}"));
            }
        }

        let ctx = ctx.clone().with_permissions(permissions);
        let timeout = tool.timeout(&args).unwrap_or(self.default_timeout);
        tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => ToolOutput::error("cancelled by the user"),
            result = tokio::time::timeout(timeout, tool.call(args, &ctx)) => match result {
                Ok(output) => output,
                Err(_) => ToolOutput::error(format!("timed out after {}s", timeout.as_secs_f64())),
            },
        }
    }

    /// Decides `request`: auto-approved by the policy, or asked through the gate. Logs the
    /// decision, stores an "always allow" rule if the user chose one, and returns it.
    ///
    /// `tool` supplies the rule matching ([`Tool::prefix_allows`]); it need not be in the
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
            None => tokio::select! {
                biased;
                _ = cancel.cancelled() => (
                    Decision::Deny { reason: "cancelled by the user".into() },
                    DecisionSource::Cancelled,
                ),
                decision = self.gate.decide(request.clone()) => (decision, DecisionSource::Gate),
            },
        };
        let _ = events
            .send(AgentEvent::Approval(log_decision(
                request, &decision, &source,
            )))
            .await;
        if decision == Decision::AlwaysAllowPrefix {
            if let Err(err) = self.policy.remember(request) {
                warn!(error = %err, "could not save the always-allow rule");
            }
        }
        (decision, source)
    }
}

/// Records a decision in the trace log (target `rozu::approval`) and returns its log entry.
pub fn log_decision(
    request: &ApprovalRequest,
    decision: &Decision,
    source: &DecisionSource,
) -> ApprovalRecord {
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
    ApprovalRecord {
        call_id: request.call_id.clone(),
        tool: request.tool.clone(),
        risk: request.risk,
        preview: request.preview.clone(),
        sandbox: request.sandbox,
        network: request.network,
        decision: decision.clone(),
        source: source.clone(),
    }
}
