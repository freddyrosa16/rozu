//! Interactive terminal sessions for rozu.
//!
//! Spawns the user's shell in a PTY (`portable-pty`) and keeps the screen grid with a VT parser
//! (`alacritty_terminal`), so the UI can render it and the agent can read it (and, with approval,
//! write to it).

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
