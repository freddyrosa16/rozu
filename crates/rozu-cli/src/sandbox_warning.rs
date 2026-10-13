//! The warning printed when this OS has no sandbox for shell commands (everything but macOS for
//! now): at the start of `rozu chat` and in `rozu doctor`, on stderr, bold red on a terminal.

use std::io::{self, IsTerminal};

use rozu_tools::platform_sandbox;

/// The banner, with ANSI bold red when `color`.
pub fn no_sandbox_warning(color: bool) -> String {
    let rule = "!".repeat(76);
    let lines = [
        rule.as_str(),
        "!! WARNING: NO OS SANDBOX ON THIS SYSTEM",
        &format!(
            "!! rozu can only sandbox shell commands on macOS (Seatbelt); this is {}.",
            std::env::consts::OS
        ),
        "!! Commands the agent runs here are NOT confined: they can read and write",
        "!! anything your user can (outside the workspace too) and use the network.",
        "!! --approval auto asks before every shell command that writes; with",
        "!! --approval full they run WITHOUT asking. Read every command you approve.",
        rule.as_str(),
    ];
    let text = lines.join("\n");
    if color {
        format!("\x1b[1;31m{text}\x1b[0m")
    } else {
        text
    }
}

/// Prints [`no_sandbox_warning`] to stderr if shell commands can't be sandboxed here. Returns
/// whether it did.
pub fn warn_if_unsandboxed() -> bool {
    if platform_sandbox().enforced() {
        return false;
    }
    let color = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
    eprintln!("{}", no_sandbox_warning(color));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_banner_is_loud_and_says_what_is_at_stake() {
        let plain = no_sandbox_warning(false);
        assert!(plain.lines().count() >= 5, "{plain}");
        assert!(plain.contains("WARNING: NO OS SANDBOX"));
        assert!(plain.contains("NOT confined"));
        assert!(plain.contains("--approval full"));
        assert!(!plain.contains('\x1b'));
        let red = no_sandbox_warning(true);
        assert!(red.starts_with("\x1b[1;31m") && red.ends_with("\x1b[0m"));
    }

    #[test]
    fn only_unsandboxed_systems_warn() {
        assert_eq!(warn_if_unsandboxed(), !platform_sandbox().enforced());
    }
}
