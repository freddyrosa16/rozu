#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rozu_core::{
    ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalRecord, ApprovalRequest,
    ApprovalResolution, Decision, DecisionSource, PendingApproval, SandboxMode, StreamEvent,
    ToolExecutor,
};
use rozu_mcp::{ApprovalBridge, McpServer, SocketGate};
use rozu_tools::{NoSandbox, Risk, ToolRegistry, Workspace};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines};
use tokio::sync::mpsc;

struct ScriptedUser {
    answer: Decision,
    asked: Mutex<Vec<ApprovalRequest>>,
}

#[async_trait]
impl ApprovalGate for ScriptedUser {
    async fn decide(&self, req: ApprovalRequest) -> Decision {
        self.asked.lock().unwrap().push(req);
        self.answer.clone()
    }
}

fn deny(reason: &str) -> Decision {
    Decision::Deny {
        reason: reason.into(),
    }
}

async fn within<T>(secs: u64, fut: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(secs), fut)
        .await
        .expect("timed out")
}

/// An approval bridge on a socket in a private dir under /tmp (socket paths have a short limit).
fn bind_bridge(
    gate: Arc<dyn ApprovalGate>,
) -> (
    tempfile::TempDir,
    ApprovalBridge,
    mpsc::UnboundedReceiver<StreamEvent>,
) {
    let dir = tempfile::Builder::new()
        .prefix("rozu-test-")
        .tempdir_in("/tmp")
        .unwrap();
    let (tx, events) = mpsc::unbounded_channel();
    let bridge = ApprovalBridge::bind(dir.path().join("approval.sock"), "test-cli", gate, tx)
        .expect("bind the approval socket");
    (dir, bridge, events)
}

/// The parent rozu process.
struct FakeParent {
    _dir: tempfile::TempDir,
    bridge: ApprovalBridge,
    user: Arc<ScriptedUser>,
    events: mpsc::UnboundedReceiver<StreamEvent>,
    prompts: Vec<StreamEvent>,
}

impl FakeParent {
    fn start(answer: Decision) -> Self {
        let user = Arc::new(ScriptedUser {
            answer,
            asked: Mutex::default(),
        });
        let (dir, bridge, events) = bind_bridge(user.clone());
        Self {
            _dir: dir,
            bridge,
            user,
            events,
            prompts: Vec::new(),
        }
    }

    fn socket(&self) -> PathBuf {
        self.bridge.path().to_path_buf()
    }

    fn asked(&self) -> Vec<ApprovalRequest> {
        self.user.asked.lock().unwrap().clone()
    }

    /// The next decision-log entry the server forwarded; prompts before it go to `prompts`.
    async fn record(&mut self) -> ApprovalRecord {
        loop {
            let event = within(5, self.events.recv()).await.expect("bridge open");
            match event {
                StreamEvent::Approval(record) => return record,
                prompt => self.prompts.push(prompt),
            }
        }
    }
}

struct Client {
    _workspace: tempfile::TempDir,
    root: PathBuf,
    writer: tokio::io::WriteHalf<DuplexStream>,
    lines: Lines<BufReader<tokio::io::ReadHalf<DuplexStream>>>,
    server: tokio::task::JoinHandle<std::io::Result<()>>,
    next_id: u64,
}

impl Client {
    fn start(mode: ApprovalMode, socket: Option<&Path>) -> Self {
        Self::start_with_audit(mode, socket, None)
    }

    fn start_with_audit(
        mode: ApprovalMode,
        socket: Option<&Path>,
        audit: Option<Arc<rozu_core::AuditLog>>,
    ) -> Self {
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("README.md"), "# demo\n").unwrap();
        let ws = Workspace::new(workspace.path()).unwrap();
        let gate = SocketGate::new(socket.map(Path::to_path_buf));
        let exec = ToolExecutor::new(
            Arc::new(ToolRegistry::standard()),
            Arc::new(gate.clone()),
            Arc::new(ApprovalPolicy::new(mode)),
            Arc::new(NoSandbox),
        )
        .with_audit(audit);
        let root = ws.root().to_path_buf();
        let server = Arc::new(McpServer::new(exec, ws, gate));
        let (client_io, server_io) = tokio::io::duplex(1 << 20);
        let (server_read, server_write) = tokio::io::split(server_io);
        let server = tokio::spawn(server.serve(server_read, server_write));
        let (read, writer) = tokio::io::split(client_io);
        Self {
            _workspace: workspace,
            root,
            writer,
            lines: BufReader::new(read).lines(),
            server,
            next_id: 1,
        }
    }

    async fn send(&mut self, message: Value) {
        let mut line = message.to_string();
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await.unwrap();
    }

    async fn recv(&mut self) -> Value {
        let line = within(10, self.lines.next_line())
            .await
            .unwrap()
            .expect("server still open");
        serde_json::from_str(&line).unwrap()
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))
            .await;
        let response = self.recv().await;
        assert_eq!(response["id"], id, "{response}");
        response
    }

    async fn call(&mut self, name: &str, arguments: Value) -> Value {
        let response = self
            .request("tools/call", json!({"name": name, "arguments": arguments}))
            .await;
        response["result"].clone()
    }

    async fn approve(&mut self, tool_name: &str, input: Value) -> Value {
        let result = self
            .call(
                "approve",
                json!({"tool_name": tool_name, "input": input, "tool_use_id": "toolu_1"}),
            )
            .await;
        assert_eq!(result["isError"], false, "{result}");
        serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap()
    }

    /// Closes stdin like the CLI does; the server must stop on its own.
    async fn close(mut self) {
        self.writer.shutdown().await.unwrap();
        within(10, self.server).await.unwrap().unwrap();
    }
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
}

fn message(answer: &Value) -> &str {
    answer["message"].as_str().unwrap()
}

#[tokio::test]
async fn initialize_list_and_call_round_trip() {
    let mut client = Client::start(ApprovalMode::AutoWorkspace, None);

    let init = client
        .request(
            "initialize",
            json!({"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "claude-code", "version": "test"}}),
        )
        .await;
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(init["result"]["protocolVersion"], "2025-06-18");
    assert_eq!(init["result"]["serverInfo"]["name"], "rozu");
    assert!(init["result"]["capabilities"]["tools"].is_object());
    // Notifications get no response; the next line answers the next request.
    client
        .send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
        .await;

    let list = client.request("tools/list", json!({})).await;
    let names: Vec<_> = list["result"]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(
        names,
        [
            "approve",
            "apply_patch",
            "grep",
            "list_dir",
            "read_file",
            "shell",
            "write_file"
        ]
    );
    for tool in list["result"]["tools"].as_array().unwrap() {
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
    }

    let read = client.call("read_file", json!({"path": "README.md"})).await;
    assert_eq!(read["isError"], false, "{read}");
    assert!(text(&read).contains("# demo"), "{read}");

    let outside = client
        .call("read_file", json!({"path": "/etc/hosts"}))
        .await;
    assert_eq!(outside["isError"], true, "{outside}");

    let unknown = client.request("tools/call", json!({"name": "nope"})).await;
    assert_eq!(unknown["error"]["code"], -32602);
    let missing = client.request("resources/list", json!({})).await;
    assert_eq!(missing["error"]["code"], -32601);
    assert_eq!(client.request("ping", json!({})).await["result"], json!({}));

    client.send(json!({"not": "json-rpc"})).await;
    assert_eq!(client.recv().await["error"]["code"], -32600);
    client.close().await;
}

#[tokio::test]
async fn initialize_answers_with_a_supported_version() {
    let mut client = Client::start(ApprovalMode::AutoWorkspace, None);
    let init = client
        .request("initialize", json!({"protocolVersion": "1999-01-01"}))
        .await;
    assert_eq!(
        init["result"]["protocolVersion"],
        rozu_mcp::PROTOCOL_VERSION
    );
    client.close().await;
}

#[tokio::test]
async fn approve_allows_reads_and_rozu_tools_without_asking() {
    let parent = FakeParent::start(deny("should not be asked"));
    let mut client = Client::start(ApprovalMode::AutoWorkspace, Some(&parent.socket()));

    let read = client
        .approve("Read", json!({"file_path": "README.md"}))
        .await;
    assert_eq!(
        read,
        json!({"behavior": "allow", "updatedInput": {"file_path": "README.md"}})
    );
    let rozu_tool = client
        .approve("mcp__rozu__shell", json!({"command": "rm -rf /"}))
        .await;
    assert_eq!(rozu_tool["behavior"], "allow", "gated when the tool runs");
    assert!(parent.asked().is_empty());
    client.close().await;
}

#[tokio::test]
async fn approve_denies_unmappable_requests_and_logs_them() {
    let mut parent = FakeParent::start(Decision::Approve);
    let mut client = Client::start(ApprovalMode::FullAccess, Some(&parent.socket()));

    let answer = client.approve("TeleportFiles", json!({"to": "mars"})).await;
    assert_eq!(answer["behavior"], "deny");
    assert!(message(&answer).contains("can't map"), "{answer}");
    assert!(parent.asked().is_empty(), "default-deny doesn't ask");
    let record = parent.record().await;
    assert_eq!(record.tool, "TeleportFiles");
    assert_eq!(record.call_id, "toolu_1");
    assert_eq!(record.source, DecisionSource::Policy);
    assert!(matches!(record.decision, Decision::Deny { .. }));
    client.close().await;
}

#[tokio::test]
async fn claude_bash_is_denied_in_favor_of_the_sandboxed_shell() {
    let mut parent = FakeParent::start(Decision::Approve);
    let mut client = Client::start(ApprovalMode::FullAccess, Some(&parent.socket()));

    let result = client.approve("Bash", json!({"command": "ls"})).await;
    assert_eq!(result["behavior"], "deny", "{result}");
    assert!(message(&result).contains("mcp__rozu__shell"), "{result}");
    assert!(parent.asked().is_empty(), "denied without asking");
    let record = parent.record().await;
    assert_eq!(record.tool, "Bash");
    assert_eq!(record.source, DecisionSource::Policy);
    client.close().await;
}

/// BUG-5 through MCP: rozu's shell with interpreter code asks even in full access.
#[tokio::test]
async fn interpreter_code_prompts_the_parent_even_in_full_access() {
    for (answer, ran) in [(Decision::Approve, true), (deny("not today"), false)] {
        let mut parent = FakeParent::start(answer);
        let mut client = Client::start(ApprovalMode::FullAccess, Some(&parent.socket()));

        let result = client
            .call("shell", json!({"command": "eval 'echo ran > ran.txt'"}))
            .await;
        assert_eq!(client.root.join("ran.txt").exists(), ran, "{result}");
        if !ran {
            assert_eq!(text(&result), "denied: not today");
        }
        let asked = parent.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].risk, Risk::Opaque);
        assert_eq!(asked[0].allow_rule, None, "never offered for inline code");
        let record = parent.record().await;
        assert_eq!(record.source, DecisionSource::Gate);
        client.close().await;
    }
}

/// BUG-7: decisions made inside `rozu mcp-serve` are in the audit log too.
#[tokio::test]
async fn mcp_decisions_go_to_the_audit_log() {
    let mut parent = FakeParent::start(deny("nope"));
    let logs = tempfile::tempdir().unwrap();
    let log_path = logs.path().join("approvals.jsonl");
    let audit = Arc::new(rozu_core::AuditLog::open(&log_path, "chat-7").unwrap());
    let mut client = Client::start_with_audit(
        ApprovalMode::AutoWorkspace,
        Some(&parent.socket()),
        Some(audit),
    );

    // Auto-approved by the mode, denied by policy, and asked (then denied by the user).
    client
        .call("write_file", json!({"path": "a.txt", "content": "a"}))
        .await;
    client.approve("TeleportFiles", json!({})).await;
    client
        .approve("WebFetch", json!({"url": "https://x.dev"}))
        .await;
    for _ in 0..3 {
        parent.record().await;
    }
    client.close().await;

    let entries: Vec<Value> = std::fs::read_to_string(&log_path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    let decided_by: Vec<_> = entries
        .iter()
        .map(|e| e["decided_by"].as_str().unwrap())
        .collect();
    assert_eq!(decided_by, ["mode", "policy", "user"]);
    assert!(entries.iter().all(|e| e["session_id"] == "chat-7"));
    assert_eq!(entries[2]["decision"], "deny");
    assert_eq!(entries[2]["reason"], "nope");
}

#[tokio::test]
async fn rozu_write_tool_goes_through_the_parent_in_read_only_mode() {
    for (answer, written, resolution) in [
        (Decision::Approve, true, ApprovalResolution::Approved),
        (deny("no"), false, ApprovalResolution::Denied),
    ] {
        let mut parent = FakeParent::start(answer);
        let mut client = Client::start(ApprovalMode::ReadOnly, Some(&parent.socket()));

        let result = client
            .call("write_file", json!({"path": "notes.md", "content": "hi\n"}))
            .await;
        assert_eq!(result["isError"], !written, "{result}");
        assert_eq!(client.root.join("notes.md").exists(), written);
        let asked = parent.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].tool, "write_file");
        assert_eq!(asked[0].risk, Risk::WriteWorkspace);
        let record = parent.record().await;
        assert_eq!(record.tool, "write_file");
        assert_eq!(record.source, DecisionSource::Gate);
        let id = asked[0].call_id.clone();
        assert_eq!(
            parent.prompts,
            [
                StreamEvent::ApprovalPending(PendingApproval {
                    request_id: id.clone(),
                    tool: "write_file".into(),
                    summary: asked[0].preview.lines().next().unwrap().trim().into(),
                    provider: "test-cli".into(),
                }),
                StreamEvent::ApprovalResolved {
                    request_id: id,
                    resolution,
                },
            ]
        );
        client.close().await;
    }
}

#[tokio::test]
async fn withdrawn_prompts_expire() {
    struct NeverAnswers;
    #[async_trait]
    impl ApprovalGate for NeverAnswers {
        async fn decide(&self, _req: ApprovalRequest) -> Decision {
            std::future::pending().await
        }
    }
    let (_dir, bridge, mut events) = bind_bridge(Arc::new(NeverAnswers));
    let gate = SocketGate::new(Some(bridge.path().to_path_buf()));
    let call = tokio::spawn(async move {
        gate.decide(ApprovalRequest {
            call_id: "call-1".into(),
            tool: "write_file".into(),
            args: json!({"path": "x.txt"}),
            risk: Risk::WriteWorkspace,
            preview: "x.txt".into(),
            sandbox: SandboxMode::InProcess,
            network: false,
            extra_writes: Vec::new(),
            outside_unknown: false,
            blocked_retry: false,
            allow_rule: None,
        })
        .await
    });
    let StreamEvent::ApprovalPending(pending) = within(5, events.recv()).await.unwrap() else {
        panic!("expected a pending prompt");
    };
    assert_eq!(
        (pending.tool.as_str(), pending.summary.as_str()),
        ("write_file", "x.txt")
    );
    // `rozu mcp-serve` hanging up (the CLI was cancelled) withdraws the open prompt.
    call.abort();
    let _ = call.await;
    assert_eq!(
        within(5, events.recv()).await.unwrap(),
        StreamEvent::ApprovalResolved {
            request_id: pending.request_id,
            resolution: ApprovalResolution::Expired,
        }
    );
}

#[tokio::test]
async fn auto_mode_writes_in_the_workspace_without_asking() {
    let mut parent = FakeParent::start(deny("should not be asked"));
    let mut client = Client::start(ApprovalMode::AutoWorkspace, Some(&parent.socket()));
    let result = client
        .call("write_file", json!({"path": "a.txt", "content": "a"}))
        .await;
    assert_eq!(result["isError"], false, "{result}");
    assert!(parent.asked().is_empty());
    let record = parent.record().await;
    assert_eq!(
        record.source,
        DecisionSource::Mode(ApprovalMode::AutoWorkspace)
    );
    assert!(parent.prompts.is_empty(), "auto-decisions open no prompt");
    client.close().await;
}

#[tokio::test]
async fn unreachable_parent_means_deny() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("nobody-listens.sock");
    for socket in [Some(gone.as_path()), None] {
        let mut client = Client::start(ApprovalMode::ReadOnly, socket);

        let answer = client
            .approve("WebFetch", json!({"url": "https://example.com"}))
            .await;
        assert_eq!(answer["behavior"], "deny", "{answer}");
        assert!(message(&answer).contains("denied by default"), "{answer}");

        let write = client
            .call("write_file", json!({"path": "x.txt", "content": "x"}))
            .await;
        assert_eq!(write["isError"], true);
        assert!(text(&write).starts_with("denied:"), "{write}");
        assert!(!client.root.join("x.txt").exists());
        client.close().await;
    }
}

#[tokio::test]
async fn reads_outside_the_workspace_always_ask() {
    let parent = FakeParent::start(Decision::Deny {
        reason: "private".into(),
    });
    let mut client = Client::start(ApprovalMode::FullAccess, Some(&parent.socket()));
    let answer = client
        .approve("Read", json!({"file_path": "/etc/hosts"}))
        .await;
    assert_eq!(answer["behavior"], "deny");
    assert_eq!(parent.asked().len(), 1);
    assert_eq!(parent.asked()[0].risk, Risk::Read);
    client.close().await;
}
