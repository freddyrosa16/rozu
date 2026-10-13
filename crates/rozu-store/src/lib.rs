//! Persistence for rozu.
//!
//! Writes one append-only JSONL file per session (messages, tool calls, approvals, usage) plus a
//! SQLite index for listing, search and resume. Also loads project memory (`ROZU.md` /
//! AGENTS.md) and user-pinned notes.

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
