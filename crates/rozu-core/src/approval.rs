//! Approval policy for tool calls.
//!
//! Every call gets a decision before it runs. The [`ApprovalPolicy`] auto-approves what the
//! [`ApprovalMode`] allows (or what a per-project "always allow" rule covers) and sends
//! everything else to an [`ApprovalGate`], which the UI or CLI implements:
//!
//! | risk            | read-only | auto (default)             | full access |
//! |-----------------|-----------|----------------------------|-------------|
//! | read            | auto      | auto                       | auto        |
//! | write workspace | prompt    | auto if sandboxed, else rule/prompt | auto |
//! | write outside   | prompt    | rule or prompt             | auto        |
//! | network         | prompt    | prompt                     | auto        |
//! | opaque          | prompt    | prompt                     | prompt      |
//! | destructive     | prompt    | prompt                     | prompt      |
//!
//! On macOS the sandbox is the boundary: shell commands run under Seatbelt in every
//! mode (writes: workspace, session temp dir, and the outside paths an approved call names).
//! Only the user's explicit [`Decision::ApproveUnsandboxed`] on a prompt runs one command
//! without it; that is never automatic and never remembered. Linux has no OS sandbox yet.
//!
//! Destructive calls always prompt and can never be covered by a rule; neither can inline
//! interpreter code or output redirects. User approvals live outside the workspace; repository
//! files never grant permission. Network requests cannot use remembered prefix rules.

use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use rozu_tools::{
    AllowRule, ExecPermissions, OutsideWrites, Risk, Sandbox, Tool, resolve_lenient,
    too_broad_to_grant,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// How much the agent may do without asking.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum ApprovalMode {
    /// Only reads run without asking.
    ReadOnly,
    /// Reads and (sandboxed) workspace writes run without asking; network access and writes
    /// outside the workspace ask, unless an "always allow" rule covers them.
    #[default]
    AutoWorkspace,
    /// Everything runs without asking, still sandboxed, except destructive and opaque calls.
    FullAccess,
}

impl fmt::Display for ApprovalMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            ApprovalMode::ReadOnly => "read-only",
            ApprovalMode::AutoWorkspace => "auto",
            ApprovalMode::FullAccess => "full access",
        })
    }
}

/// How an approved call is confined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxMode {
    /// A subprocess under the named OS sandbox: writes only in the workspace, the session temp
    /// dir and the call's approved outside paths.
    Enforced(&'static str),
    /// A subprocess with no OS sandbox (none on this platform).
    Off,
    /// Runs inside rozu; the tool itself only touches workspace paths.
    InProcess,
}

/// [`SandboxMode::Enforced`] name for commands the Codex CLI runs under its own sandbox
/// (workspace-write, network off). rozu decides whether they run; it can't lift that sandbox,
/// so "unsandboxed" there means a one-time approval only.
pub const CODEX_SANDBOX: &str = "codex";

impl fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxMode::Enforced(CODEX_SANDBOX) => f.write_str("codex's own sandbox"),
            SandboxMode::Enforced(name) => write!(f, "{name} (writes: workspace + session tmp)"),
            SandboxMode::Off => f.write_str("off (no OS sandbox here)"),
            SandboxMode::InProcess => f.write_str("n/a (in-process, workspace paths only)"),
        }
    }
}

/// A tool call waiting for a decision.
#[derive(Debug, Clone, PartialEq)]
pub struct ApprovalRequest {
    pub call_id: String,
    pub tool: String,
    pub args: Value,
    pub risk: Risk,
    /// Exactly what will run: the command line, the diff, the file contents.
    pub preview: String,
    /// How the call will be confined if approved.
    pub sandbox: SandboxMode,
    /// Whether the call will have network access if approved.
    pub network: bool,
    /// Outside paths (canonical) the sandbox will also let this one call write, if approved.
    pub extra_writes: Vec<PathBuf>,
    /// The call may write outside the workspace somewhere rozu can't name, so under the
    /// sandbox those writes will fail (the prompt says so and offers the unsandboxed escape).
    pub outside_unknown: bool,
    /// The call already ran sandboxed and the sandbox blocked it; the question is whether to
    /// rerun it unsandboxed.
    pub blocked_retry: bool,
    /// What "always allow" would remember; `None` when it isn't offered (always for
    /// destructive calls, inline code and redirects).
    pub allow_rule: Option<AllowRule>,
}

impl ApprovalRequest {
    /// Whether the prompt may offer [`Decision::ApproveUnsandboxed`]: only for calls that would
    /// otherwise run under an OS sandbox.
    pub fn can_run_unsandboxed(&self) -> bool {
        matches!(self.sandbox, SandboxMode::Enforced(_))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Approve,
    /// The reason is sent back to the model as the tool error.
    Deny {
        reason: String,
    },
    /// Approve, and auto-approve calls covered by [`ApprovalRequest::allow_rule`] in this
    /// project from now on.
    AlwaysAllowPrefix,
    /// Approve this one call to run without the OS sandbox. Only the user can choose it (never
    /// a mode or a rule), and it is never remembered.
    ApproveUnsandboxed,
}

impl Decision {
    pub fn is_approval(&self) -> bool {
        !matches!(self, Decision::Deny { .. })
    }
}

/// Who made a decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DecisionSource {
    /// The approval mode allows it.
    Mode(ApprovalMode),
    /// A stored "always allow" rule covers it.
    Rule { prefix: String },
    /// The [`ApprovalGate`] (the user, or a non-interactive default).
    Gate,
    /// The turn was cancelled while waiting for the gate.
    Cancelled,
    /// Denied by rule without asking: a request rozu can't map to a tool and risk class
    /// (default-deny), or a write to `.rozu/`.
    Policy,
    /// Passed on to rozu's own MCP server (`rozu mcp-serve`), which applies the policy,
    /// sandbox and audit log itself when the call runs: a CLI's approval prompt for one of
    /// rozu's own tools.
    Delegated,
}

impl fmt::Display for DecisionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecisionSource::Mode(mode) => write!(f, "{mode} mode"),
            DecisionSource::Rule { prefix } => write!(f, "rule `{prefix}`"),
            DecisionSource::Gate => f.write_str("user"),
            DecisionSource::Cancelled => f.write_str("cancelled"),
            DecisionSource::Policy => f.write_str("rozu policy (default-deny)"),
            DecisionSource::Delegated => f.write_str("delegated to rozu mcp-serve"),
        }
    }
}

/// The decision log entry for one tool call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApprovalRecord {
    pub call_id: String,
    pub tool: String,
    pub risk: Risk,
    pub preview: String,
    pub sandbox: SandboxMode,
    pub network: bool,
    pub extra_writes: Vec<PathBuf>,
    /// A decision on rerunning a sandbox-blocked call unsandboxed.
    pub blocked_retry: bool,
    pub decision: Decision,
    pub source: DecisionSource,
}

impl ApprovalRecord {
    /// Whether the call runs (ran) under the OS sandbox.
    pub fn sandboxed(&self) -> bool {
        matches!(self.sandbox, SandboxMode::Enforced(_))
            && self.decision != Decision::ApproveUnsandboxed
    }
}

/// Longest [`PendingApproval::summary`].
const MAX_SUMMARY_CHARS: usize = 200;

/// An approval prompt that is open and waiting for the user. Sent the moment the user is asked
/// (never for decisions a mode, rule or policy makes on its own), and followed by one
/// [`ApprovalResolution`] for the same `request_id` unless the turn is torn down first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingApproval {
    /// The tool call the prompt is about ([`ApprovalRequest::call_id`]).
    pub request_id: String,
    pub tool: String,
    /// One line saying what would run: the first line of the preview, clipped.
    pub summary: String,
    /// Who asked: a CLI-backed provider (`claude-cli`, `codex`, `grok`) or the model provider
    /// whose agent loop rozu runs (`openai`, …).
    pub provider: String,
}

impl PendingApproval {
    pub fn new(request: &ApprovalRequest, provider: impl Into<String>) -> Self {
        let first = request.preview.lines().next().unwrap_or("").trim();
        let mut summary: String = first.chars().take(MAX_SUMMARY_CHARS).collect();
        if first.chars().count() > MAX_SUMMARY_CHARS {
            summary.push('…');
        }
        if request.blocked_retry {
            summary = format!("rerun unsandboxed: {summary}");
        }
        Self {
            request_id: request.call_id.clone(),
            tool: request.tool.clone(),
            summary,
            provider: provider.into(),
        }
    }
}

/// How an open approval prompt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApprovalResolution {
    Approved,
    Denied,
    /// Nobody answered: the turn was cancelled, or the asker went away, while it was open.
    Expired,
}

impl ApprovalResolution {
    /// The resolution of a prompt answered with `decision` (by the user, or a cancellation).
    pub fn of(decision: &Decision, source: &DecisionSource) -> Self {
        match (decision, source) {
            (_, DecisionSource::Cancelled) => Self::Expired,
            (Decision::Deny { .. }, _) => Self::Denied,
            _ => Self::Approved,
        }
    }
}

impl fmt::Display for ApprovalResolution {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Approved => "approved",
            Self::Denied => "denied",
            Self::Expired => "expired",
        })
    }
}

/// How a call would be confined if it runs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confinement {
    /// What the prompt shows.
    pub mode: SandboxMode,
    /// What the tool gets.
    pub permissions: ExecPermissions,
    /// Some outside write can't be granted (unknown, or too broad to scope).
    pub outside_unknown: bool,
}

/// Asks someone (usually the user) about a call the policy didn't auto-approve.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn decide(&self, req: ApprovalRequest) -> Decision;

    /// Whether [`Self::decide`] waits for a person, so asking it opens an approval prompt
    /// ([`PendingApproval`]). False for gates that answer on their own (a non-interactive
    /// session's default) and for relays whose other end reports the prompt itself.
    fn asks_user(&self) -> bool {
        true
    }
}

/// Denies every request it is asked about.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadOnlyGate;

#[async_trait]
impl ApprovalGate for ReadOnlyGate {
    fn asks_user(&self) -> bool {
        false
    }

    async fn decide(&self, req: ApprovalRequest) -> Decision {
        Decision::Deny {
            reason: format!(
                "{} needs approval ({}) and no one can approve it in this session",
                req.tool, req.risk
            ),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Rules

/// User-owned file holding approvals for the canonical workspace. Repository rules are ignored.
pub fn approvals_path(workspace_root: &Path) -> io::Result<PathBuf> {
    let dirs = directories::ProjectDirs::from("dev", "rozu", "rozu")
        .ok_or_else(|| io::Error::other("cannot locate the user configuration directory"))?;
    project_rules_path(workspace_root, &dirs.config_dir().join("approvals"))
}

fn project_rules_path(workspace_root: &Path, trusted_dir: &Path) -> io::Result<PathBuf> {
    let root = workspace_root.canonicalize()?;
    let key = crate::audit::sha256_hex(root.as_os_str().as_encoded_bytes());
    Ok(resolve_lenient(trusted_dir).join(format!("{key}.json")))
}

/// One user-approved tool prefix. The legacy `host` field is retained for decoding old data;
/// host-scoped network rules no longer authorize calls (see [`Tool::rules_allow`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefixRule {
    pub tool: String,
    pub prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl PrefixRule {
    fn allow_rule(&self) -> AllowRule {
        AllowRule {
            prefix: self.prefix.clone(),
            host: self.host.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalRules {
    #[serde(default = "rules_version")]
    pub version: u32,
    #[serde(default)]
    pub rules: Vec<PrefixRule>,
}

fn rules_version() -> u32 {
    1
}

impl ApprovalRules {
    /// Reads rules from `path`; a missing file means no rules.
    pub fn load(path: &Path) -> io::Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str(&text).map_err(|err| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{}: {err}", path.display()),
                )
            }),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(err) => Err(err),
        }
    }

    /// Writes rules atomically through a private, exclusively created temporary file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        let dir = path
            .parent()
            .ok_or_else(|| io::Error::other("rules need a directory"))?;
        let mut builder = std::fs::DirBuilder::new();
        builder.recursive(true);
        #[cfg(unix)]
        std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
        builder.create(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut json = serde_json::to_string_pretty(&Self {
            version: rules_version(),
            rules: self.rules.clone(),
        })
        .map_err(io::Error::other)?;
        json.push('\n');
        let mut tmp = tempfile::NamedTempFile::new_in(dir)?;
        tmp.write_all(json.as_bytes())?;
        tmp.persist(path).map_err(|err| err.error)?;
        Ok(())
    }

    fn rules_for(&self, tool: &str) -> Vec<AllowRule> {
        self.rules
            .iter()
            .filter(|r| r.tool == tool)
            .map(PrefixRule::allow_rule)
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Policy

/// The approval mode plus the project's "always allow" rules.
#[derive(Debug)]
pub struct ApprovalPolicy {
    mode: ApprovalMode,
    rules: Mutex<ApprovalRules>,
    /// Where rules are persisted; `None` keeps them in memory only.
    store: Option<PathBuf>,
}

impl ApprovalPolicy {
    /// A policy with in-memory rules.
    pub fn new(mode: ApprovalMode) -> Self {
        Self {
            mode,
            rules: Mutex::default(),
            store: None,
        }
    }

    /// Loads only this user's approvals, keyed by canonical workspace identity.
    pub fn for_project(mode: ApprovalMode, workspace_root: &Path) -> io::Result<Self> {
        Self::with_store(mode, workspace_root, approvals_path(workspace_root)?)
    }

    /// Uses an explicit user-owned approvals directory (also useful for isolated tests).
    pub fn for_project_in(
        mode: ApprovalMode,
        workspace_root: &Path,
        trusted_dir: &Path,
    ) -> io::Result<Self> {
        Self::with_store(
            mode,
            workspace_root,
            project_rules_path(workspace_root, trusted_dir)?,
        )
    }

    fn with_store(mode: ApprovalMode, workspace_root: &Path, store: PathBuf) -> io::Result<Self> {
        // Opening a broad workspace such as HOME must not let it supply its own authority.
        if store.starts_with(workspace_root.canonicalize()?) {
            return Ok(Self::new(mode));
        }
        let rules = ApprovalRules::load(&store)?;
        Ok(Self {
            mode,
            rules: Mutex::new(rules),
            store: Some(store),
        })
    }

    pub fn mode(&self) -> ApprovalMode {
        self.mode
    }

    pub fn rules(&self) -> ApprovalRules {
        self.rules.lock().expect("rules lock").clone()
    }

    /// How a call would be confined if it runs.
    ///
    /// Sandboxed tools run under the OS sandbox in every mode. The outside paths the call names
    /// are granted to it (for that call only, and only if it is approved); a path that can't be
    /// named or is too broad to scope (`/`, `/etc`, the home dir) isn't, and is reported as
    /// [`Confinement::outside_unknown`]. Platforms without a sandbox run unconfined.
    pub fn confinement(
        &self,
        tool: &dyn Tool,
        wants_network: bool,
        outside: &OutsideWrites,
        sandbox: &dyn Sandbox,
    ) -> Confinement {
        if !tool.sandboxed() {
            return Confinement {
                mode: SandboxMode::InProcess,
                permissions: ExecPermissions {
                    sandbox: false,
                    network: false,
                    extra_writable: Vec::new(),
                },
                outside_unknown: false,
            };
        }
        if !sandbox.enforced() {
            return Confinement {
                mode: SandboxMode::Off,
                permissions: ExecPermissions::unsandboxed(),
                outside_unknown: false,
            };
        }
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let mut outside_unknown = outside.unknown;
        let mut extra_writable: Vec<PathBuf> = Vec::new();
        for path in &outside.paths {
            let resolved = resolve_lenient(path);
            // Both forms: `/etc` resolves to the deeper-looking `/private/etc`.
            if too_broad_to_grant(path, home.as_deref())
                || too_broad_to_grant(&resolved, home.as_deref())
            {
                outside_unknown = true;
            } else if !extra_writable.contains(&resolved) {
                extra_writable.push(resolved);
            }
        }
        Confinement {
            mode: SandboxMode::Enforced(sandbox.name()),
            permissions: ExecPermissions {
                sandbox: true,
                network: wants_network,
                extra_writable,
            },
            outside_unknown,
        }
    }

    /// The rule "always allow" would store for this call, if it may be offered.
    pub fn allow_rule(&self, tool: &dyn Tool, args: &Value, risk: Risk) -> Option<AllowRule> {
        if matches!(risk, Risk::Destructive | Risk::Opaque | Risk::Network)
            || self.mode == ApprovalMode::ReadOnly
        {
            return None;
        }
        tool.allow_rule(args)
    }

    /// Who auto-approves `req`, or `None` if the gate must be asked. Never yields
    /// [`Decision::ApproveUnsandboxed`]: auto-approved calls keep their sandbox.
    pub fn auto_approval(&self, req: &ApprovalRequest, tool: &dyn Tool) -> Option<DecisionSource> {
        let by_mode = Some(DecisionSource::Mode(self.mode));
        match (req.risk, self.mode) {
            (Risk::Destructive | Risk::Opaque, _) => None,
            (Risk::Read, _) => by_mode,
            (_, ApprovalMode::ReadOnly) => None,
            (_, ApprovalMode::FullAccess) => by_mode,
            (Risk::WriteWorkspace, ApprovalMode::AutoWorkspace)
                if req.sandbox != SandboxMode::Off =>
            {
                by_mode
            }
            (_, ApprovalMode::AutoWorkspace) => self.matching_rule(req, tool),
        }
    }

    fn matching_rule(&self, req: &ApprovalRequest, tool: &dyn Tool) -> Option<DecisionSource> {
        if req.risk == Risk::Network || (req.network && req.sandbox != SandboxMode::Off) {
            return None;
        }
        let rules = self.rules.lock().expect("rules lock").rules_for(&req.tool);
        rules
            .iter()
            .find(|rule| tool.rules_allow(&req.args, std::slice::from_ref(rule)))
            .map(|rule| DecisionSource::Rule {
                prefix: rule.to_string(),
            })
    }

    /// Stores an "always allow" rule for `req` (and persists it, if this policy has a store).
    /// Ignored for destructive calls and calls without a rule.
    pub fn remember(&self, req: &ApprovalRequest) -> io::Result<()> {
        let Some(allow) = req.allow_rule.clone() else {
            return Ok(());
        };
        if matches!(req.risk, Risk::Destructive | Risk::Opaque | Risk::Network)
            || (req.network && req.sandbox != SandboxMode::Off)
        {
            return Ok(());
        }
        let mut rules = self.rules.lock().expect("rules lock");
        let rule = PrefixRule {
            tool: req.tool.clone(),
            prefix: allow.prefix,
            host: allow.host,
        };
        if !rules.rules.contains(&rule) {
            rules.rules.push(rule);
        }
        match &self.store {
            Some(path) => rules.save(path),
            None => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use rozu_tools::{NoSandbox, Seatbelt, Shell, ToolCtx, Workspace, WriteFile};
    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;

    struct Fixture {
        _dir: tempfile::TempDir,
        ctx: ToolCtx,
    }

    impl Fixture {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let ctx = ToolCtx::new(
                Workspace::new(dir.path()).unwrap(),
                CancellationToken::new(),
            );
            Self { _dir: dir, ctx }
        }

        /// A `shell` request under Seatbelt.
        fn sh(&self, policy: &ApprovalPolicy, command: &str) -> ApprovalRequest {
            self.request(policy, &Shell, shell(command), &Seatbelt)
        }

        fn request(
            &self,
            policy: &ApprovalPolicy,
            tool: &dyn Tool,
            args: Value,
            sandbox: &dyn Sandbox,
        ) -> ApprovalRequest {
            let risk = tool.risk(&args, &self.ctx);
            let network = tool.wants_network(&args, &self.ctx);
            let outside = tool.outside_writes(&args, &self.ctx);
            let confinement = policy.confinement(tool, network, &outside, sandbox);
            ApprovalRequest {
                call_id: "c1".into(),
                tool: tool.name().into(),
                allow_rule: policy.allow_rule(tool, &args, risk),
                args,
                risk,
                preview: String::new(),
                sandbox: confinement.mode,
                network: confinement.permissions.network,
                extra_writes: confinement.permissions.extra_writable,
                outside_unknown: confinement.outside_unknown,
                blocked_retry: false,
            }
        }
    }

    fn shell(command: &str) -> Value {
        json!({ "command": command })
    }

    const MODES: [ApprovalMode; 3] = [
        ApprovalMode::ReadOnly,
        ApprovalMode::AutoWorkspace,
        ApprovalMode::FullAccess,
    ];

    fn rule(policy: &ApprovalPolicy, prefix: &str) {
        policy.rules.lock().unwrap().rules.push(PrefixRule {
            tool: "shell".into(),
            prefix: prefix.into(),
            host: None,
        });
    }

    /// (command, risk, auto-approved in [read-only, auto, full]).
    #[test]
    fn approval_matrix() {
        let f = Fixture::new();
        let cases: &[(&str, Risk, [bool; 3])] = &[
            ("ls", Risk::Read, [true, true, true]),
            ("cargo test", Risk::WriteWorkspace, [false, true, true]),
            ("cp a /etc/a", Risk::WriteOutside, [false, false, true]),
            ("curl https://x", Risk::Network, [false, false, true]),
            ("rm -rf build", Risk::Destructive, [false, false, false]),
            ("git push --force", Risk::Destructive, [false, false, false]),
            // BUG-5: interpreter code and redirects ask even in full access.
            (
                "python3 -c 'import os'",
                Risk::Opaque,
                [false, false, false],
            ),
            (
                "perl -e 'unlink \"x\"'",
                Risk::Opaque,
                [false, false, false],
            ),
            (
                "eval \"$(echo cm0gLWYg | base64 -d)x\"",
                Risk::Opaque,
                [false, false, false],
            ),
            ("> out.txt", Risk::Opaque, [false, false, false]),
            // Script files in the workspace run sandboxed like `make`, whatever the interpreter.
            (
                "python3 script.py",
                Risk::WriteWorkspace,
                [false, true, true],
            ),
            ("sh script.sh", Risk::WriteWorkspace, [false, true, true]),
            ("node script.js", Risk::WriteWorkspace, [false, true, true]),
            ("python3 /opt/x.py", Risk::Opaque, [false, false, false]),
            ("sh /opt/x.sh", Risk::Opaque, [false, false, false]),
        ];
        for (command, risk, expected) in cases {
            for (mode, auto) in MODES.into_iter().zip(expected) {
                let policy = ApprovalPolicy::new(mode);
                let req = f.sh(&policy, command);
                assert_eq!(req.risk, *risk, "{command}");
                assert_eq!(
                    policy.auto_approval(&req, &Shell).is_some(),
                    *auto,
                    "{command} in {mode}"
                );
            }
        }
    }

    #[test]
    fn workspace_writes_need_a_sandbox_to_be_automatic() {
        let f = Fixture::new();
        let policy = ApprovalPolicy::new(ApprovalMode::AutoWorkspace);
        let req = f.request(&policy, &Shell, shell("make"), &NoSandbox);
        assert_eq!(req.sandbox, SandboxMode::Off);
        assert_eq!(policy.auto_approval(&req, &Shell), None);
        // In-process write tools don't need one.
        let req = f.request(
            &policy,
            &WriteFile,
            json!({"path": "a.txt", "content": ""}),
            &NoSandbox,
        );
        assert_eq!(req.sandbox, SandboxMode::InProcess);
        assert!(policy.auto_approval(&req, &WriteFile).is_some());
    }

    #[test]
    fn every_mode_runs_shell_commands_in_the_sandbox() {
        let f = Fixture::new();
        for mode in MODES {
            let policy = ApprovalPolicy::new(mode);
            for command in [
                "cargo test",
                "cp a /etc/a",
                "python3 -c 'pass'",
                "rm -rf build",
            ] {
                let req = f.sh(&policy, command);
                assert_eq!(
                    req.sandbox,
                    SandboxMode::Enforced("seatbelt"),
                    "{command} {mode}"
                );
                assert!(req.can_run_unsandboxed());
            }
            let req = f.sh(&policy, "cargo test");
            assert!(!req.network, "{mode}");
            let req = f.sh(&policy, "curl x");
            assert!(
                req.network,
                "approved network calls get the network ({mode})"
            );
        }
        // Only a platform without a sandbox runs unconfined, and then `u` isn't offered.
        let full = ApprovalPolicy::new(ApprovalMode::FullAccess);
        let req = f.request(&full, &Shell, shell("cargo test"), &NoSandbox);
        assert_eq!(req.sandbox, SandboxMode::Off);
        assert!(!req.can_run_unsandboxed());
    }

    #[test]
    fn approved_outside_targets_are_granted_to_that_call() {
        let f = Fixture::new();
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("scratch/approved_canary.txt");
        let policy = ApprovalPolicy::new(ApprovalMode::AutoWorkspace);
        let command = format!("echo approved > {}", target.display());
        let req = f.sh(&policy, &command);
        assert_eq!(req.risk, Risk::Destructive);
        assert_eq!(
            req.extra_writes,
            [outside
                .path()
                .canonicalize()
                .unwrap()
                .join("scratch/approved_canary.txt")]
        );
        assert!(!req.outside_unknown);
        // Unknown and too-broad targets aren't granted.
        let req = f.sh(&policy, "echo x > $NOWHERE/f");
        assert!(req.extra_writes.is_empty() && req.outside_unknown);
        let req = f.sh(&policy, "rm -rf /etc");
        assert!(req.extra_writes.is_empty() && req.outside_unknown);
    }

    #[test]
    fn destructive_always_prompts_even_with_a_rule() {
        let f = Fixture::new();
        for mode in MODES {
            let policy = ApprovalPolicy::new(mode);
            rule(&policy, "rm");
            let req = f.sh(&policy, "rm -rf build");
            assert_eq!(req.risk, Risk::Destructive);
            assert_eq!(req.allow_rule, None, "{mode}");
            assert_eq!(policy.auto_approval(&req, &Shell), None, "{mode}");
            // Even a forged "always allow" doesn't store a destructive rule.
            let mut forged = req.clone();
            forged.allow_rule = Some(AllowRule::prefix("rm -rf"));
            policy.remember(&forged).unwrap();
            assert!(!policy.rules().rules.iter().any(|r| r.prefix == "rm -rf"));
        }
    }

    #[test]
    fn inline_code_is_never_covered_by_a_rule() {
        let f = Fixture::new();
        for mode in [ApprovalMode::AutoWorkspace, ApprovalMode::FullAccess] {
            let policy = ApprovalPolicy::new(mode);
            rule(&policy, "python3");
            let req = f.sh(&policy, "python3 -c 'import os'");
            assert_eq!(req.allow_rule, None);
            assert_eq!(policy.auto_approval(&req, &Shell), None, "{mode}");
            // A named script may be.
            let req = f.sh(&policy, "python3 gen.py");
            assert_eq!(req.allow_rule, Some(AllowRule::prefix("python3 gen.py")));
            assert!(policy.auto_approval(&req, &Shell).is_some(), "{mode}");
        }
    }

    #[test]
    fn opaque_calls_ignore_stored_rules_and_cannot_be_remembered() {
        let f = Fixture::new();
        for mode in MODES {
            let policy = ApprovalPolicy::new(mode);
            rule(&policy, "python3");
            for command in [
                "python3 -m pytest",
                "python3 /opt/script.py",
                "git status",
                "./cat file",
                "ssh -F /dev/null -o 'ProxyCommand=rm -rf victim' example.invalid",
                "curl --engine ./engine.so https://example.com",
            ] {
                let mut req = f.sh(&policy, command);
                assert_eq!(req.risk, Risk::Opaque, "{command}");
                assert!(req.allow_rule.is_none());
                assert!(policy.auto_approval(&req, &Shell).is_none());
                req.allow_rule = Some(AllowRule::prefix(command));
                policy.remember(&req).unwrap();
                assert_eq!(policy.rules().rules.len(), 1);
            }
        }
    }

    #[test]
    fn approvals_are_private_user_decisions_bound_to_a_project() {
        let f = Fixture::new();
        let root = f.ctx.workspace.root();
        let trusted = tempfile::tempdir().unwrap();
        let legacy = root.join(".rozu/approvals.json");
        std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
        std::fs::write(
            &legacy,
            r#"{"rules":[{"tool":"shell","prefix":"python3 gen.py"}]}"#,
        )
        .unwrap();
        let policy =
            ApprovalPolicy::for_project_in(ApprovalMode::AutoWorkspace, root, trusted.path())
                .unwrap();
        assert!(
            policy.rules().rules.is_empty(),
            "repository rules are not user decisions"
        );
        let req = f.request(&policy, &Shell, shell("python3 gen.py"), &Seatbelt);
        policy.remember(&req).unwrap();
        policy.remember(&req).unwrap();
        let path = project_rules_path(root, trusted.path()).unwrap();
        let saved = ApprovalRules::load(&path).unwrap();
        assert_eq!(saved.rules.len(), 1);
        let reloaded =
            ApprovalPolicy::for_project_in(ApprovalMode::AutoWorkspace, root, trusted.path())
                .unwrap();
        assert_eq!(reloaded.rules(), saved);
        let other = tempfile::tempdir().unwrap();
        assert!(
            ApprovalPolicy::for_project_in(
                ApprovalMode::AutoWorkspace,
                other.path(),
                trusted.path()
            )
            .unwrap()
            .rules()
            .rules
            .is_empty()
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                std::fs::metadata(trusted.path())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            let alias_dir = tempfile::tempdir().unwrap();
            let alias = alias_dir.path().join("project");
            std::os::unix::fs::symlink(root, &alias).unwrap();
            assert_eq!(
                project_rules_path(&alias, trusted.path()).unwrap(),
                project_rules_path(root, trusted.path()).unwrap()
            );
        }
    }

    #[test]
    fn a_workspace_cannot_supply_its_own_trusted_store() {
        let f = Fixture::new();
        let root = f.ctx.workspace.root();
        let trusted = root.join("config/approvals");
        let path = project_rules_path(root, &trusted).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "invalid attacker data").unwrap();
        let policy =
            ApprovalPolicy::for_project_in(ApprovalMode::AutoWorkspace, root, &trusted).unwrap();
        assert!(policy.store.is_none() && policy.rules().rules.is_empty());
        assert_eq!(
            std::fs::read_to_string(path).unwrap(),
            "invalid attacker data"
        );
    }

    #[test]
    fn network_grants_never_use_or_create_remembered_rules() {
        let f = Fixture::new();
        let policy = ApprovalPolicy::new(ApprovalMode::AutoWorkspace);
        rule(&policy, "cargo test");
        rule(&policy, "curl");
        for args in [
            shell("curl https://example.com"),
            json!({"command":"cargo test", "network":true}),
        ] {
            let mut req = f.request(&policy, &Shell, args, &Seatbelt);
            assert!(req.network);
            assert_eq!(policy.auto_approval(&req, &Shell), None);
            assert_eq!(req.allow_rule, None);
            req.allow_rule = Some(AllowRule::with_host("curl", "example.com"));
            policy.remember(&req).unwrap();
            assert_eq!(policy.rules().rules.len(), 2);
        }
    }

    #[test]
    fn rules_without_a_host_still_load() {
        let rules: ApprovalRules = serde_json::from_str(
            r#"{"version":1,"rules":[{"tool":"shell","prefix":"cargo test"}]}"#,
        )
        .unwrap();
        assert_eq!(rules.rules[0].host, None);
    }

    #[test]
    fn corrupt_rules_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let trusted = tempfile::tempdir().unwrap();
        let path = project_rules_path(dir.path(), trusted.path()).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(
            ApprovalPolicy::for_project_in(ApprovalMode::AutoWorkspace, dir.path(), trusted.path())
                .is_err()
        );
    }
}
