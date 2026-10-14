//! Everything `rozu chat` and the desktop app share to run a chat: the [`AgentFactory`] that
//! builds an [`rozu_core::Agent`] for a model choice (an API-key provider, or a vendor CLI on
//! the user's subscription), the default provider when none is named, the system prompt, and
//! the project memory that follows it. A feature added here reaches both front ends.

mod approval;
mod context;
mod factory;
mod models;

pub use approval::{ApprovalOptions, unsandboxed_action_label, unsandboxed_approval_label};
pub use context::{APP_HOST, TERMINAL_HOST, load_memory, set_system, system_prompt};
pub use factory::{AgentFactory, GROK_HOOK_MAX_BYTES_ENV};
pub use models::{any_api_key, base_url_override, cli_installed, default_provider, resolve_choice};

use rozu_core::ApprovalMode;

/// Version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The `--approval` name (`rozu chat`, `rozu mcp-serve`) of a mode.
pub fn approval_arg(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::ReadOnly => "read-only",
        ApprovalMode::AutoWorkspace => "auto",
        ApprovalMode::FullAccess => "full",
    }
}

/// The mode an `--approval` name stands for.
pub fn approval_mode(name: &str) -> Option<ApprovalMode> {
    [
        ApprovalMode::ReadOnly,
        ApprovalMode::AutoWorkspace,
        ApprovalMode::FullAccess,
    ]
    .into_iter()
    .find(|mode| approval_arg(*mode) == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn approval_modes_round_trip_through_their_cli_names() {
        for mode in [
            ApprovalMode::ReadOnly,
            ApprovalMode::AutoWorkspace,
            ApprovalMode::FullAccess,
        ] {
            assert_eq!(approval_mode(approval_arg(mode)), Some(mode));
        }
        assert_eq!(approval_mode("yolo"), None);
    }
}
