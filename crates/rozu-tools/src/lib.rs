//! Tools the rozu agent can call.
//!
//! Defines the `Tool` trait and registry, the built-in tools (`read_file`, `list_dir`, `grep`,
//! `write_file`, `apply_patch`, `shell`), shell command risk classification, and the
//! [`Sandbox`] that confines shell commands (Seatbelt on macOS).
//!
//! Every path the file tools touch goes through [`Workspace::resolve`] (or
//! [`Workspace::resolve_for_write`]), which rejects anything outside the workspace root,
//! including `..` and symlink escapes. The write tools also refuse `.rozu/`, where the
//! user-managed project settings and pinned memory live. Writes use directory handles to
//! prevent a concurrent directory rename or symlink swap from redirecting an operation.

mod command_risk;
mod fs;
mod patch;
mod sandbox;
mod shell;
mod tool;
mod truncate;
mod workspace;
mod workspace_file;
mod write;

pub use command_risk::{
    AllowRule, CommandRisk, PathScope, classify_command, rules_allow, split_segments, suggest_rule,
};
pub use fs::{Grep, ListDir, ReadFile};
pub use patch::{PatchError, SearchReplace, apply_search_replace, apply_unified_diff};
pub use sandbox::{
    NoSandbox, SANDBOX_BLOCKED, SANDBOX_EXEC, Sandbox, SandboxPolicy, Seatbelt, SessionTemp,
    platform_sandbox, resolve_lenient, sandbox_blocked, seatbelt_profile, too_broad_to_grant,
};
pub use shell::{DEFAULT_SHELL_TIMEOUT_SECS, MAX_SHELL_TIMEOUT_SECS, Shell};
pub use tool::{
    ExecPermissions, OutsideWrites, Risk, Tool, ToolCtx, ToolOutput, ToolRegistry, parse_args,
    schema_for,
};
pub use truncate::{MAX_OUTPUT_BYTES, truncate_head_tail};
pub use workspace::{PROTECTED_DIR, PathError, Workspace};
pub use write::{ApplyPatch, WriteFile};

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
