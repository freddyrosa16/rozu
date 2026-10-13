//! Tools the rozu agent can call.
//!
//! Defines the `Tool` trait and registry, the built-in tools (`read_file`, `list_dir`, `grep`,
//! `apply_patch` / `write_file`, `shell`), risk classification, the `ApprovalGate` used for side
//! effects, and the macOS Seatbelt sandbox that limits shell writes to the workspace.
//!
//! M1 ships the read-only file tools. Every path they touch goes through [`Workspace::resolve`],
//! which rejects anything outside the workspace root, including `..` and symlink escapes.

mod fs;
mod tool;
mod truncate;
mod workspace;

pub use fs::{Grep, ListDir, ReadFile};
pub use tool::{Risk, Tool, ToolCtx, ToolOutput, ToolRegistry, parse_args, schema_for};
pub use truncate::{MAX_OUTPUT_BYTES, truncate_head_tail};
pub use workspace::{PathError, Workspace};

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
