//! The MCP server over an in-memory stdio pair, with a fake parent rozu process on the approval
//! socket.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

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

/// The user in the parent process: answers every prompt with `answer` and remembers them.
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

/// The parent rozu process: an approval bridge on a socket in a private temp dir.
struct FakeParent {
    _dir: tempfile::TempDir,
    bridge: ApprovalBridge,
    user: Arc<ScriptedUser>,
    events: mpsc::UnboundedReceiver<StreamEvent>,
    /// Approval prompts the bridge reported, in order.
    prompts: Vec<StreamEvent>,
}

impl FakeParent {
    fn start(answer: Decision) -> Self {
        let dir = tempfile::Builder::new()
            .prefix("rozu-test-")
            .tempdir_in("/tmp")
            .unwrap();
        let user = Arc::new(ScriptedUser {
            answer,
            asked: Mutex::default(),
        });
        let (tx, events) = mpsc::unbounded_channel();
        let bridge = ApprovalBridge::bind(
            dir.path().join("approval.sock"),
            "test-cli",
            user.clone(),
            tx,
        )
        .expect("bind the approval socket");
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
            let event = tokio::time::timeout(std::time::Duration::from_secs(5), self.events.recv())
                .await
                .expect("a record within 5s")
                .expect("bridge open");
            match event {
                StreamEvent::Approval(record) => return record,
                prompt => self.prompts.push(prompt),
            }
        }
    }
}

/// An MCP client talking to a server task over an in-memory pipe.
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
        Self::start_inner(mode, socket, None)
    }

    fn start_with_audit(
        mode: ApprovalMode,
        socket: Option<&Path>,
        audit: Arc<rozu_core::AuditLog>,
    ) -> Self {
        Self::start_inner(mode, socket, Some(audit))
    }

    fn start_inner(
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
        let line = tokio::time::timeout(std::time::Duration::from_secs(10), self.lines.next_line())
            .await
            .expect("a response within 10s")
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

    /// The parsed answer of the `approve` tool.
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

    /// Ends the input (like the CLI closing stdin) and waits for the server to finish.
    async fn close(mut self) {
        self.writer.shutdown().await.unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(10), self.server)
            .await
            .expect("server stops at end of input")
            .unwrap()
            .unwrap();
    }
}

fn text(result: &Value) -> &str {
    result["content"][0]["text"].as_str().unwrap()
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

    // Workspace confinement still applies.
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
    let parent = FakeParent::start(Decision::Deny {
        reason: "should not be asked".into(),
    });
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
    assert!(
        answer["message"].as_str().unwrap().contains("can't map"),
        "{answer}"
    );
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
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .contains("mcp__rozu__shell"),
        "{result}"
    );
    assert!(parent.asked().is_empty(), "denied without asking");
    let record = parent.record().await;
    assert_eq!(record.tool, "Bash");
    assert_eq!(record.source, DecisionSource::Policy);
    client.close().await;
}

/// BUG-5 through MCP: rozu's shell with interpreter code asks even in full access.
#[tokio::test]
async fn interpreter_code_prompts_the_parent_even_in_full_access() {
    for (answer, ran) in [
        (Decision::Approve, true),
        (
            Decision::Deny {
                reason: "not today".into(),
            },
            false,
        ),
    ] {
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
    let mut parent = FakeParent::start(Decision::Deny {
        reason: "nope".into(),
    });
    let logs = tempfile::tempdir().unwrap();
    let log_path = logs.path().join("approvals.jsonl");
    let audit = Arc::new(rozu_core::AuditLog::open(&log_path, "chat-7").unwrap());
    let mut client =
        Client::start_with_audit(ApprovalMode::AutoWorkspace, Some(&parent.socket()), audit);

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
        (
            Decision::Deny {
                reason: "no".into(),
            },
            false,
            ApprovalResolution::Denied,
        ),
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
        // The parent reported the forwarded prompt while it was open, then how it ended.
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

/// A prompt withdrawn because `rozu mcp-serve` hung up (the CLI was cancelled) expires.
#[tokio::test]
async fn withdrawn_prompts_expire() {
    struct NeverAnswers;
    #[async_trait]
    impl ApprovalGate for NeverAnswers {
        async fn decide(&self, _req: ApprovalRequest) -> Decision {
            std::future::pending().await
        }
    }
    let dir = tempfile::Builder::new()
        .prefix("rozu-test-")
        .tempdir_in("/tmp")
        .unwrap();
    let (tx, mut events) = mpsc::unbounded_channel();
    let bridge = ApprovalBridge::bind(
        dir.path().join("approval.sock"),
        "test-cli",
        Arc::new(NeverAnswers),
        tx,
    )
    .unwrap();
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
    async fn next(events: &mut mpsc::UnboundedReceiver<StreamEvent>) -> StreamEvent {
        tokio::time::timeout(std::time::Duration::from_secs(5), events.recv())
            .await
            .unwrap()
            .unwrap()
    }
    let StreamEvent::ApprovalPending(pending) = next(&mut events).await else {
        panic!("expected a pending prompt");
    };
    assert_eq!(
        (pending.tool.as_str(), pending.summary.as_str()),
        ("write_file", "x.txt")
    );
    // `rozu mcp-serve` goes away with the question still open.
    call.abort();
    let _ = call.await;
    assert_eq!(
        next(&mut events).await,
        StreamEvent::ApprovalResolved {
            request_id: pending.request_id,
            resolution: ApprovalResolution::Expired,
        }
    );
}

#[tokio::test]
async fn auto_mode_writes_in_the_workspace_without_asking() {
    let mut parent = FakeParent::start(Decision::Deny {
        reason: "should not be asked".into(),
    });
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
        assert!(
            answer["message"]
                .as_str()
                .unwrap()
                .contains("denied by default"),
            "{answer}"
        );

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
