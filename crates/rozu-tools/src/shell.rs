//! The one-shot `shell` tool: runs a command line with `/bin/sh -c` in its own process group,
//! under the sandbox the approval policy picked, and returns the exit code with stdout/stderr
//! (head + tail truncated).
//!
//! On timeout or cancel, and when the command finishes, the whole process group is killed, so
//! background children can't outlive the call. Dropping the call's future (e.g. when the agent
//! gives up on it) does the same.

use std::collections::VecDeque;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::command_risk::{
    AllowRule, CommandRisk, PathScope, classify_command, rules_allow, suggest_rule,
};
use crate::sandbox::{NoSandbox, SANDBOX_BLOCKED, Sandbox, SandboxPolicy};
use crate::tool::{OutsideWrites, parse_args, schema_for};
use crate::{MAX_OUTPUT_BYTES, Risk, Tool, ToolCtx, ToolOutput};

pub const DEFAULT_SHELL_TIMEOUT_SECS: u64 = 120;
pub const MAX_SHELL_TIMEOUT_SECS: u64 = 600;
/// How long to wait for output pipes to close after the process group is killed.
const DRAIN_GRACE: Duration = Duration::from_secs(2);

/// Runs a shell command in the workspace.
#[derive(Debug, Clone, Copy, Default)]
pub struct Shell;

#[derive(Debug, Deserialize, JsonSchema)]
struct ShellArgs {
    /// Command line, run with `/bin/sh -c`.
    command: String,
    /// Working directory, relative to the workspace root. Defaults to the root.
    #[serde(default)]
    cwd: Option<String>,
    /// Seconds before the command and all its child processes are killed (default 120, max
    /// 600).
    #[serde(default)]
    timeout_secs: Option<u64>,
    /// Set to true when the command needs network access (downloads, package installs, git
    /// fetch/push). The network is blocked otherwise.
    #[serde(default)]
    network: bool,
}

impl ShellArgs {
    fn timeout(&self) -> Duration {
        Duration::from_secs(
            self.timeout_secs
                .unwrap_or(DEFAULT_SHELL_TIMEOUT_SECS)
                .clamp(1, MAX_SHELL_TIMEOUT_SECS),
        )
    }

    fn cwd(&self, ctx: &ToolCtx) -> Result<PathBuf, ToolOutput> {
        let cwd = ctx
            .workspace
            .resolve(self.cwd.as_deref().unwrap_or("."))
            .map_err(|err| ToolOutput::error(format!("cwd: {err}")))?;
        if !cwd.is_dir() {
            return Err(ToolOutput::error(format!(
                "cwd {} is not a directory",
                self.cwd.as_deref().unwrap_or(".")
            )));
        }
        Ok(cwd)
    }

    fn classify(&self, ctx: &ToolCtx) -> CommandRisk {
        let cwd = self
            .cwd(ctx)
            .unwrap_or_else(|_| ctx.workspace.root().to_path_buf());
        // Inside: the workspace and the session temp dir, which the command gets as $TMPDIR.
        let mut inside = vec![ctx.workspace.root().to_path_buf()];
        let mut scope_vars = Vec::new();
        if let Some(tmp) = &ctx.temp_dir {
            inside.push(tmp.clone());
            scope_vars.push(("TMPDIR", tmp.clone()));
        }
        if let Some(home) = home_dir() {
            scope_vars.push(("HOME", home));
        }
        let mut scope = PathScope::new(cwd, inside);
        for (name, value) in scope_vars {
            scope = scope.with_var(name, value);
        }
        let mut risk = classify_command(&self.command, &scope);
        if self.network {
            risk.network = true;
            risk.risk = risk.risk.max(Risk::Network);
        }
        risk
    }
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
}

/// Appended to the output of a sandboxed command that failed with `EPERM`.
fn blocked_note(temp_dir: Option<&std::path::Path>) -> String {
    format!(
        "[{SANDBOX_BLOCKED} are not permitted. Writable: the workspace{} and paths the user \
         approved for this call; the network is off unless approved. If it must write elsewhere, \
         say so: the user can run it unsandboxed, once, from the approval prompt.]",
        if temp_dir.is_some() {
            " and the session temp dir ($TMPDIR)"
        } else {
            ""
        }
    )
}

#[async_trait]
impl Tool for Shell {
    fn name(&self) -> &'static str {
        "shell"
    }

    fn description(&self) -> &'static str {
        "Run a shell command (`/bin/sh -c`) in the workspace and return its exit code, stdout and \
         stderr. One-shot and non-interactive: no stdin, killed with its children after \
         timeout_secs. It runs in a sandbox: writes are limited to the workspace and $TMPDIR (a \
         private temp dir), and the network is off unless you set network: true (which needs \
         the user's approval). Prefer read_file/list_dir/grep for looking at files, and \
         write_file/apply_patch over redirects or interpreter one-liners for editing them."
    }

    fn schema(&self) -> Value {
        schema_for::<ShellArgs>()
    }

    fn risk(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        match parse_args::<ShellArgs>(args.clone()) {
            Ok(args) => args.classify(ctx).risk,
            Err(_) => Risk::WriteWorkspace,
        }
    }

    fn validate(&self, args: &Value, ctx: &ToolCtx) -> Result<(), ToolOutput> {
        let args: ShellArgs = parse_args(args.clone())?;
        if args.command.trim().is_empty() {
            return Err(ToolOutput::error("command is empty"));
        }
        args.cwd(ctx).map(|_| ())
    }

    fn preview(&self, args: &Value) -> String {
        let command = args.get("command").and_then(Value::as_str).unwrap_or("");
        let mut out = format!("$ {command}");
        if let Some(cwd) = args.get("cwd").and_then(Value::as_str) {
            let _ = write!(out, "\n(in {cwd})");
        }
        out
    }

    fn sandboxed(&self) -> bool {
        true
    }

    fn wants_network(&self, args: &Value, ctx: &ToolCtx) -> bool {
        parse_args::<ShellArgs>(args.clone()).is_ok_and(|args| args.classify(ctx).network)
    }

    fn timeout(&self, args: &Value) -> Option<Duration> {
        let args: ShellArgs = parse_args(args.clone()).ok()?;
        // A little slack so the tool's own timeout (which reports partial output) fires first.
        Some(args.timeout() + DRAIN_GRACE + Duration::from_secs(3))
    }

    fn outside_writes(&self, args: &Value, ctx: &ToolCtx) -> OutsideWrites {
        match parse_args::<ShellArgs>(args.clone()) {
            Ok(args) => {
                let risk = args.classify(ctx);
                OutsideWrites {
                    paths: risk.outside_writes,
                    unknown: risk.outside_unknown,
                }
            }
            Err(_) => OutsideWrites::default(),
        }
    }

    fn allow_rule(&self, args: &Value) -> Option<AllowRule> {
        suggest_rule(args.get("command")?.as_str()?)
    }

    fn rules_allow(&self, args: &Value, rules: &[AllowRule]) -> bool {
        // An explicit network request isn't covered by a rule for the bare command.
        args.get("network").and_then(Value::as_bool) != Some(true)
            && args
                .get("command")
                .and_then(Value::as_str)
                .is_some_and(|command| rules_allow(command, rules))
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput {
        let args: ShellArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let cwd = match args.cwd(ctx) {
            Ok(cwd) => cwd,
            Err(output) => return output,
        };
        let policy = SandboxPolicy::for_workspace(
            &ctx.workspace,
            ctx.temp_dir.as_deref(),
            ctx.permissions.network,
            &ctx.permissions.extra_writable,
        );
        let sandboxed = ctx.permissions.sandbox && ctx.sandbox.enforced();
        let sandbox: &dyn Sandbox = if ctx.permissions.sandbox {
            &*ctx.sandbox
        } else {
            &NoSandbox
        };
        let mut cmd = sandbox.command(&args.command, &policy);
        if let Some(tmp) = &ctx.temp_dir {
            cmd.env("TMPDIR", tmp).env("TMP", tmp).env("TEMP", tmp);
        }
        cmd.current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(unix)]
        cmd.process_group(0);

        let timeout = args.timeout();
        tracing::debug!(command = %args.command, sandbox = sandbox.name(),
            network = ctx.permissions.network, "running shell command");
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(err) => return ToolOutput::error(format!("could not start the command: {err}")),
        };
        let group = ProcessGroup::new(child.id());

        let stdout = Capture::shared();
        let stderr = Capture::shared();
        let readers = [
            spawn_reader(child.stdout.take(), stdout.clone()),
            spawn_reader(child.stderr.take(), stderr.clone()),
        ];

        let ending = tokio::select! {
            biased;
            _ = ctx.cancel.cancelled() => Ending::Cancelled,
            _ = tokio::time::sleep(timeout) => Ending::TimedOut,
            status = child.wait() => match status {
                Ok(status) => Ending::Exited(status),
                Err(err) => Ending::WaitFailed(err.to_string()),
            },
        };
        // Whatever happened, nothing from this call may keep running.
        group.kill();
        let _ = child.start_kill();
        let _ = child.wait().await;
        for reader in readers {
            if tokio::time::timeout(DRAIN_GRACE, reader).await.is_err() {
                tracing::warn!("shell output pipe stayed open after the process group was killed");
            }
        }

        let stdout = stdout.lock().map(|c| c.finish()).unwrap_or_default();
        let stderr = stderr.lock().map(|c| c.finish()).unwrap_or_default();
        let (header, is_error) = match ending {
            Ending::Exited(status) => (describe_status(status), !status.success()),
            Ending::TimedOut => (
                format!(
                    "timed out after {}s; the command and its children were killed",
                    timeout.as_secs()
                ),
                true,
            ),
            Ending::Cancelled => ("cancelled by the user".to_owned(), true),
            Ending::WaitFailed(err) => (format!("could not wait for the command: {err}"), true),
        };
        let mut out = header;
        if !stdout.is_empty() {
            let _ = write!(out, "\nstdout:\n{stdout}");
        }
        if !stderr.is_empty() {
            let _ = write!(out, "\nstderr:\n{stderr}");
        }
        if stdout.is_empty() && stderr.is_empty() {
            out.push_str("\n(no output)");
        }
        // Seatbelt denials surface as EPERM (even when a later command makes the exit status
        // 0); say plainly who refused.
        if sandboxed
            && (stderr.contains("Operation not permitted")
                || stdout.contains("Operation not permitted"))
        {
            let _ = write!(out, "\n{}", blocked_note(ctx.temp_dir.as_deref()));
        }
        ToolOutput {
            content: out,
            is_error,
        }
    }
}

enum Ending {
    Exited(ExitStatus),
    TimedOut,
    Cancelled,
    WaitFailed(String),
}

fn describe_status(status: ExitStatus) -> String {
    if let Some(code) = status.code() {
        return format!("exit code: {code}");
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return format!("killed by signal {signal}");
        }
    }
    "exited without a status code".to_owned()
}

/// The child's process group (its pid, since it was started with `process_group(0)`). Killed
/// on [`ProcessGroup::kill`] and on drop.
struct ProcessGroup {
    pgid: Option<u32>,
}

impl ProcessGroup {
    fn new(pid: Option<u32>) -> Self {
        Self { pgid: pid }
    }

    #[cfg(unix)]
    fn kill(&self) {
        if let Some(pgid) = self.pgid.and_then(|p| libc::pid_t::try_from(p).ok()) {
            // SAFETY: killpg only sends a signal; an already-gone group returns ESRCH.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
    }

    /// Without process groups only the direct child is killed (via `kill_on_drop`).
    #[cfg(not(unix))]
    fn kill(&self) {}
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        self.kill();
    }
}

/// Keeps the first and last `MAX_OUTPUT_BYTES / 2` bytes of a stream, counting what's dropped.
#[derive(Debug, Default)]
struct Capture {
    head: Vec<u8>,
    tail: VecDeque<u8>,
    omitted: usize,
}

impl Capture {
    const HALF: usize = MAX_OUTPUT_BYTES / 2;

    fn shared() -> Arc<Mutex<Self>> {
        Arc::default()
    }

    fn push(&mut self, mut bytes: &[u8]) {
        if self.head.len() < Self::HALF {
            let take = bytes.len().min(Self::HALF - self.head.len());
            self.head.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
        }
        self.tail.extend(bytes);
        let overflow = self.tail.len().saturating_sub(Self::HALF);
        if overflow > 0 {
            self.tail.drain(..overflow);
            self.omitted += overflow;
        }
    }

    fn finish(&self) -> String {
        let mut out = String::from_utf8_lossy(&self.head).into_owned();
        if self.omitted > 0 {
            let _ = write!(out, "\n[… {} bytes truncated …]\n", self.omitted);
        }
        let tail: Vec<u8> = self.tail.iter().copied().collect();
        out.push_str(&String::from_utf8_lossy(&tail));
        out
    }
}

fn spawn_reader<R>(pipe: Option<R>, capture: Arc<Mutex<Capture>>) -> tokio::task::JoinHandle<()>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let Some(mut pipe) = pipe else { return };
        let mut buf = vec![0u8; 16 * 1024];
        loop {
            match pipe.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if let Ok(mut capture) = capture.lock() {
                        capture.push(&buf[..n]);
                    }
                }
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::Workspace;

    fn setup() -> (tempfile::TempDir, ToolCtx) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        let ctx = ToolCtx::new(
            Workspace::new(dir.path()).unwrap(),
            CancellationToken::new(),
        );
        (dir, ctx)
    }

    #[test]
    fn capture_keeps_head_and_tail() {
        let mut capture = Capture::default();
        capture.push(&vec![b'a'; Capture::HALF]);
        capture.push(&vec![b'm'; 1000]);
        capture.push(&vec![b'z'; Capture::HALF]);
        let out = capture.finish();
        assert!(out.starts_with('a') && out.ends_with('z'));
        assert!(out.contains("[… 1000 bytes truncated …]"), "{}", out.len());
        assert!(!out.contains('m'));
    }

    #[tokio::test]
    async fn runs_commands_and_reports_exit_codes() {
        let (_dir, ctx) = setup();
        let out = Shell
            .call(json!({"command": "echo out; echo err >&2"}), &ctx)
            .await;
        assert!(!out.is_error, "{out:?}");
        assert_eq!(out.content, "exit code: 0\nstdout:\nout\n\nstderr:\nerr\n");

        let out = Shell.call(json!({"command": "exit 3"}), &ctx).await;
        assert!(out.is_error);
        assert_eq!(out.content, "exit code: 3\n(no output)");

        let out = Shell
            .call(json!({"command": "pwd", "cwd": "sub"}), &ctx)
            .await;
        assert!(out.content.trim_end().ends_with("/sub"), "{out:?}");
    }

    #[tokio::test]
    async fn validates_arguments() {
        let (_dir, ctx) = setup();
        for bad in [
            json!({"command": "  "}),
            json!({"command": "ls", "cwd": ".."}),
            json!({"command": "ls", "cwd": "nope"}),
        ] {
            assert!(Shell.validate(&bad, &ctx).is_err(), "{bad}");
        }
        assert!(Shell.validate(&json!({"command": "ls"}), &ctx).is_ok());
    }

    #[test]
    fn risk_and_network_follow_the_classifier() {
        let (_dir, ctx) = setup();
        assert_eq!(Shell.risk(&json!({"command": "ls"}), &ctx), Risk::Read);
        assert_eq!(
            Shell.risk(&json!({"command": "rm -rf x"}), &ctx),
            Risk::Destructive
        );
        assert_eq!(
            Shell.risk(&json!({"command": "cargo build", "network": true}), &ctx),
            Risk::Network
        );
        assert!(Shell.wants_network(&json!({"command": "curl x"}), &ctx));
        assert!(!Shell.wants_network(&json!({"command": "ls"}), &ctx));
    }

    #[test]
    fn only_the_session_temp_dir_is_inside() {
        let (_dir, ctx) = setup();
        let session = crate::SessionTemp::create("test").unwrap();
        let ctx = ctx.with_temp_dir(session.path());
        let risk = |command: &str| Shell.risk(&json!({ "command": command }), &ctx);
        // A redirect is opaque even into the session temp dir.
        let inside = session.path().join("x.txt");
        assert_eq!(
            risk(&format!("touch {}", inside.display())),
            Risk::WriteWorkspace
        );
        assert_eq!(risk("touch $TMPDIR/x.txt"), Risk::WriteWorkspace);
        assert_eq!(risk("echo hi > \"$TMPDIR/x.txt\""), Risk::Opaque);
        let args = json!({"command": "touch /tmp/rozu-elsewhere.txt"});
        assert_eq!(Shell.risk(&args, &ctx), Risk::WriteOutside);
        assert_eq!(
            Shell.outside_writes(&args, &ctx).paths,
            [PathBuf::from("/tmp/rozu-elsewhere.txt")]
        );
    }

    #[tokio::test]
    async fn commands_get_the_session_temp_dir_as_tmpdir() {
        let (_dir, ctx) = setup();
        let session = crate::SessionTemp::create("test").unwrap();
        let ctx = ctx.with_temp_dir(session.path());
        let out = Shell
            .call(json!({"command": "echo \"$TMPDIR\""}), &ctx)
            .await;
        assert_eq!(
            out.content,
            format!("exit code: 0\nstdout:\n{}\n", session.path().display())
        );
    }

    #[test]
    fn rules() {
        let rules = vec![AllowRule::prefix("cargo test")];
        assert!(Shell.rules_allow(&json!({"command": "cargo test -q"}), &rules));
        assert!(!Shell.rules_allow(&json!({"command": "cargo test", "network": true}), &rules));
        assert_eq!(
            Shell.allow_rule(&json!({"command": "cargo test -q"})),
            Some(AllowRule::prefix("cargo test"))
        );
    }

    #[tokio::test]
    async fn output_is_bounded() {
        let (_dir, ctx) = setup();
        let out = Shell
            .call(
                json!({"command": "i=0; while [ $i -lt 5000 ]; do echo line-$i-xxxxxxxxxxxxxxxx; i=$((i+1)); done"}),
                &ctx,
            )
            .await;
        assert!(!out.is_error);
        assert!(
            out.content.len() < MAX_OUTPUT_BYTES + 200,
            "{}",
            out.content.len()
        );
        assert!(out.content.contains("line-0-"));
        assert!(out.content.contains("line-4999-"));
        assert!(out.content.contains("bytes truncated"));
    }

    #[tokio::test]
    async fn cancel_stops_the_command() {
        let (_dir, ctx) = setup();
        let cancel = ctx.cancel.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            cancel.cancel();
        });
        let started = std::time::Instant::now();
        let out = Shell.call(json!({"command": "sleep 30"}), &ctx).await;
        assert!(out.is_error && out.content.contains("cancelled"), "{out:?}");
        assert!(started.elapsed() < Duration::from_secs(10));
    }
}
