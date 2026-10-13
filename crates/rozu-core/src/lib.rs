//! Core of the rozu coding agent.
//!
//! Owns the agent loop (stream → tool calls → policy → approval → execute → tool results), the
//! session model, the `Command` / `Event` channels the UI and CLI talk through, and policy such as
//! turn and token caps. It has no UI and does no HTTP itself; providers and tools plug in from
//! their own crates.

pub mod agent;
pub mod approval;
pub mod audit;
pub mod exec;
pub mod model;
pub mod provider;
pub mod telemetry;

pub use agent::{
    Agent, AgentConfig, AgentError, AgentEvent, DEFAULT_MAX_STEPS, DEFAULT_TOOL_TIMEOUT, Session,
    TurnOutcome, TurnUsage,
};
pub use approval::{
    ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalRecord, ApprovalRequest, ApprovalRules,
    Confinement, Decision, DecisionSource, PrefixRule, ReadOnlyGate, SandboxMode, approvals_path,
};
pub use audit::{AUDIT_LOG_ENV, AuditLog, audit_path_from_env, default_audit_path, new_session_id};
pub use exec::{ToolExecutor, log_decision};
pub use model::{
    Billing, ChatRequest, ContentPart, LongContextPrices, Message, ModelInfo, Role, StopReason,
    StreamEvent, ToolSpec, Usage,
};
pub use provider::{EventStream, Provider, ProviderError, ProviderKind, ProviderResult};
pub use telemetry::{LogTarget, default_log_path, init_tracing, redact};

/// Version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_semver() {
        assert_eq!(VERSION.split('.').count(), 3);
    }
}
