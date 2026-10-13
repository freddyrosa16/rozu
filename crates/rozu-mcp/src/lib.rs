//! rozu's stdio MCP server, for CLI-backed providers.
//!
//! `rozu mcp-serve` runs [`McpServer`] over stdin/stdout. A vendor CLI (Claude Code) starts it
//! from an `--mcp-config` file and reaches two kinds of tools through it:
//!
//! - **rozu's own tools** (`read_file`, `list_dir`, `grep`, `write_file`, `apply_patch`,
//!   `shell`; Claude sees them as `mcp__rozu__<name>`), executed by the same
//!   [`ToolExecutor`](rozu_core::ToolExecutor) as with HTTP providers: workspace checks, the
//!   Seatbelt sandbox, the [`ApprovalPolicy`](rozu_core::ApprovalPolicy) and the decision log.
//! - **`approve`**, the permission-prompt tool (`--permission-prompt-tool mcp__rozu__approve`),
//!   which decides on anything the CLI wants to run itself. See [`claude`] for how its requests
//!   are mapped to a risk class; anything unmappable is denied.
//!
//! Prompts go to the parent `rozu` process (which owns the terminal) over the Unix socket named by
//! [`APPROVAL_SOCKET_ENV`]; see [`bridge`]. If the parent can't be reached, the answer is deny.

pub mod bridge;
pub mod claude;
mod jsonrpc;
mod server;

pub use bridge::{APPROVAL_SOCKET_ENV, ApprovalBridge, SocketGate};
pub use server::{APPROVE_TOOL, McpServer, PROTOCOL_VERSION, SERVER_NAME};

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
