#![allow(dead_code)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rozu_core::{
    Agent, AgentConfig, AgentEvent, ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalRecord,
    ApprovalRequest, AuditLog, ChatRequest, Decision, DecisionSource, EventStream, Message,
    ModelInfo, Provider, ProviderError, SandboxMode, Session, StreamEvent, ToolExecutor,
    TurnOutcome,
};
use rozu_mcp::SocketGate;
use rozu_tools::{NoSandbox, Risk, ToolRegistry, Workspace};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const TIMEOUT: Duration = Duration::from_secs(10);

/// A temp dir holding a copy of `tests/fixtures/<cli>/fake_<cli>.sh` as `<cli>`.
pub struct Fake {
    dir: tempfile::TempDir,
    cli: &'static str,
}

impl Fake {
    pub fn new(cli: &'static str, subdirs: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in subdirs {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let fake = Self { dir, cli };
        let bin = fake.binary();
        std::fs::copy(fake.fixture(&format!("fake_{cli}.sh")), &bin).unwrap();
        make_executable(&bin);
        fake
    }

    /// For the script-driven fakes: `script.txt` is `scripts` concatenated.
    pub fn scripted(cli: &'static str, scripts: &[&str]) -> Self {
        let fake = Self::new(cli, &["log", "workspace"]);
        fake.write("workspace/README.md", "# demo\n");
        let script: String = scripts.iter().map(|s| fake.read_fixture(s)).collect();
        fake.write("script.txt", &script);
        fake
    }

    pub fn fixture(&self, name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(self.cli)
            .join(name)
    }

    fn read_fixture(&self, name: &str) -> String {
        std::fs::read_to_string(self.fixture(name)).unwrap()
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn binary(&self) -> PathBuf {
        self.path(self.cli)
    }

    pub fn workspace(&self) -> Workspace {
        Workspace::new(self.path("workspace")).unwrap()
    }

    pub fn write(&self, rel: &str, contents: &str) {
        std::fs::write(self.path(rel), contents).unwrap();
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap()
    }

    pub fn jsonl(&self, rel: &str) -> Vec<Value> {
        std::fs::read_to_string(self.path(rel))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    /// argv of each start of the fake.
    pub fn starts(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.path("log/args.txt"))
            .unwrap_or_default()
            .split("--\n")
            .filter(|s| !s.is_empty())
            .map(|s| s.lines().map(str::to_owned).collect())
            .collect()
    }

    pub fn sent(&self) -> Vec<Value> {
        self.jsonl("log/stdin.jsonl")
    }

    pub fn sent_method(&self, method: &str) -> Vec<Value> {
        self.sent()
            .into_iter()
            .filter(|m| m["method"] == method)
            .collect()
    }

    /// rozu's answer to the fake's request `id`.
    pub fn reply(&self, id: &str) -> Value {
        self.sent()
            .into_iter()
            .find(|m| m["id"] == id && m.get("method").is_none())
            .unwrap_or_else(|| panic!("no reply to {id}: {:?}", self.sent()))
    }

    pub fn audit(&self) -> Vec<Value> {
        self.jsonl("approvals.jsonl")
    }

    pub fn env(&self, prefix: &str) -> Vec<(String, String)> {
        vec![
            (
                format!("{prefix}_SCRIPT"),
                self.path("script.txt").display().to_string(),
            ),
            (
                format!("{prefix}_LOG"),
                self.path("log").display().to_string(),
            ),
        ]
    }

    pub fn exec(&self, gate: Arc<dyn ApprovalGate>, mode: ApprovalMode) -> ToolExecutor {
        let audit = AuditLog::open(self.path("approvals.jsonl"), "s-test").unwrap();
        ToolExecutor::new(
            Arc::new(ToolRegistry::standard()),
            gate,
            Arc::new(ApprovalPolicy::new(mode)),
            Arc::new(NoSandbox),
        )
        .with_audit(Some(Arc::new(audit)))
        .with_provider(Some(self.cli.into()))
    }

    /// Runs the fake with `vars` set (tests can't change their own environment).
    pub fn wrapper(&self, vars: &[(String, String)]) -> PathBuf {
        let path = self.path(&format!("{}-wrapper", self.cli));
        let exports: String = vars.iter().map(|(k, v)| format!("{k}='{v}' ")).collect();
        let script = format!(
            "#!/bin/sh\n{exports}exec '{}' \"$@\"\n",
            self.binary().display()
        );
        std::fs::write(&path, script).unwrap();
        make_executable(&path);
        path
    }
}

fn make_executable(path: &Path) {
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

pub fn request(model: &str, messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages,
        tools: Vec::new(),
        max_output_tokens: None,
    }
}

pub async fn start(
    provider: &impl Provider,
    model: &str,
    cancel: &CancellationToken,
) -> EventStream {
    provider
        .stream(request(model, vec![Message::user("one")]), cancel.clone())
        .await
        .unwrap()
}

pub async fn start_err(provider: &impl Provider, model: &str) -> ProviderError {
    provider
        .stream(
            request(model, vec![Message::user("hi")]),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("an error")
}

pub async fn next(stream: &mut EventStream) -> Option<Result<StreamEvent, ProviderError>> {
    tokio::time::timeout(TIMEOUT, stream.next()).await.unwrap()
}

pub async fn collect(
    provider: &impl Provider,
    req: ChatRequest,
) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let mut stream = provider
        .stream(req, CancellationToken::new())
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(item) = next(&mut stream).await {
        match item {
            Ok(event) => events.push(event),
            Err(err) => return (events, Some(err)),
        }
    }
    (events, None)
}

pub fn approvals(events: &[StreamEvent]) -> Vec<&ApprovalRecord> {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Approval(record) => Some(record),
            _ => None,
        })
        .collect()
}

/// Expects `first` as the turn's first text, then cancels and expects `Cancelled`.
pub async fn cancel_after(stream: &mut EventStream, cancel: &CancellationToken, first: &str) {
    assert_eq!(
        next(stream).await.unwrap().unwrap(),
        StreamEvent::TextDelta(first.into())
    );
    cancel.cancel();
    assert_eq!(next(stream).await.unwrap(), Err(ProviderError::Cancelled));
}

pub fn gate_record(
    call_id: &str,
    risk: Risk,
    preview: &str,
    network: bool,
    reason: &str,
) -> ApprovalRecord {
    ApprovalRecord {
        call_id: call_id.into(),
        tool: "shell".into(),
        risk,
        preview: preview.into(),
        sandbox: SandboxMode::Enforced("seatbelt"),
        network,
        extra_writes: Vec::new(),
        blocked_retry: false,
        decision: Decision::Deny {
            reason: reason.into(),
        },
        source: DecisionSource::Gate,
    }
}

pub fn socket_gate(runtime_dir: &Path) -> SocketGate {
    SocketGate::new(Some(runtime_dir.join("approval.sock")))
}

/// What `rozu mcp-serve` sends over the approval socket shows up on the stream.
pub async fn assert_mcp_record_joins(stream: &mut EventStream, runtime_dir: &Path, preview: &str) {
    let record = gate_record("mcp-1", Risk::Destructive, preview, false, "no");
    socket_gate(runtime_dir).send_record(record.clone()).await;
    assert_eq!(
        next(stream).await.unwrap(),
        Ok(StreamEvent::Approval(record))
    );
}

/// A user who gives `answer` to each question once released.
pub struct HeldUser {
    pub release: tokio::sync::Notify,
    answer: Decision,
}

impl HeldUser {
    pub fn new(answer: Decision) -> Arc<Self> {
        Arc::new(Self {
            release: tokio::sync::Notify::new(),
            answer,
        })
    }
}

#[async_trait::async_trait]
impl ApprovalGate for HeldUser {
    async fn decide(&self, _req: ApprovalRequest) -> Decision {
        self.release.notified().await;
        self.answer.clone()
    }
}

pub struct AgentTurn {
    pub outcome: TurnOutcome,
    pub events: Vec<AgentEvent>,
    pub session: Session,
}

pub async fn run_agent_turn(
    provider: impl Provider + 'static,
    workspace: Workspace,
    model: ModelInfo,
    prompt: &str,
) -> AgentTurn {
    let config = AgentConfig::new(model.id.clone()).with_model_info(Some(model));
    let agent = Agent::new(
        Arc::new(provider),
        Arc::new(ToolRegistry::standard()),
        workspace,
        config,
    );
    let mut session = Session::with_system_prompt("You are rozu.");
    let (tx, mut rx) = mpsc::channel(64);
    let outcome = agent
        .run_turn(prompt, &mut session, &tx, CancellationToken::new())
        .await
        .unwrap();
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    AgentTurn {
        outcome,
        events,
        session,
    }
}
