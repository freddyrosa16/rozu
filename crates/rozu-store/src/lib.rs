//! Persistence for rozu.
//!
//! Writes one append-only JSONL file per session (messages, usage, compaction checkpoints) plus a
//! SQLite index for listing, search and resume. Also loads project memory (`ROZU.md` /
//! `AGENTS.md`) and user-pinned notes.

mod memory;
mod session;

pub use memory::{MAX_MEMORY_BYTES, MEMORY_FILES, PINNED_FILE, ProjectMemory, pin};
pub use session::{
    Record, SESSIONS_DIR_ENV, SessionMeta, SessionStore, SessionSummary, SessionWriter, StoreError,
    unix_now,
};

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
