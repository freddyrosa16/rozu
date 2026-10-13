//! OS sandboxes for shell commands.
//!
//! On macOS commands run under Seatbelt (`/usr/bin/sandbox-exec -p <profile>`): reads are allowed
//! everywhere, writes only under the workspace, the temp directories and `/dev`, and the network
//! is denied unless the call was approved with network access. Other platforms get
//! [`NoSandbox`] for now (Linux Landlock is planned), so the approval policy treats them as
//! unsandboxed.

use std::fmt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::process::Command;

use crate::Workspace;

/// Path of the Seatbelt launcher on macOS.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// What a sandboxed command may touch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxPolicy {
    /// Directories (canonical) the command may write under.
    pub writable_roots: Vec<PathBuf>,
    /// Directories inside `writable_roots` that stay read-only (e.g. `<workspace>/.rozu`).
    pub read_only_subpaths: Vec<PathBuf>,
    /// Allow network access.
    pub network: bool,
}

impl SandboxPolicy {
    /// Writes limited to the workspace, the temp directories and `/dev`; `.rozu` read-only.
    pub fn for_workspace(workspace: &Workspace, network: bool) -> Self {
        let mut writable_roots = vec![workspace.root().to_path_buf()];
        for dir in temp_dirs() {
            if let Ok(canonical) = dir.canonicalize() {
                if !writable_roots.contains(&canonical) {
                    writable_roots.push(canonical);
                }
            }
        }
        writable_roots.push(PathBuf::from("/dev"));
        Self {
            writable_roots,
            read_only_subpaths: vec![workspace.protected_dir()],
            network,
        }
    }
}

/// `$TMPDIR` (if set) and `/tmp`, as given (not canonicalized).
pub fn temp_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(tmp) = std::env::var_os("TMPDIR").filter(|v| !v.is_empty()) {
        dirs.push(PathBuf::from(tmp));
    }
    if cfg!(unix) {
        dirs.push(PathBuf::from("/tmp"));
    }
    dirs
}

/// Runs shell command lines, possibly confined.
pub trait Sandbox: Send + Sync + fmt::Debug {
    /// Short name shown on approval prompts (`seatbelt`, `none`).
    fn name(&self) -> &'static str;

    /// Whether this sandbox actually restricts anything.
    fn enforced(&self) -> bool;

    /// A command that runs `script` with `/bin/sh -c` under `policy`.
    fn command(&self, script: &str, policy: &SandboxPolicy) -> Command;
}

/// Runs commands unconfined.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoSandbox;

impl Sandbox for NoSandbox {
    fn name(&self) -> &'static str {
        "none"
    }

    fn enforced(&self) -> bool {
        false
    }

    fn command(&self, script: &str, _policy: &SandboxPolicy) -> Command {
        let mut cmd = Command::new("/bin/sh");
        cmd.arg("-c").arg(script);
        cmd
    }
}

/// macOS Seatbelt via `sandbox-exec`. Only works on macOS; see [`platform_sandbox`].
#[derive(Debug, Clone, Copy, Default)]
pub struct Seatbelt;

impl Sandbox for Seatbelt {
    fn name(&self) -> &'static str {
        "seatbelt"
    }

    fn enforced(&self) -> bool {
        true
    }

    fn command(&self, script: &str, policy: &SandboxPolicy) -> Command {
        let mut cmd = Command::new(SANDBOX_EXEC);
        cmd.arg("-p")
            .arg(seatbelt_profile(policy))
            .arg("/bin/sh")
            .arg("-c")
            .arg(script);
        cmd
    }
}

/// The best sandbox for this OS: Seatbelt on macOS (if `sandbox-exec` exists), else none.
pub fn platform_sandbox() -> Arc<dyn Sandbox> {
    if cfg!(target_os = "macos") && Path::new(SANDBOX_EXEC).exists() {
        Arc::new(Seatbelt)
    } else {
        Arc::new(NoSandbox)
    }
}

/// The Seatbelt (SBPL) profile for `policy`.
///
/// Starts from `allow default` so ordinary tools keep working, then denies all file writes and
/// re-allows them under the writable roots. Later rules win in SBPL, so the read-only subpaths
/// come last. Paths are embedded as escaped string literals.
pub fn seatbelt_profile(policy: &SandboxPolicy) -> String {
    let mut profile = String::from(
        "(version 1)\n\
         (allow default)\n\
         (deny file-write*)\n",
    );
    if !policy.writable_roots.is_empty() {
        profile.push_str("(allow file-write*");
        for root in &policy.writable_roots {
            profile.push_str(&format!("\n    (subpath {})", sbpl_string(root)));
        }
        profile.push_str(")\n");
    }
    if !policy.read_only_subpaths.is_empty() {
        profile.push_str("(deny file-write*");
        for path in &policy.read_only_subpaths {
            profile.push_str(&format!("\n    (subpath {})", sbpl_string(path)));
        }
        profile.push_str(")\n");
    }
    if !policy.network {
        profile.push_str("(deny network*)\n");
    }
    profile
}

/// `path` as an SBPL string literal: double-quoted, with `\`, `"` and control characters
/// escaped.
fn sbpl_string(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');
    for c in raw.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => out.push_str(&format!("\\x{:02x};", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn policy(network: bool) -> SandboxPolicy {
        SandboxPolicy {
            writable_roots: vec![PathBuf::from("/work/space"), PathBuf::from("/dev")],
            read_only_subpaths: vec![PathBuf::from("/work/space/.rozu")],
            network,
        }
    }

    #[test]
    fn profile_limits_writes_and_network() {
        assert_eq!(
            seatbelt_profile(&policy(false)),
            "(version 1)\n\
             (allow default)\n\
             (deny file-write*)\n\
             (allow file-write*\n    (subpath \"/work/space\")\n    (subpath \"/dev\"))\n\
             (deny file-write*\n    (subpath \"/work/space/.rozu\"))\n\
             (deny network*)\n"
        );
    }

    #[test]
    fn approved_network_drops_the_network_rule() {
        let profile = seatbelt_profile(&policy(true));
        assert!(!profile.contains("network"), "{profile}");
        assert!(profile.contains("(deny file-write*)"));
    }

    #[test]
    fn read_only_rule_comes_after_the_allow() {
        let profile = seatbelt_profile(&policy(false));
        let allow = profile.find("(allow file-write*").unwrap();
        let deny_rozu = profile.find(".rozu").unwrap();
        assert!(deny_rozu > allow);
    }

    #[test]
    fn paths_are_escaped() {
        assert_eq!(
            sbpl_string(Path::new(r#"/tmp/we"ird\dir"#)),
            r#""/tmp/we\"ird\\dir""#
        );
        assert_eq!(sbpl_string(Path::new("/a\nb")), r#""/a\nb""#);
        // An injected rule stays inside the string literal.
        let evil = SandboxPolicy {
            writable_roots: vec![PathBuf::from("/x\")) (allow file-write* (subpath \"/")],
            read_only_subpaths: vec![],
            network: false,
        };
        let profile = seatbelt_profile(&evil);
        assert!(
            profile.contains(r#"(subpath "/x\")) (allow file-write* (subpath \"/")"#),
            "{profile}"
        );
    }

    #[test]
    fn workspace_policy_includes_tmp_and_protects_rozu_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        let policy = SandboxPolicy::for_workspace(&ws, false);
        assert_eq!(policy.writable_roots[0], ws.root());
        assert!(policy.writable_roots.contains(&PathBuf::from("/dev")));
        if cfg!(unix) {
            let tmp = Path::new("/tmp").canonicalize().unwrap();
            assert!(policy.writable_roots.contains(&tmp), "{policy:?}");
        }
        assert_eq!(policy.read_only_subpaths, [ws.root().join(".rozu")]);
        assert!(!policy.network);
    }

    #[test]
    fn no_sandbox_runs_sh_directly() {
        let cmd = NoSandbox.command("echo hi", &policy(false));
        let std = cmd.as_std();
        assert_eq!(std.get_program(), "/bin/sh");
        assert_eq!(std.get_args().collect::<Vec<_>>(), ["-c", "echo hi"]);
    }

    #[test]
    fn seatbelt_wraps_sh_with_the_profile() {
        let cmd = Seatbelt.command("echo hi", &policy(false));
        let std = cmd.as_std();
        assert_eq!(std.get_program(), SANDBOX_EXEC);
        let args: Vec<_> = std.get_args().map(|a| a.to_string_lossy()).collect();
        assert_eq!(args[0], "-p");
        assert_eq!(args[1], seatbelt_profile(&policy(false)));
        assert_eq!(args[2..], ["/bin/sh", "-c", "echo hi"]);
    }
}
