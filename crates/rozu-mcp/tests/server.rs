//! The MCP server over an in-memory stdio pair, with a fake parent rozu process on the approval
//! socket.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use rozu_core::{
    ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalRecord, ApprovalRequest, Decision,
    DecisionSource, ToolExecutor,
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
    records: mpsc::UnboundedReceiver<ApprovalRecord>,
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
        let (tx, records) = mpsc::unbounded_channel();
        let bridge = ApprovalBridge::bind(dir.path().join("approval.sock"), user.clone(), tx)
            .expect("bind the approval socket");
        Self {
            _dir: dir,
            bridge,
            user,
            records,
        }
    }

    fn socket(&self) -> PathBuf {
        self.bridge.path().to_path_buf()
    }

    fn asked(&self) -> Vec<ApprovalRequest> {
        self.user.asked.lock().unwrap().clone()
    }

    /// The next decision-log entry the server forwarded.
    async fn record(&mut self) -> ApprovalRecord {
        tokio::time::timeout(std::time::Duration::from_secs(5), self.records.recv())
            .await
            .expect("a record within 5s")
            .expect("bridge open")
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
        let workspace = tempfile::tempdir().unwrap();
        std::fs::write(workspace.path().join("README.md"), "# demo\n").unwrap();
        let ws = Workspace::new(workspace.path()).unwrap();
        let gate = SocketGate::new(socket.map(Path::to_path_buf));
        let exec = ToolExecutor::new(
            Arc::new(ToolRegistry::standard()),
            Arc::new(gate.clone()),
            Arc::new(ApprovalPolicy::new(mode)),
            Arc::new(NoSandbox),
        );
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
async fn destructive_bash_prompts_the_parent_even_in_full_access() {
    for (answer, expected) in [
        (Decision::Approve, "allow"),
        (
            Decision::Deny {
                reason: "not today".into(),
            },
            "deny",
        ),
    ] {
        let mut parent = FakeParent::start(answer);
        let mut client = Client::start(ApprovalMode::FullAccess, Some(&parent.socket()));

        let result = client
            .approve("Bash", json!({"command": "git push --force origin main"}))
            .await;
        assert_eq!(result["behavior"], expected, "{result}");
        if expected == "deny" {
            assert_eq!(result["message"], "denied by rozu: not today");
        }

        let asked = parent.asked();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].tool, "Bash");
        assert_eq!(asked[0].risk, Risk::Destructive);
        assert_eq!(asked[0].allow_prefix, None, "never offered for destructive");
        assert!(asked[0].preview.contains("git push --force"));
        let record = parent.record().await;
        assert_eq!(record.source, DecisionSource::Gate);
        assert_eq!(record.risk, Risk::Destructive);
        client.close().await;
    }
}

#[tokio::test]
async fn rozu_write_tool_goes_through_the_parent_in_read_only_mode() {
    for (answer, written) in [
        (Decision::Approve, true),
        (
            Decision::Deny {
                reason: "no".into(),
            },
            false,
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
        client.close().await;
    }
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
    client.close().await;
}

#[tokio::test]
async fn unreachable_parent_means_deny() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("nobody-listens.sock");
    for socket in [Some(gone.as_path()), None] {
        let mut client = Client::start(ApprovalMode::ReadOnly, socket);

        let answer = client
            .approve("Bash", json!({"command": "rm -rf build"}))
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
