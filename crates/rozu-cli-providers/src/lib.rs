//! CLI-backed providers: rozu drives the **official vendor CLI** as a subprocess and speaks its
//! machine protocol, so the user's subscription is used without rozu ever touching credentials.
//!
//! - [`process`]: a generic child process speaking JSON lines (own process group, killed on
//!   cancel/drop, stderr to the log file only).
//! - [`claude`]: Claude Code (`claude -p` with stream-json), an agent backend whose tools are
//!   rozu's own, reached over rozu's MCP server.
//!
//! The HTTP (API-key) adapters live in `rozu-providers` and are unaffected.

pub mod claude;
pub mod process;

pub use claude::{ClaudeCliConfig, ClaudeCliProvider};
pub use process::{CliCommand, CliProcess};

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
