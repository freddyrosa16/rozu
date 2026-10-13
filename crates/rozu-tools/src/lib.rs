//! Tools the rozu agent can call.
//!
//! Defines the `Tool` trait and registry, the built-in tools (`read_file`, `list_dir`, `grep`,
//! `apply_patch` / `write_file`, `shell`), risk classification, the `ApprovalGate` used for side
//! effects, and the macOS Seatbelt sandbox that limits shell writes to the workspace.

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
