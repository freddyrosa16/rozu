//! Eval-style "fix a failing test" task: only the model is scripted; the agent loop, auto-mode
//! policy, tools and (on macOS, when usable) Seatbelt are real.

mod common;

use std::sync::Arc;

use common::*;
use rozu_core::{
    Agent, AgentConfig, ApprovalMode, ApprovalPolicy, Decision, DecisionSource, Session,
    TurnOutcome,
};
use rozu_tools::{NoSandbox, Sandbox, Seatbelt, ToolRegistry, Workspace, platform_sandbox};
use serde_json::{Value, json};

fn call(id: &str, name: &str, args: Value) -> Script {
    tool_step(&[(id, name, &args.to_string())])
}

const MATH_SH: &str = "add() {\n  echo $(($1 - $2))\n}\n";
const TEST_SH: &str = r#". ./math.sh
result=$(add 2 3)
if [ "$result" = "5" ]; then
  echo "PASS"
else
  echo "FAIL: add 2 3 = $result, want 5"
  exit 1
fi
"#;

fn sandbox() -> Arc<dyn Sandbox> {
    if platform_sandbox().enforced() && seatbelt_usable() {
        Arc::new(Seatbelt)
    } else {
        Arc::new(NoSandbox)
    }
}

#[tokio::test]
async fn agent_fixes_a_failing_test() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("math.sh"), MATH_SH).unwrap();
    std::fs::write(repo.path().join("test.sh"), TEST_SH).unwrap();
    let workspace = Workspace::new(repo.path()).unwrap();

    let diff = "--- a/math.sh\n+++ b/math.sh\n@@ -1,3 +1,3 @@\n add() {\n-  echo $(($1 - $2))\n+  echo $(($1 + $2))\n }\n";
    let provider = ScriptedProvider::new([
        call("c1", "shell", json!({"command": "sh test.sh"})),
        call("c2", "read_file", json!({"path": "math.sh"})),
        call(
            "c3",
            "apply_patch",
            json!({"path": "math.sh", "diff": diff}),
        ),
        call("c4", "shell", json!({"command": "sh test.sh"})),
        reply("Fixed: add() subtracted instead of adding."),
    ]);
    let sandbox = sandbox();
    let sandboxed = sandbox.enforced();
    let gate = ScriptedGate::always(Decision::Approve);
    let policy =
        ApprovalPolicy::for_project(ApprovalMode::AutoWorkspace, workspace.root()).unwrap();
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::standard()),
        workspace.clone(),
        AgentConfig::new("scripted"),
    )
    .with_approval_policy(Arc::new(policy))
    .with_approval_gate(gate.clone())
    .with_sandbox(sandbox);

    let mut session = Session::new();
    let (outcome, events) =
        run_turn(&agent, &mut session, "test.sh fails; fix it", |_, _| {}).await;

    assert_eq!(outcome.unwrap(), TurnOutcome::Completed);
    let results = tool_results(&session);
    assert_eq!(results.len(), 4, "{results:#?}");
    let (_, first_run, failed) = &results[0];
    assert!(
        *failed && first_run.contains("FAIL: add 2 3 = -1"),
        "{first_run}"
    );
    let (_, patch, patch_failed) = &results[2];
    assert!(!patch_failed, "{patch}");
    let (_, second_run, failed) = &results[3];
    assert!(!failed, "{second_run}");
    assert!(second_run.starts_with("exit code: 0") && second_run.contains("PASS"));

    assert!(
        std::fs::read_to_string(repo.path().join("math.sh"))
            .unwrap()
            .contains("$1 + $2")
    );
    let status = std::process::Command::new("/bin/sh")
        .arg("test.sh")
        .current_dir(repo.path())
        .output()
        .unwrap();
    assert!(status.status.success());

    // Auto mode: the patch never needed a prompt; with a sandbox, neither did the test runs.
    let asked = gate.asked();
    assert!(asked.iter().all(|r| r.tool == "shell"), "{asked:#?}");
    assert_eq!(asked.len(), if sandboxed { 0 } else { 2 });
    let decisions: Vec<_> = approvals(&events)
        .into_iter()
        .map(|record| (record.tool, record.source))
        .collect();
    assert_eq!(decisions.len(), 4);
    assert!(decisions.contains(&(
        "apply_patch".to_owned(),
        DecisionSource::Mode(ApprovalMode::AutoWorkspace)
    )));
}
