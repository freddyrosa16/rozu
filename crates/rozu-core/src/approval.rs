//! Approval gate for tool calls with side effects.
//!
//! The agent auto-approves [`Risk::Read`] calls and asks the gate about everything else. The UI or
//! CLI implements the gate (M3); until then [`ReadOnlyGate`] denies anything that isn't a read.

use async_trait::async_trait;
use rozu_tools::Risk;
use serde_json::Value;

/// A tool call waiting for a decision.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub tool: String,
    pub args: Value,
    pub risk: Risk,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Approve,
    Deny { reason: String },
}

#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn decide(&self, req: ApprovalRequest) -> Decision;
}

/// Denies every request. Reads never reach a gate, so this makes the agent read-only.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadOnlyGate;

#[async_trait]
impl ApprovalGate for ReadOnlyGate {
    async fn decide(&self, req: ApprovalRequest) -> Decision {
        Decision::Deny {
            reason: format!(
                "{} needs approval ({:?}) and this session is read-only",
                req.tool, req.risk
            ),
        }
    }
}
