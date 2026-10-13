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
//! | network         | prompt    | rule or prompt             | auto        |
//! | destructive     | prompt    | prompt                     | prompt      |
//!
//! Destructive calls always prompt and can never be covered by a rule. Rules are stored in
//! `<workspace>/.rozu/approvals.json`, which the tools themselves can't write.

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use async_trait::async_trait;
use rozu_tools::{ExecPermissions, PROTECTED_DIR, Risk, Sandbox, Tool};
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
    /// Everything runs without asking, unsandboxed, except destructive calls.
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
    /// A subprocess under the named OS sandbox: writes only in the workspace and temp dirs.
    Enforced(&'static str),
    /// A subprocess with no OS sandbox.
    Off,
    /// Runs inside rozu; the tool itself only touches workspace paths.
    InProcess,
}

impl fmt::Display for SandboxMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SandboxMode::Enforced(name) => write!(f, "{name} (writes: workspace + tmp)"),
            SandboxMode::Off => f.write_str("off"),
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
    /// What "always allow" would remember; `None` when it isn't offered (always for
    /// destructive calls).
    pub allow_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Approve,
    /// The reason is sent back to the model as the tool error.
    Deny {
        reason: String,
    },
    /// Approve, and auto-approve calls with [`ApprovalRequest::allow_prefix`] in this project
    /// from now on.
    AlwaysAllowPrefix,
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
}

impl fmt::Display for DecisionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecisionSource::Mode(mode) => write!(f, "{mode} mode"),
            DecisionSource::Rule { prefix } => write!(f, "rule `{prefix}`"),
            DecisionSource::Gate => f.write_str("user"),
            DecisionSource::Cancelled => f.write_str("cancelled"),
        }
    }
}

/// Asks someone (usually the user) about a call the policy didn't auto-approve.
#[async_trait]
pub trait ApprovalGate: Send + Sync {
    async fn decide(&self, req: ApprovalRequest) -> Decision;
}

/// Denies every request it is asked about.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadOnlyGate;

#[async_trait]
impl ApprovalGate for ReadOnlyGate {
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

/// File (under the workspace) holding the per-project "always allow" rules.
pub fn approvals_path(workspace_root: &Path) -> PathBuf {
    workspace_root.join(PROTECTED_DIR).join("approvals.json")
}

/// One "always allow" rule: calls to `tool` covered by `prefix` (see [`Tool::prefix_allows`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrefixRule {
    pub tool: String,
    pub prefix: String,
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

    /// Writes the rules to `path` (atomically), creating its directory.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut json = serde_json::to_string_pretty(&Self {
            version: rules_version(),
            rules: self.rules.clone(),
        })
        .map_err(io::Error::other)?;
        json.push('\n');
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, path)
    }

    fn prefixes_for(&self, tool: &str) -> Vec<String> {
        self.rules
            .iter()
            .filter(|r| r.tool == tool)
            .map(|r| r.prefix.clone())
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

    /// A policy whose rules live in `<workspace>/.rozu/approvals.json`.
    pub fn for_project(mode: ApprovalMode, workspace_root: &Path) -> io::Result<Self> {
        let store = approvals_path(workspace_root);
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

    /// How a call would be confined if it runs: (what the prompt shows, what the tool gets).
    pub fn confinement(
        &self,
        tool: &dyn Tool,
        risk: Risk,
        wants_network: bool,
        sandbox: &dyn Sandbox,
    ) -> (SandboxMode, ExecPermissions) {
        if !tool.sandboxed() {
            return (
                SandboxMode::InProcess,
                ExecPermissions {
                    sandbox: false,
                    network: false,
                },
            );
        }
        let unconfined = (
            SandboxMode::Off,
            ExecPermissions {
                sandbox: false,
                network: true,
            },
        );
        // Full access runs unsandboxed; an approved write outside the workspace can't work
        // inside the sandbox; and some platforms have no sandbox at all.
        if self.mode == ApprovalMode::FullAccess
            || risk == Risk::WriteOutside
            || !sandbox.enforced()
        {
            return unconfined;
        }
        (
            SandboxMode::Enforced(sandbox.name()),
            ExecPermissions {
                sandbox: true,
                network: wants_network,
            },
        )
    }

    /// The prefix "always allow" would store for this call, if it may be offered.
    pub fn allow_prefix(&self, tool: &dyn Tool, args: &Value, risk: Risk) -> Option<String> {
        if risk == Risk::Destructive || self.mode == ApprovalMode::ReadOnly {
            return None;
        }
        tool.allow_prefix(args)
    }

    /// Who auto-approves `req`, or `None` if the gate must be asked.
    pub fn auto_approval(&self, req: &ApprovalRequest, tool: &dyn Tool) -> Option<DecisionSource> {
        let by_mode = Some(DecisionSource::Mode(self.mode));
        match (req.risk, self.mode) {
            (Risk::Destructive, _) => None,
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
        let prefixes = self
            .rules
            .lock()
            .expect("rules lock")
            .prefixes_for(&req.tool);
        prefixes
            .iter()
            .find(|prefix| tool.prefix_allows(&req.args, std::slice::from_ref(prefix)))
            .map(|prefix| DecisionSource::Rule {
                prefix: prefix.clone(),
            })
    }

    /// Stores an "always allow" rule for `req` (and persists it, if this policy has a store).
    /// Ignored for destructive calls and calls without a prefix.
    pub fn remember(&self, req: &ApprovalRequest) -> io::Result<()> {
        let Some(prefix) = req.allow_prefix.clone() else {
            return Ok(());
        };
        if req.risk == Risk::Destructive {
            return Ok(());
        }
        let mut rules = self.rules.lock().expect("rules lock");
        let rule = PrefixRule {
            tool: req.tool.clone(),
            prefix,
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

        fn request(
            &self,
            policy: &ApprovalPolicy,
            tool: &dyn Tool,
            args: Value,
            sandbox: &dyn Sandbox,
        ) -> ApprovalRequest {
            let risk = tool.risk(&args, &self.ctx);
            let network = tool.wants_network(&args, &self.ctx);
            let (sandbox, perms) = policy.confinement(tool, risk, network, sandbox);
            ApprovalRequest {
                call_id: "c1".into(),
                tool: tool.name().into(),
                allow_prefix: policy.allow_prefix(tool, &args, risk),
                args,
                risk,
                preview: String::new(),
                sandbox,
                network: perms.network,
            }
        }
    }

    fn shell(command: &str) -> Value {
        json!({ "command": command })
    }

    /// (risk, mode) → auto-approved? Uses the shell tool under Seatbelt.
    #[test]
    fn approval_matrix() {
        use ApprovalMode::*;
        let f = Fixture::new();
        let cases: &[(&str, Risk, [bool; 3])] = &[
            //                                        read-only, auto, full
            ("ls", Risk::Read, [true, true, true]),
            ("cargo test", Risk::WriteWorkspace, [false, true, true]),
            ("cp a /etc/a", Risk::WriteOutside, [false, false, true]),
            ("curl https://x", Risk::Network, [false, false, true]),
            ("rm -rf build", Risk::Destructive, [false, false, false]),
            ("git push --force", Risk::Destructive, [false, false, false]),
        ];
        for (command, risk, expected) in cases {
            for (mode, auto) in [ReadOnly, AutoWorkspace, FullAccess]
                .into_iter()
                .zip(expected)
            {
                let policy = ApprovalPolicy::new(mode);
                let req = f.request(&policy, &Shell, shell(command), &Seatbelt);
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
    fn confinement_per_mode() {
        let f = Fixture::new();
        let auto = ApprovalPolicy::new(ApprovalMode::AutoWorkspace);
        let req = f.request(&auto, &Shell, shell("cargo test"), &Seatbelt);
        assert_eq!(req.sandbox, SandboxMode::Enforced("seatbelt"));
        assert!(!req.network);
        let req = f.request(&auto, &Shell, shell("curl x"), &Seatbelt);
        assert_eq!(req.sandbox, SandboxMode::Enforced("seatbelt"));
        assert!(req.network, "approved network calls get the network");
        let req = f.request(&auto, &Shell, shell("cp a /etc/a"), &Seatbelt);
        assert_eq!(req.sandbox, SandboxMode::Off);

        let full = ApprovalPolicy::new(ApprovalMode::FullAccess);
        let req = f.request(&full, &Shell, shell("cargo test"), &Seatbelt);
        assert_eq!(req.sandbox, SandboxMode::Off);
        assert!(req.network);
    }

    #[test]
    fn destructive_always_prompts_even_with_a_rule() {
        let f = Fixture::new();
        for mode in [
            ApprovalMode::ReadOnly,
            ApprovalMode::AutoWorkspace,
            ApprovalMode::FullAccess,
        ] {
            let policy = ApprovalPolicy::new(mode);
            policy.rules.lock().unwrap().rules.push(PrefixRule {
                tool: "shell".into(),
                prefix: "rm".into(),
            });
            let req = f.request(&policy, &Shell, shell("rm -rf build"), &Seatbelt);
            assert_eq!(req.risk, Risk::Destructive);
            assert_eq!(req.allow_prefix, None, "{mode}");
            assert_eq!(policy.auto_approval(&req, &Shell), None, "{mode}");
            // Even a forged "always allow" doesn't store a destructive rule.
            let mut forged = req.clone();
            forged.allow_prefix = Some("rm -rf".into());
            policy.remember(&forged).unwrap();
            assert!(!policy.rules().rules.iter().any(|r| r.prefix == "rm -rf"));
        }
    }

    #[test]
    fn prefix_rules_persist_per_project() {
        let f = Fixture::new();
        let root = f.ctx.workspace.root();
        let policy = ApprovalPolicy::for_project(ApprovalMode::AutoWorkspace, root).unwrap();
        let req = f.request(
            &policy,
            &Shell,
            shell("curl -s https://example.com"),
            &Seatbelt,
        );
        assert_eq!(policy.auto_approval(&req, &Shell), None);
        assert_eq!(req.allow_prefix.as_deref(), Some("curl"));

        policy.remember(&req).unwrap();
        policy.remember(&req).unwrap(); // no duplicates
        let saved = std::fs::read_to_string(approvals_path(root)).unwrap();
        let parsed: ApprovalRules = serde_json::from_str(&saved).unwrap();
        assert_eq!(
            parsed.rules,
            [PrefixRule {
                tool: "shell".into(),
                prefix: "curl".into()
            }]
        );

        // A fresh policy for the same project picks the rule up.
        let reloaded = ApprovalPolicy::for_project(ApprovalMode::AutoWorkspace, root).unwrap();
        let req = f.request(&reloaded, &Shell, shell("curl https://other"), &Seatbelt);
        assert_eq!(
            reloaded.auto_approval(&req, &Shell),
            Some(DecisionSource::Rule {
                prefix: "curl".into()
            })
        );
        // ...but not for other commands, compound commands that sneak something else in, or
        // in read-only mode.
        let req = f.request(&reloaded, &Shell, shell("wget https://other"), &Seatbelt);
        assert_eq!(reloaded.auto_approval(&req, &Shell), None);
        let req = f.request(&reloaded, &Shell, shell("curl x && cp a /etc/a"), &Seatbelt);
        assert_eq!(reloaded.auto_approval(&req, &Shell), None);
        let read_only = ApprovalPolicy::for_project(ApprovalMode::ReadOnly, root).unwrap();
        let req = f.request(&read_only, &Shell, shell("curl x"), &Seatbelt);
        assert_eq!(read_only.auto_approval(&req, &Shell), None);
    }

    #[test]
    fn corrupt_rules_file_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let path = approvals_path(dir.path());
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "{not json").unwrap();
        assert!(ApprovalPolicy::for_project(ApprovalMode::AutoWorkspace, dir.path()).is_err());
    }
}
