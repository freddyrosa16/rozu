//! Core of the rozu coding agent.
//!
//! Owns the agent loop (stream → tool calls → policy → approval → execute → tool results), the
//! session model, the `Command` / `Event` channels the UI and CLI talk through, and policy such as
//! turn and token caps. It has no UI and does no HTTP itself; providers and tools plug in from
//! their own crates.

pub mod telemetry;

pub use telemetry::{init_tracing, redact};

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
