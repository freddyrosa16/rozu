//! Tools the rozu agent can call.
//!
//! Defines the `Tool` trait and registry, the built-in tools (`read_file`, `list_dir`, `grep`,
//! `write_file`, `apply_patch`, `shell`), shell command risk classification, and the
//! [`Sandbox`] that confines shell commands (Seatbelt on macOS).
//!
//! Every path the file tools touch goes through [`Workspace::resolve`] (or
//! [`Workspace::resolve_for_write`]), which rejects anything outside the workspace root,
//! including `..` and symlink escapes. The write tools also refuse `.rozu/`, where the
//! per-project approval allowlist lives.

mod command_risk;
mod fs;
mod patch;
mod sandbox;
mod shell;
mod tool;
mod truncate;
mod workspace;
mod write;

pub use command_risk::{
    CommandRisk, PathScope, classify_command, prefixes_allow, split_segments, suggest_prefix,
};
pub use fs::{Grep, ListDir, ReadFile};
pub use patch::{PatchError, SearchReplace, apply_search_replace, apply_unified_diff};
pub use sandbox::{
    NoSandbox, SANDBOX_EXEC, Sandbox, SandboxPolicy, Seatbelt, platform_sandbox, seatbelt_profile,
    temp_dirs,
};
pub use shell::{DEFAULT_SHELL_TIMEOUT_SECS, MAX_SHELL_TIMEOUT_SECS, Shell};
pub use tool::{
    ExecPermissions, Risk, Tool, ToolCtx, ToolOutput, ToolRegistry, parse_args, schema_for,
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
