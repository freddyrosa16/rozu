//! Core of the rozu coding agent.
//!
//! Owns the agent loop (stream → tool calls → policy → approval → execute → tool results), the
//! session model, the `Command` / `Event` channels the UI and CLI talk through, and policy such as
//! turn and token caps. It has no UI and does no HTTP itself; providers and tools plug in from
//! their own crates.

pub mod agent;
pub mod approval;
pub mod model;
pub mod provider;
pub mod telemetry;

pub use agent::{
    Agent, AgentConfig, AgentError, AgentEvent, DEFAULT_MAX_STEPS, DEFAULT_TOOL_TIMEOUT, Session,
    TurnOutcome,
};
pub use approval::{ApprovalGate, ApprovalRequest, Decision, ReadOnlyGate};
pub use model::{
    ChatRequest, ContentPart, Message, ModelInfo, Role, StopReason, StreamEvent, ToolSpec, Usage,
};
pub use provider::{EventStream, Provider, ProviderError, ProviderResult};
pub use telemetry::{init_tracing, redact};

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
