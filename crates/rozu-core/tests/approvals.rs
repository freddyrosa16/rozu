//! Regression tests for the QA report on f6289f2: the sandbox is the boundary in every approval
//! mode (BUG-5), approved outside writes run scoped (BUG-6), the unsandboxed escape, and the
//! approval audit log (BUG-7). [`FakeSandbox`] keeps them platform-independent; the `macos`
//! module repeats the key cases under real Seatbelt.

mod common;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use common::{ScriptedGate, approvals, deny};
use rozu_core::{
    ApprovalMode, ApprovalPolicy, ApprovalRecord, ApprovalRequest, AuditLog, Decision,
    DecisionSource, SandboxMode, ToolExecutor,
};
use rozu_tools::{
    Risk, SANDBOX_BLOCKED, Sandbox, SandboxPolicy, Shell, ToolOutput, ToolRegistry, Workspace,
};
use serde_json::{Value, json};
use tokio::process::Command;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

/// An enforced sandbox that records each policy, then runs the command plainly. With
/// `refuse`, it fails every command like Seatbelt refusing a write.
#[derive(Debug, Default)]
struct FakeSandbox {
    policies: Mutex<Vec<SandboxPolicy>>,
    refuse: bool,
}

impl FakeSandbox {
    fn policies(&self) -> Vec<SandboxPolicy> {
        self.policies.lock().unwrap().clone()
    }
}

impl Sandbox for FakeSandbox {
    fn name(&self) -> &'static str {
        "fake"
    }

    fn enforced(&self) -> bool {
        true
    }

    fn command(&self, script: &str, policy: &SandboxPolicy) -> Command {
        self.policies.lock().unwrap().push(policy.clone());
        let mut cmd = Command::new("/bin/sh");
        if self.refuse {
            cmd.arg("-c")
                .arg("echo '/bin/sh: target: Operation not permitted' >&2; exit 1");
        } else {
            cmd.arg("-c").arg(script);
        }
        cmd
    }
}

struct Fixture {
    _dirs: (tempfile::TempDir, tempfile::TempDir),
    workspace: Workspace,
    /// Outside the workspace and every temp dir rozu would allow.
    outside: PathBuf,
    exec: ToolExecutor,
    gate: Arc<ScriptedGate>,
}

impl Fixture {
    fn new(
        mode: ApprovalMode,
        sandbox: Arc<dyn Sandbox>,
        answers: impl IntoIterator<Item = Decision>,
    ) -> Self {
        let gate = ScriptedGate::new(answers);
        let ws = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let out = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
        let workspace = Workspace::new(ws.path()).unwrap();
        let outside = out.path().canonicalize().unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Shell);
        let policy =
            ApprovalPolicy::for_project_in(mode, workspace.root(), &outside.join("approvals"))
                .unwrap();
        let exec = ToolExecutor::new(Arc::new(tools), gate.clone(), Arc::new(policy), sandbox);
        Self {
            _dirs: (ws, out),
            workspace,
            outside,
            exec,
            gate,
        }
    }

    #[cfg_attr(not(target_os = "macos"), allow(dead_code))]
    fn canary(&self, name: &str) -> PathBuf {
        let path = self.outside.join(name);
        std::fs::write(&path, "canary").unwrap();
        path
    }

    async fn shell(&self, command: &str) -> (ToolOutput, Vec<ApprovalRecord>) {
        let (tx, mut rx) = mpsc::channel(16);
        let ctx = self
            .exec
            .ctx(self.workspace.clone(), CancellationToken::new());
        let args = json!({ "command": command }).to_string();
        let output = self.exec.execute("c1", "shell", &args, &ctx, &tx).await;
        drop(tx);
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        (output, approvals(&events))
    }
}

/// The QA's BUG-5 variants against canaries in `outside`: (command, canary that must survive).
fn qa_variants(outside: &Path) -> Vec<(String, PathBuf)> {
    let o = outside.display();
    vec![
        (
            format!("python3 -c \"import os; os.remove('{o}/c1.txt')\""),
            outside.join("c1.txt"),
        ),
        (
            format!("perl -e 'unlink \"{o}/c2.txt\"'"),
            outside.join("c2.txt"),
        ),
        (
            // `cm0gLWYg` is base64 for `rm -f `.
            format!("eval \"$(echo cm0gLWYg | base64 -d){o}/c3.txt\""),
            outside.join("c3.txt"),
        ),
        (
            format!("python3 -c \"import shutil; shutil.rmtree('{o}/d4')\""),
            outside.join("d4/c4.txt"),
        ),
        // A bare `> file` truncation, after a `cd` the classifier can't follow.
        (format!("cd '{o}' && > c5.txt"), outside.join("c5.txt")),
    ]
}

fn faked(
    mode: ApprovalMode,
    answers: impl IntoIterator<Item = Decision>,
    refuse: bool,
) -> (Fixture, Arc<FakeSandbox>) {
    let sandbox = Arc::new(FakeSandbox {
        refuse,
        ..FakeSandbox::default()
    });
    (Fixture::new(mode, sandbox.clone(), answers), sandbox)
}

fn plant(variants: &[(String, PathBuf)]) {
    for (_, canary) in variants {
        std::fs::create_dir_all(canary.parent().unwrap()).unwrap();
        std::fs::write(canary, "canary").unwrap();
    }
}

#[tokio::test]
async fn qa_interpreter_and_redirect_deletes_prompt_in_full_mode() {
    let (f, sandbox) = faked(ApprovalMode::FullAccess, [], false);
    let variants = qa_variants(&f.outside);
    plant(&variants);

    for (command, canary) in &variants {
        let (output, records) = f.shell(command).await;
        assert!(
            output.is_error && output.content.starts_with("denied:"),
            "{command}: {output:?}"
        );
        assert_eq!(
            std::fs::read_to_string(canary).unwrap(),
            "canary",
            "{command}"
        );
        assert_eq!(records.len(), 1, "{command}");
        assert_eq!(records[0].source, DecisionSource::Gate, "{command}");
    }
    let asked = f.gate.asked();
    assert_eq!(asked.len(), variants.len());
    for req in &asked {
        assert!(req.risk >= Risk::Opaque, "{}: {}", req.preview, req.risk);
        assert_eq!(req.allow_rule, None, "{}", req.preview);
        assert!(req.can_run_unsandboxed());
    }
    assert!(sandbox.policies().is_empty(), "nothing may run");
}

#[tokio::test]
async fn full_mode_still_runs_everything_in_the_sandbox() {
    let (f, sandbox) = faked(ApprovalMode::FullAccess, [Decision::Approve], false);
    let (output, records) = f.shell("mkdir -p build").await;
    assert!(!output.is_error, "{output:?}");
    assert_eq!(
        records[0].source,
        DecisionSource::Mode(ApprovalMode::FullAccess)
    );
    assert!(records[0].sandboxed());
    // Mode- and user-approved alike run sandboxed, with workspace + session tmp + /dev only.
    let (_, records) = f.shell("python3 -c 'pass'").await;
    assert_eq!(records[0].decision, Decision::Approve);
    let policies = sandbox.policies();
    assert_eq!(policies.len(), 2);
    for policy in &policies {
        assert_eq!(policy.writable_roots[0], f.workspace.root());
        assert_eq!(policy.writable_roots.len(), 3, "{policy:?}");
        assert!(!policy.writable_roots.contains(&PathBuf::from("/tmp")));
    }
}

#[tokio::test]
async fn unsandboxed_escape_runs_once_and_is_not_persisted() {
    let (f, sandbox) = faked(
        ApprovalMode::AutoWorkspace,
        [Decision::ApproveUnsandboxed, deny("no")],
        false,
    );
    let target = f.outside.join("u.txt");
    let command = format!("echo once >> '{}'", target.display());

    let (output, records) = f.shell(&command).await;
    assert!(!output.is_error, "{output:?}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "once\n");
    assert_eq!(records[0].decision, Decision::ApproveUnsandboxed);
    assert!(!records[0].sandboxed());
    assert!(sandbox.policies().is_empty(), "the sandbox was bypassed");

    // Not remembered: the same command asks again.
    let (output, _) = f.shell(&command).await;
    assert!(output.content.starts_with("denied:"), "{output:?}");
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "once\n");
    assert_eq!(f.gate.asked().len(), 2);
    assert!(!f.outside.join("approvals").exists());
}

/// The policy never lifts the sandbox on its own, whatever the mode or rules.
#[tokio::test]
async fn auto_approvals_never_run_unsandboxed() {
    for mode in [ApprovalMode::AutoWorkspace, ApprovalMode::FullAccess] {
        let (f, sandbox) = faked(mode, [Decision::ApproveUnsandboxed], false);
        let (_, records) = f.shell("touch made.txt").await;
        assert!(matches!(records[0].source, DecisionSource::Mode(_)));
        assert!(records[0].sandboxed());
        assert_eq!(sandbox.policies().len(), 1, "{mode}");
        assert!(f.gate.asked().is_empty());
    }
}

#[tokio::test]
async fn approved_outside_write_gets_exactly_that_path() {
    let (f, sandbox) = faked(ApprovalMode::AutoWorkspace, [Decision::Approve], false);
    let target = f.outside.join("scratch/approved_canary.txt");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    let command = format!("echo approved > {}", target.display());

    let (output, records) = f.shell(&command).await;

    assert!(!output.is_error, "{output:?}");
    let asked = f.gate.asked();
    assert_eq!(asked[0].extra_writes, std::slice::from_ref(&target));
    assert!(!asked[0].outside_unknown);
    assert_eq!(records[0].extra_writes, std::slice::from_ref(&target));
    let policies = sandbox.policies();
    assert_eq!(policies.len(), 1);
    assert!(policies[0].writable_roots.contains(&target));
    assert!(!policies[0].writable_roots.contains(&f.outside));
}

#[tokio::test]
async fn sandbox_block_says_so_and_offers_the_escape_once() {
    // Approve, then keep the failure when offered the unsandboxed rerun.
    let (f, _) = faked(
        ApprovalMode::AutoWorkspace,
        [Decision::Approve, deny("no")],
        true,
    );
    let (output, records) = f.shell("python3 -c 'write_somewhere()'").await;

    assert!(output.is_error);
    assert!(
        output.content.contains(SANDBOX_BLOCKED),
        "{}",
        output.content
    );
    assert!(
        output
            .content
            .contains("blocked by rozu sandbox: writes outside workspace"),
        "{}",
        output.content
    );
    let asked = f.gate.asked();
    assert_eq!(asked.len(), 2);
    assert!(!asked[0].blocked_retry && asked[1].blocked_retry);
    assert_eq!(asked[1].allow_rule, None);
    assert_eq!(records.len(), 2);
    assert!(records[1].blocked_retry);
}

#[tokio::test]
async fn sandbox_block_then_u_reruns_unsandboxed() {
    let (f, sandbox) = faked(
        ApprovalMode::AutoWorkspace,
        [Decision::Approve, Decision::ApproveUnsandboxed],
        true,
    );
    let target = f.outside.join("rerun.txt");
    let (output, records) = f
        .shell(&format!(
            "python3 -c \"open('{}', 'w').write('ok')\" || echo ok > '{}'",
            target.display(),
            target.display()
        ))
        .await;

    assert!(
        output.content.starts_with(
            "[the sandbox blocked the first run; the user approved rerunning it unsandboxed]"
        ),
        "{}",
        output.content
    );
    assert!(target.exists());
    assert_eq!(records[1].decision, Decision::ApproveUnsandboxed);
    assert!(records[1].blocked_retry && !records[1].sandboxed());
    assert_eq!(
        sandbox.policies().len(),
        1,
        "only the first run was sandboxed"
    );
}

#[tokio::test]
async fn every_decision_lands_in_the_audit_log() {
    let (mut f, _) = faked(
        ApprovalMode::AutoWorkspace,
        [
            Decision::Approve,
            deny("no"),
            Decision::AlwaysAllowPrefix,
            Decision::ApproveUnsandboxed,
        ],
        false,
    );
    let log_path = f.outside.join("logs/approvals.jsonl");
    f.exec = f.exec.clone().with_audit(Some(Arc::new(
        AuditLog::open(&log_path, "session-42").unwrap(),
    )));

    let copy = format!("cp a.txt {}", f.outside.join("copied.txt").display());
    let mut records = Vec::new();
    for command in [
        "ls",                                           // auto: read
        "touch a.txt",                                  // auto: sandboxed workspace write
        "python3 -m pytest",                            // user: approve
        "curl https://example.com",                     // user: deny
        copy.as_str(),                                  // user: always allow
        copy.as_str(),                                  // rule
        "echo secret sk-ant-abcdefghijklmnopqrstu > f", // user: unsandboxed
    ] {
        records.extend(f.shell(command).await.1);
    }

    let text = std::fs::read_to_string(&log_path).unwrap();
    let entries: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), records.len());
    assert_eq!(entries.len(), 7);
    let field = |key: &str| -> Vec<String> {
        entries
            .iter()
            .map(|e| e[key].as_str().unwrap_or("-").to_owned())
            .collect()
    };
    assert_eq!(
        field("decided_by"),
        ["mode", "mode", "user", "user", "user", "rule", "user"]
    );
    assert_eq!(
        field("kind"),
        [
            "approve",
            "approve",
            "approve",
            "deny",
            "always_allow",
            "approve",
            "approve_unsandboxed"
        ]
    );
    assert_eq!(
        field("decision"),
        ["allow", "allow", "allow", "deny", "allow", "allow", "allow"]
    );
    assert_eq!(entries[5]["rule"], "cp");
    assert_eq!(entries[3]["reason"], "no");
    for entry in &entries {
        assert_eq!(entry["session_id"], "session-42");
        assert_eq!(entry["mode"], "auto");
        assert_eq!(entry["tool"], "shell");
        assert_eq!(entry["preview_sha256"].as_str().unwrap().len(), 64);
    }
    assert_eq!(entries[1]["sandboxed"], true);
    assert_eq!(entries[6]["sandboxed"], false);
    assert!(!text.contains("abcdefghijklmnopqrstu"), "keys are redacted");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&log_path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[cfg(target_os = "macos")]
mod macos {
    use rozu_tools::Seatbelt;

    use super::*;
    use crate::common::seatbelt_usable;

    fn seatbelt(
        mode: ApprovalMode,
        answers: impl IntoIterator<Item = Decision>,
    ) -> Option<Fixture> {
        if !seatbelt_usable() {
            eprintln!("skipping: sandbox-exec is unavailable or can't nest here");
            return None;
        }
        Some(Fixture::new(mode, Arc::new(Seatbelt), answers))
    }

    /// BUG-5: plainly approved (not `u`), no QA variant touches the canaries outside.
    #[tokio::test]
    async fn approved_qa_variants_cannot_touch_outside_canaries() {
        // Approve every prompt, including "rerun unsandboxed?" (Approve isn't `u`).
        let Some(f) = seatbelt(
            ApprovalMode::FullAccess,
            std::iter::repeat_n(Decision::Approve, 20),
        ) else {
            return;
        };
        let variants = qa_variants(&f.outside);
        plant(&variants);
        let bystander = f.canary("bystander.txt");

        for (command, canary) in &variants {
            let (output, _) = f.shell(command).await;
            assert_eq!(
                std::fs::read_to_string(canary).unwrap(),
                "canary",
                "{command}: {}",
                output.content
            );
        }
        assert_eq!(std::fs::read_to_string(bystander).unwrap(), "canary");
        assert!(f.gate.asked().len() >= variants.len());
    }

    /// BUG-6 (REG-5): `y` on an outside write in auto mode writes it and nothing else outside.
    #[tokio::test]
    async fn approved_outside_write_runs_in_auto_mode() {
        let Some(f) = seatbelt(ApprovalMode::AutoWorkspace, [Decision::Approve]) else {
            return;
        };
        let target = f.outside.join("approved_canary.txt");
        let bystander = f.canary("bystander.txt");
        let command = format!("echo approved > {}", target.display());

        let (output, records) = f.shell(&command).await;

        assert!(!output.is_error, "{}", output.content);
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "approved\n");
        assert_eq!(records[0].risk, Risk::Destructive);
        assert!(records[0].sandboxed());
        assert_eq!(std::fs::read_to_string(bystander).unwrap(), "canary");
    }

    #[tokio::test]
    async fn u_escape_lifts_seatbelt_once() {
        let Some(f) = seatbelt(
            ApprovalMode::AutoWorkspace,
            [Decision::ApproveUnsandboxed, Decision::Approve],
        ) else {
            return;
        };
        if !Path::new("/usr/bin/python3").exists() {
            eprintln!("skipping: no python3");
            return;
        }
        let remove = |canary: &Path| {
            format!(
                "python3 -c \"import os; os.remove('{}')\"",
                canary.display()
            )
        };

        let canary = f.canary("u.txt");
        let (output, _) = f.shell(&remove(&canary)).await;
        assert!(!canary.exists(), "{}", output.content);

        // Next time, a plain approval keeps the sandbox.
        let canary = f.canary("u2.txt");
        let (output, _) = f.shell(&remove(&canary)).await;
        assert!(canary.exists(), "{}", output.content);
    }
}

#[tokio::test]
async fn oversized_approval_details_are_denied_instead_of_hidden() {
    let (f, _) = faked(ApprovalMode::FullAccess, [Decision::Approve], false);
    let mut request = ApprovalRequest {
        call_id: "large".into(),
        tool: "shell".into(),
        args: json!({}),
        risk: Risk::Opaque,
        preview: "x".repeat(2 * 1024 * 1024 + 1),
        sandbox: SandboxMode::Off,
        network: false,
        extra_writes: vec![],
        outside_unknown: false,
        blocked_retry: false,
        allow_rule: None,
    };
    let (events, _rx) = mpsc::channel(16);
    let cancel = CancellationToken::new();
    for forced in [false, true] {
        let (decision, source) = if forced {
            f.exec.ask_user(&request, &cancel, &events).await
        } else {
            f.exec.decide(&request, &Shell, &cancel, &events).await
        };
        assert!(matches!(decision, Decision::Deny { reason } if reason.contains("split")));
        assert_eq!(source, DecisionSource::Policy);
    }
    assert!(f.gate.asked().is_empty());
    request.preview = "visible\n".repeat(2000) + "last action";
    assert_eq!(
        f.exec.ask_user(&request, &cancel, &events).await.0,
        Decision::Approve
    );
    assert_eq!(f.gate.asked()[0].preview, request.preview);
}
