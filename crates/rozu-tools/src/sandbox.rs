//! OS sandboxes for shell commands.
//!
//! On macOS commands run under Seatbelt (`/usr/bin/sandbox-exec -p <profile>`) in every approval
//! mode: reads are allowed everywhere, writes only under the workspace, the session's private
//! temp dir ([`SessionTemp`], which commands get as `$TMPDIR`), `/dev`, and the outside paths
//! the user approved for that one call. `/tmp` and the user's `$TMPDIR` are *not* writable. The
//! network is denied unless the call was approved with network access. Other platforms get
//! [`NoSandbox`] for now (Linux Landlock is planned), so the approval policy treats them as
//! unsandboxed.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::process::Command;

use crate::Workspace;
use crate::workspace::normalize;

/// Path of the Seatbelt launcher on macOS.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Starts the note added to tool output when the sandbox (probably) refused something.
pub const SANDBOX_BLOCKED: &str = "blocked by rozu sandbox: writes outside workspace";

/// Whether tool output carries the [`SANDBOX_BLOCKED`] note.
pub fn sandbox_blocked(output: &str) -> bool {
    output.contains(SANDBOX_BLOCKED)
}

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
    /// Writes limited to the workspace, `temp_dir` (the session's), `/dev` and `extra` (the
    /// approved outside targets of one call); `.rozu` read-only.
    pub fn for_workspace(
        workspace: &Workspace,
        temp_dir: Option<&Path>,
        network: bool,
        extra: &[PathBuf],
    ) -> Self {
        let mut writable_roots = vec![workspace.root().to_path_buf()];
        let mut add = |path: PathBuf| {
            if !writable_roots.contains(&path) {
                writable_roots.push(path);
            }
        };
        if let Some(dir) = temp_dir {
            add(dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf()));
        }
        add(PathBuf::from("/dev"));
        for path in extra {
            add(path.clone());
        }
        Self {
            writable_roots,
            read_only_subpaths: vec![workspace.protected_dir()],
            network,
        }
    }
}

/// `path` with its deepest existing ancestor canonicalized (symlinks resolved, `/tmp` →
/// `/private/tmp`) and the missing rest appended. Seatbelt matches real paths, so this is the
/// form an approved outside target is granted in.
pub fn resolve_lenient(path: &Path) -> PathBuf {
    let normalized = normalize(path);
    let mut existing = normalized.as_path();
    let mut missing = Vec::new();
    loop {
        if let Ok(canonical) = existing.canonicalize() {
            return missing
                .iter()
                .rev()
                .fold(canonical, |acc: PathBuf, name| acc.join(name));
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => return normalized,
        }
    }
}

/// Whether granting writes under `path` would open up too much to call it scoped: `/`, a
/// top-level directory (`/etc`, `/Users`), or the home directory itself or one of its parents.
pub fn too_broad_to_grant(path: &Path, home: Option<&Path>) -> bool {
    path.components().count() <= 2 || home.is_some_and(|home| home.starts_with(path))
}

/// A private (0700) temp directory for one rozu session, removed when dropped. Shell commands
/// get it as `$TMPDIR`; it is the only temp location the sandbox lets them write.
#[derive(Debug)]
pub struct SessionTemp {
    path: PathBuf,
}

impl SessionTemp {
    /// Creates `rozu-<label>-<random>` under the system temp dir.
    pub fn create(label: &str) -> io::Result<Self> {
        let base = std::env::temp_dir();
        let label: String = label
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
            .take(40)
            .collect();
        let mut last_err = None;
        for _ in 0..16 {
            let dir = base.join(format!("rozu-{label}-{:08x}", random_u32()));
            let mut builder = std::fs::DirBuilder::new();
            #[cfg(unix)]
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
            match builder.create(&dir) {
                Ok(()) => {
                    let path = dir.canonicalize()?;
                    return Ok(Self { path });
                }
                Err(err) if err.kind() == io::ErrorKind::AlreadyExists => last_err = Some(err),
                Err(err) => return Err(err),
            }
        }
        Err(last_err.unwrap_or_else(|| io::Error::other("could not create a session temp dir")))
    }

    /// The canonical path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SessionTemp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

/// A random-enough number for unique names (std's per-process random hash keys, plus time).
fn random_u32() -> u32 {
    use std::hash::{BuildHasher, Hasher};
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
    );
    hasher.write_u32(std::process::id());
    hasher.finish() as u32
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
    fn workspace_policy_has_only_the_session_tmp_and_protects_rozu_dir() {
        let dir = tempfile::tempdir().unwrap();
        let ws = Workspace::new(dir.path()).unwrap();
        let session = SessionTemp::create("test").unwrap();
        let extra = PathBuf::from("/Users/me/scratch/approved.txt");
        let policy = SandboxPolicy::for_workspace(
            &ws,
            Some(session.path()),
            false,
            std::slice::from_ref(&extra),
        );
        assert_eq!(
            policy.writable_roots,
            [
                ws.root().to_path_buf(),
                session.path().to_path_buf(),
                PathBuf::from("/dev"),
                extra
            ]
        );
        // Neither /tmp nor the user's $TMPDIR as a whole.
        if cfg!(unix) {
            let tmp = Path::new("/tmp").canonicalize().unwrap();
            assert!(!policy.writable_roots.contains(&tmp), "{policy:?}");
        }
        let user_tmp = std::env::temp_dir().canonicalize().unwrap();
        assert!(!policy.writable_roots.contains(&user_tmp), "{policy:?}");
        assert_eq!(policy.read_only_subpaths, [ws.root().join(".rozu")]);
        assert!(!policy.network);
    }

    #[test]
    fn session_temp_is_private_and_removed_on_drop() {
        let session = SessionTemp::create("abc/../x").unwrap();
        let path = session.path().to_path_buf();
        assert!(path.is_dir());
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        assert!(name.starts_with("rozu-abcx-"), "{name}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        std::fs::write(path.join("f"), "x").unwrap();
        drop(session);
        assert!(!path.exists());
    }

    #[test]
    fn lenient_resolution_and_breadth() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().canonicalize().unwrap();
        assert_eq!(
            resolve_lenient(&dir.path().join("a/../missing/f.txt")),
            real.join("missing/f.txt")
        );
        let home = Path::new("/Users/me");
        for (path, broad) in [
            ("/", true),
            ("/etc", true),
            ("/Users/me", true),
            ("/Users/me/scratch", false),
            ("/etc/hosts", false),
        ] {
            assert_eq!(
                too_broad_to_grant(Path::new(path), Some(home)),
                broad,
                "{path}"
            );
        }
    }

    #[test]
    fn blocked_marker() {
        assert!(sandbox_blocked(&format!(
            "exit code: 1\n[{SANDBOX_BLOCKED} …]"
        )));
        assert!(!sandbox_blocked("Operation not permitted"));
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
