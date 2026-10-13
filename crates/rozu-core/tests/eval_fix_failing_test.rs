//! Eval-style task: "fix a failing test in a temp repo".
//!
//! A scripted provider plays the model: it runs the test (fails), reads the code, patches the
//! bug with `apply_patch`, reruns the test through `shell` (passes) and answers. Everything
//! below the provider is real: the agent loop, the approval policy in auto mode, the tools and,
//! on macOS when available, the Seatbelt sandbox.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use rozu_core::{
    Agent, AgentConfig, AgentEvent, ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalRequest,
    ChatRequest, ContentPart, Decision, DecisionSource, EventStream, Provider, ProviderResult,
    Session, StopReason, StreamEvent, TurnOutcome,
};
use rozu_tools::{NoSandbox, Sandbox, Seatbelt, ToolRegistry, Workspace, platform_sandbox};
use serde_json::json;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

struct ScriptedProvider {
    steps: Mutex<VecDeque<Vec<StreamEvent>>>,
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    async fn stream(&self, _: ChatRequest, _: CancellationToken) -> ProviderResult<EventStream> {
        let events = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .expect("provider called more often than scripted");
        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

fn call(id: &str, name: &str, args: serde_json::Value) -> Vec<StreamEvent> {
    vec![
        StreamEvent::ToolCallStart {
            id: id.into(),
            name: name.into(),
        },
        StreamEvent::ToolCallArgsDelta {
            id: id.into(),
            json: args.to_string(),
        },
        StreamEvent::ToolCallEnd { id: id.into() },
        StreamEvent::Done(StopReason::ToolUse),
    ]
}

/// Approves everything it's asked and remembers what that was.
#[derive(Default)]
struct RecordingGate(Mutex<Vec<ApprovalRequest>>);

#[async_trait]
impl ApprovalGate for RecordingGate {
    async fn decide(&self, req: ApprovalRequest) -> Decision {
        self.0.lock().unwrap().push(req);
        Decision::Approve
    }
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

/// Seatbelt if this machine can apply it (not when already sandboxed), else no sandbox.
fn sandbox() -> Arc<dyn Sandbox> {
    let sandbox = platform_sandbox();
    if sandbox.enforced() {
        let usable = std::process::Command::new(rozu_tools::SANDBOX_EXEC)
            .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
            .status()
            .is_ok_and(|s| s.success());
        if usable {
            return Arc::new(Seatbelt);
        }
    }
    Arc::new(NoSandbox)
}

#[tokio::test]
async fn agent_fixes_a_failing_test() {
    let repo = tempfile::tempdir().unwrap();
    std::fs::write(repo.path().join("math.sh"), MATH_SH).unwrap();
    std::fs::write(repo.path().join("test.sh"), TEST_SH).unwrap();
    let workspace = Workspace::new(repo.path()).unwrap();

    let diff = "--- a/math.sh\n+++ b/math.sh\n@@ -1,3 +1,3 @@\n add() {\n-  echo $(($1 - $2))\n+  echo $(($1 + $2))\n }\n";
    let provider = Arc::new(ScriptedProvider {
        steps: Mutex::new(VecDeque::from([
            call("c1", "shell", json!({"command": "sh test.sh"})),
            call("c2", "read_file", json!({"path": "math.sh"})),
            call(
                "c3",
                "apply_patch",
                json!({"path": "math.sh", "diff": diff}),
            ),
            call("c4", "shell", json!({"command": "sh test.sh"})),
            vec![
                StreamEvent::TextDelta("Fixed: add() subtracted instead of adding.".into()),
                StreamEvent::Done(StopReason::EndTurn),
            ],
        ])),
    });
    let sandbox = sandbox();
    let sandboxed = sandbox.enforced();
    let gate = Arc::new(RecordingGate::default());
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
    let (tx, mut rx) = mpsc::channel(64);
    let collect = tokio::spawn(async move {
        let mut events = Vec::new();
        while let Some(event) = rx.recv().await {
            events.push(event);
        }
        events
    });
    let outcome = agent
        .run_turn(
            "test.sh fails; fix it",
            &mut session,
            &tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    drop(tx);
    let events = collect.await.unwrap();

    assert_eq!(outcome, TurnOutcome::Completed);
    let results: Vec<(String, bool)> = session
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|part| match part {
            ContentPart::ToolResult {
                output, is_error, ..
            } => Some((output.clone(), *is_error)),
            _ => None,
        })
        .collect();
    assert_eq!(results.len(), 4, "{results:#?}");
    let (first_run, failed) = &results[0];
    assert!(
        *failed && first_run.contains("FAIL: add 2 3 = -1"),
        "{first_run}"
    );
    let (patch, patch_failed) = &results[2];
    assert!(!patch_failed, "{patch}");
    let (second_run, failed) = &results[3];
    assert!(!failed, "{second_run}");
    assert!(second_run.starts_with("exit code: 0") && second_run.contains("PASS"));

    // The fix is on disk and the test passes outside the agent too.
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
    let asked = gate.0.lock().unwrap().clone();
    assert!(asked.iter().all(|r| r.tool == "shell"), "{asked:#?}");
    assert_eq!(asked.len(), if sandboxed { 0 } else { 2 });
    let decisions: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Approval(record) => Some((record.tool.as_str(), record.source.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(decisions.len(), 4);
    assert!(decisions.contains(&(
        "apply_patch",
        DecisionSource::Mode(ApprovalMode::AutoWorkspace)
    )));
}
