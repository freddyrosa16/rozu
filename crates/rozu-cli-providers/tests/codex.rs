//! `CodexAppServerProvider` against a fake `codex` binary that follows scripted JSON-RPC
//! exchanges. No real Codex CLI is run.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rozu_cli_providers::codex::install::{
    LoginStatus, codex_login_status, codex_version, list_models,
};
use rozu_cli_providers::{CodexAppServerProvider, CodexConfig};
use rozu_core::{
    Agent, AgentConfig, AgentEvent, ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalRecord,
    ApprovalRequest, AuditLog, ChatRequest, Decision, DecisionSource, Message, ModelInfo, Provider,
    ProviderError, SandboxMode, Session, StopReason, StreamEvent, ToolExecutor, TurnOutcome, Usage,
};
use rozu_mcp::SocketGate;
use rozu_tools::{NoSandbox, Risk, ToolRegistry, Workspace};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/codex");

/// Answers like a user: approves what `approve` accepts (with `u`, the strongest answer on the
/// card), denies the rest, and remembers what it was asked.
struct TestGate {
    asked: Mutex<Vec<ApprovalRequest>>,
    approve: fn(&ApprovalRequest) -> bool,
}

impl TestGate {
    fn new(approve: fn(&ApprovalRequest) -> bool) -> Arc<Self> {
        Arc::new(Self {
            asked: Mutex::default(),
            approve,
        })
    }

    fn asked(&self) -> Vec<ApprovalRequest> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl ApprovalGate for TestGate {
    async fn decide(&self, request: ApprovalRequest) -> Decision {
        let approve = (self.approve)(&request);
        self.asked.lock().unwrap().push(request);
        if approve {
            Decision::ApproveUnsandboxed
        } else {
            Decision::Deny {
                reason: "not this one".into(),
            }
        }
    }
}

/// The fake codex, its script, log directory and a workspace.
struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    /// The fake app-server follows `scripts` (fixture names), concatenated.
    fn new(scripts: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["log", "workspace"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        std::fs::write(dir.path().join("workspace/README.md"), "# demo\n").unwrap();
        let bin = dir.path().join("codex");
        std::fs::copy(Path::new(FIXTURES).join("fake_codex.sh"), &bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script: String = scripts
            .iter()
            .map(|name| std::fs::read_to_string(Path::new(FIXTURES).join(name)).unwrap())
            .collect();
        std::fs::write(dir.path().join("script.txt"), script).unwrap();
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn workspace(&self) -> Workspace {
        Workspace::new(self.path("workspace")).unwrap()
    }

    fn env(&self) -> Vec<(String, String)> {
        vec![
            (
                "FAKE_CODEX_SCRIPT".into(),
                self.path("script.txt").display().to_string(),
            ),
            (
                "FAKE_CODEX_LOG".into(),
                self.path("log").display().to_string(),
            ),
        ]
    }

    fn exec(&self, gate: Arc<dyn ApprovalGate>, mode: ApprovalMode) -> ToolExecutor {
        let audit = AuditLog::open(self.path("approvals.jsonl"), "s-test").unwrap();
        ToolExecutor::new(
            Arc::new(ToolRegistry::standard()),
            gate,
            Arc::new(ApprovalPolicy::new(mode)),
            Arc::new(NoSandbox),
        )
        .with_audit(Some(Arc::new(audit)))
        .with_provider(Some("codex".into()))
    }

    fn provider_with(
        &self,
        gate: Arc<dyn ApprovalGate>,
        mode: ApprovalMode,
    ) -> CodexAppServerProvider {
        let mut config = CodexConfig::new(
            self.path("codex"),
            self.workspace(),
            "/nonexistent/rozu",
            vec!["mcp-serve".into(), "--provider".into(), "codex".into()],
            self.exec(gate, mode),
        );
        config.env = self.env();
        config.interrupt_grace = Duration::from_secs(5);
        CodexAppServerProvider::new(config).unwrap()
    }

    fn provider(&self) -> CodexAppServerProvider {
        self.provider_with(TestGate::new(|_| false), ApprovalMode::AutoWorkspace)
    }

    /// argv of each app-server start.
    fn starts(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.path("log/args.txt"))
            .unwrap_or_default()
            .split("--\n")
            .filter(|s| !s.is_empty())
            .map(|s| s.lines().map(str::to_owned).collect())
            .collect()
    }

    /// Everything rozu wrote to the app-server.
    fn sent(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("log/stdin.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn sent_request(&self, method: &str) -> Vec<Value> {
        self.sent()
            .into_iter()
            .filter(|m| m["method"] == method)
            .collect()
    }

    /// rozu's answer to the server request `id`.
    fn reply(&self, id: &str) -> Value {
        self.sent()
            .into_iter()
            .find(|m| m["id"] == id && m.get("method").is_none())
            .unwrap_or_else(|| panic!("no reply to {id}: {:?}", self.sent()))
    }

    fn audit(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("approvals.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

fn request(model: &str, messages: Vec<Message>) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages,
        tools: Vec::new(),
        max_output_tokens: None,
    }
}

async fn collect(
    provider: &CodexAppServerProvider,
    req: ChatRequest,
) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let mut stream = provider
        .stream(req, CancellationToken::new())
        .await
        .unwrap();
    let mut events = Vec::new();
    while let Some(item) = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .unwrap()
    {
        match item {
            Ok(event) => events.push(event),
            Err(err) => return (events, Some(err)),
        }
    }
    (events, None)
}

fn approvals(events: &[StreamEvent]) -> Vec<&ApprovalRecord> {
    events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::Approval(record) => Some(record),
            _ => None,
        })
        .collect()
}

fn override_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2)
        .filter(|w| w[0] == "-c")
        .find_map(|w| w[1].strip_prefix(&format!("{key}=")))
}

#[tokio::test]
async fn handshake_and_text_turn() {
    let fake = Fake::new(&["handshake.script", "text_turn.script"]);
    let provider = fake.provider();

    let (events, err) = collect(
        &provider,
        request(
            "default",
            vec![Message::system("You are rozu."), Message::user("hi")],
        ),
    )
    .await;

    assert_eq!(err, None);
    assert_eq!(
        events,
        [
            StreamEvent::TextDelta("Hello ".into()),
            StreamEvent::TextDelta("from Codex!".into()),
            // cachedInputTokens is part of inputTokens; no cache writes.
            StreamEvent::Usage(Usage {
                input_tokens: 1200,
                output_tokens: 30,
                cache_read_tokens: 1000,
                cache_write_tokens: 0,
            }),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );

    // The exact command line: config overrides, then `app-server`.
    let starts = fake.starts();
    assert_eq!(starts.len(), 1);
    let args = &starts[0];
    assert_eq!(args.last().map(String::as_str), Some("app-server"));
    assert_eq!(
        override_value(args, "approval_policy"),
        Some("\"untrusted\"")
    );
    assert_eq!(
        override_value(args, "sandbox_mode"),
        Some("\"workspace-write\"")
    );
    assert_eq!(
        override_value(args, "sandbox_workspace_write.network_access"),
        Some("false")
    );
    assert_eq!(override_value(args, "web_search"), Some("\"disabled\""));
    assert_eq!(
        override_value(args, "mcp_servers.rozu.command"),
        Some("\"/nonexistent/rozu\"")
    );
    assert_eq!(
        override_value(args, "mcp_servers.rozu.args"),
        Some("[\"mcp-serve\", \"--provider\", \"codex\"]")
    );
    // Codex doesn't ask before rozu's own tools (rozu gates them); only that server.
    assert_eq!(
        override_value(args, "mcp_servers.rozu.default_tools_approval_mode"),
        Some("\"approve\"")
    );
    assert_eq!(
        args.iter()
            .filter(|a| a.contains("approval_mode"))
            .collect::<Vec<_>>(),
        ["mcp_servers.rozu.default_tools_approval_mode=\"approve\""]
    );
    let env = override_value(args, "mcp_servers.rozu.env").unwrap();
    let socket = env
        .strip_prefix("{ ROZU_APPROVAL_SOCKET = \"")
        .and_then(|s| s.strip_suffix("\" }"))
        .unwrap_or_else(|| panic!("{env}"));
    assert!(Path::new(socket).exists(), "{socket}");
    assert!(
        !args
            .iter()
            .any(|a| a.contains("never") || a.contains("danger"))
    );

    // Handshake, thread and turn as sent.
    let init = &fake.sent_request("initialize")[0];
    assert_eq!(init["params"]["clientInfo"]["name"], "rozu");
    assert_eq!(fake.sent_request("initialized").len(), 1);
    assert_eq!(
        fake.sent_request("account/read")[0]["params"],
        json!({"refreshToken": false})
    );
    let thread = &fake.sent_request("thread/start")[0]["params"];
    let root = fake.workspace().root().display().to_string();
    assert_eq!(thread["cwd"], root.as_str());
    assert_eq!(thread["approvalPolicy"], "untrusted");
    assert_eq!(thread["sandbox"], "workspace-write");
    assert!(thread.get("model").is_none(), "default model: Codex picks");
    let instructions = thread["developerInstructions"].as_str().unwrap();
    assert!(instructions.starts_with("You are rozu."), "{instructions}");
    assert!(instructions.contains("`rozu` MCP"), "{instructions}");
    let turn = &fake.sent_request("turn/start")[0]["params"];
    assert_eq!(turn["threadId"], "thr_1");
    assert_eq!(
        turn["input"],
        json!([{"type": "text", "text": "hi", "text_elements": []}])
    );
    assert!(turn.get("model").is_none());

    // Codex runs in the workspace on its own login, not an API key from the environment.
    let cwd = std::fs::read_to_string(fake.path("log/cwd.txt")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        fake.workspace().root()
    );
    let env = std::fs::read_to_string(fake.path("log/env.txt")).unwrap();
    assert_eq!(env.trim(), "OPENAI_API_KEY=unset");
}

#[tokio::test]
async fn follow_up_turns_reuse_the_thread_and_report_their_own_usage() {
    let fake = Fake::new(&["handshake.script", "text_turn.script", "second_turn.script"]);
    let provider = fake.provider();
    let first = vec![Message::system("s"), Message::user("one")];
    collect(&provider, request("default", first.clone())).await;
    let mut second = first;
    second.push(Message::assistant("Hello from Codex!"));
    second.push(Message::user("two"));

    // A model switch is a `turn/start` parameter, not a restart.
    let (events, err) = collect(&provider, request("gpt-6-luna", second)).await;

    assert_eq!(err, None);
    assert_eq!(
        events,
        [
            StreamEvent::TextDelta("Second answer.".into()),
            StreamEvent::Usage(Usage {
                input_tokens: 1800,
                output_tokens: 40,
                cache_read_tokens: 1400,
                cache_write_tokens: 0,
            }),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );
    assert_eq!(fake.starts().len(), 1, "one long-lived app-server");
    let turns = fake.sent_request("turn/start");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1]["params"]["input"][0]["text"], "two");
    assert_eq!(turns[1]["params"]["model"], "gpt-6-luna");
}

#[tokio::test]
async fn command_approvals_go_through_rozu_policy() {
    let fake = Fake::new(&["handshake.script", "exec_approvals.script"]);
    // The "user" approves only the curl command.
    let gate = TestGate::new(|req| req.preview.contains("curl"));
    let provider = fake.provider_with(gate.clone(), ApprovalMode::AutoWorkspace);

    let (events, err) = collect(&provider, request("default", vec![Message::user("go")])).await;
    assert_eq!(err, None);

    // cargo test: a workspace command under Codex's sandbox, auto-approved without asking.
    assert_eq!(fake.reply("srv-1")["result"], json!({"decision": "accept"}));
    // Force push: destructive, the user was asked and said no.
    assert_eq!(
        fake.reply("srv-2")["result"],
        json!({"decision": "decline"})
    );
    // curl: network, the user approved; a one-time accept, never for the session.
    assert_eq!(fake.reply("srv-3")["result"], json!({"decision": "accept"}));
    // Legacy exec request for a read: auto-approved.
    assert_eq!(
        fake.reply("srv-4")["result"],
        json!({"decision": "approved"})
    );
    // An escalation (Codex gives a reason): always the user, even for `ls`.
    assert_eq!(
        fake.reply("srv-5")["result"],
        json!({"decision": "decline"})
    );
    // A working directory outside the workspace: the user.
    assert_eq!(
        fake.reply("srv-6")["result"],
        json!({"decision": "decline"})
    );

    let asked = gate.asked();
    let risks: Vec<Risk> = asked.iter().map(|r| r.risk).collect();
    assert_eq!(
        risks,
        [
            Risk::Destructive,
            Risk::Network,
            Risk::Read,
            Risk::WriteOutside
        ]
    );
    let push = &asked[0];
    assert_eq!(push.tool, "codex:shell");
    assert!(
        push.preview.starts_with("$ git push --force origin main\n(codex runs: /bin/zsh -lc 'git push --force origin main')"),
        "{}",
        push.preview
    );
    assert_eq!(
        push.sandbox,
        SandboxMode::Enforced(rozu_core::CODEX_SANDBOX)
    );
    assert_eq!(
        push.allow_rule, None,
        "no always-allow for destructive commands"
    );
    assert!(asked[2].preview.contains("retry without it?"));
    assert_eq!(asked[2].allow_rule, None);

    // Every decision is in the stream and the audit log, tagged with the provider.
    let records = approvals(&events);
    assert_eq!(records.len(), 6);
    assert_eq!(
        records[0].source,
        DecisionSource::Mode(ApprovalMode::AutoWorkspace)
    );
    assert_eq!(records[1].source, DecisionSource::Gate);
    let audit = fake.audit();
    assert_eq!(audit.len(), 6);
    assert!(audit.iter().all(|e| e["provider"] == "codex"), "{audit:?}");
    assert_eq!(audit[1]["decision"], "deny");
    assert_eq!(audit[1]["risk"], "destructive");
    // `u` on a Codex command is a one-time approval: Codex's sandbox stays on.
    assert_eq!(records[2].decision, Decision::Approve);
    assert_eq!(audit[2]["kind"], "approve");
    assert_eq!(audit[2]["sandboxed"], true);

    // The commands themselves show up as tool calls with results.
    assert!(events.contains(&StreamEvent::ToolCallStart {
        id: "call_1".into(),
        name: "codex:shell".into()
    }));
    assert!(events.contains(&StreamEvent::ToolResult {
        id: "call_1".into(),
        output: "test result: ok\n".into(),
        is_error: false
    }));
    assert!(events.contains(&StreamEvent::ToolResult {
        id: "call_2".into(),
        output: "declined".into(),
        is_error: true
    }));
}

#[tokio::test]
async fn read_only_mode_runs_codex_read_only_and_asks_for_workspace_commands() {
    let fake = Fake::new(&["handshake.script", "exec_approvals.script"]);
    let gate = TestGate::new(|_| false);
    let provider = fake.provider_with(gate.clone(), ApprovalMode::ReadOnly);
    collect(&provider, request("default", vec![Message::user("go")])).await;

    let args = &fake.starts()[0];
    assert_eq!(override_value(args, "sandbox_mode"), Some("\"read-only\""));
    assert_eq!(
        fake.sent_request("thread/start")[0]["params"]["sandbox"],
        "read-only"
    );
    // cargo test now needs the user (who declines).
    assert_eq!(
        fake.reply("srv-1")["result"],
        json!({"decision": "decline"})
    );
    assert_eq!(gate.asked()[0].risk, Risk::WriteWorkspace);
}

#[tokio::test]
async fn patch_approvals_check_every_path() {
    let fake = Fake::new(&["handshake.script", "patch_approvals.script"]);
    let gate = TestGate::new(|_| false);
    let provider = fake.provider_with(gate.clone(), ApprovalMode::AutoWorkspace);

    let (events, err) = collect(&provider, request("default", vec![Message::user("go")])).await;
    assert_eq!(err, None);

    // A new file in the workspace: auto-approved; the diff came from the fileChange item.
    assert_eq!(fake.reply("srv-1")["result"], json!({"decision": "accept"}));
    // .rozu/ is the user's: denied without asking.
    assert_eq!(
        fake.reply("srv-2")["result"],
        json!({"decision": "decline"})
    );
    // Legacy patch outside the workspace: the user said no.
    let legacy = &fake.reply("srv-3")["result"]["decision"]["denied"]["rejection"];
    assert!(
        legacy.as_str().unwrap().contains("not this one"),
        "{legacy}"
    );
    // Deleting a file: destructive, the user said no.
    assert_eq!(
        fake.reply("srv-4")["result"],
        json!({"decision": "decline"})
    );
    // An item rozu never saw: it can't tell what changes, so no.
    assert_eq!(
        fake.reply("srv-5")["result"],
        json!({"decision": "decline"})
    );

    let asked = gate.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(asked[0].tool, "codex:apply_patch");
    assert_eq!(asked[0].risk, Risk::WriteOutside);
    assert!(
        asked[0]
            .preview
            .contains("*** update /etc/hosts\n@@ -1 +1 @@"),
        "{}",
        asked[0].preview
    );
    assert_eq!(asked[1].risk, Risk::Destructive);
    assert!(asked[1].preview.contains("*** delete README.md"));

    let records = approvals(&events);
    assert_eq!(records.len(), 5);
    assert!(records[0].preview.contains("*** add notes.md\n+hello"));
    assert_eq!(records[1].source, DecisionSource::Policy);
    assert_eq!(records[4].source, DecisionSource::Policy);
    assert!(events.contains(&StreamEvent::ToolResult {
        id: "fc_1".into(),
        output: "applied: notes.md".into(),
        is_error: false
    }));
}

#[tokio::test]
async fn unmapped_requests_are_denied_and_token_refresh_is_refused() {
    let fake = Fake::new(&["handshake.script", "unmapped.script"]);
    let gate = TestGate::new(|_| false);
    let provider = fake.provider_with(gate.clone(), ApprovalMode::FullAccess);

    let (events, err) = collect(&provider, request("default", vec![Message::user("go")])).await;
    assert_eq!(err, None);

    let user_input = fake.reply("srv-1");
    assert_eq!(user_input["error"]["code"], -32601);
    assert_eq!(
        fake.reply("srv-2")["result"],
        json!({"action": "decline"}),
        "elicitations are declined"
    );
    let dynamic = &fake.reply("srv-3")["result"];
    assert_eq!(dynamic["success"], false);
    // rozu never handles ChatGPT tokens.
    let refresh = fake.reply("srv-4");
    assert!(refresh.get("result").is_none(), "{refresh}");
    let message = refresh["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("does not handle ChatGPT tokens"),
        "{message}"
    );
    assert!(message.contains("codex login"), "{message}");
    assert_eq!(fake.reply("srv-5")["error"]["code"], -32601);
    // More permissions (network) always go to the user, even in full access; denied → none granted.
    assert_eq!(
        fake.reply("srv-6")["result"],
        json!({"permissions": {}, "scope": "turn"})
    );
    let asked = gate.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].risk, Risk::Network);
    assert!(asked[0].network);

    let records = approvals(&events);
    assert_eq!(records.len(), 6);
    assert!(
        records[..5]
            .iter()
            .all(|r| r.source == DecisionSource::Policy && !r.decision.is_approval())
    );
    assert_eq!(fake.audit().len(), 6);
    assert!(
        fake.audit()
            .iter()
            .any(|e| e["tool"] == "codex:chatgpt_auth_tokens_refresh" && e["decision"] == "deny")
    );
}

#[tokio::test]
async fn only_tool_approvals_for_rozus_mcp_server_are_accepted() {
    let fake = Fake::new(&["handshake.script", "elicitations.script"]);
    // A user who would approve anything: none of this may reach them.
    let gate = TestGate::new(|_| true);
    let provider = fake.provider_with(gate.clone(), ApprovalMode::AutoWorkspace);

    let (events, err) = collect(&provider, request("default", vec![Message::user("go")])).await;
    assert_eq!(err, None);

    // rozu's own tools: accepted; `rozu mcp-serve` decides each call when it runs.
    for id in ["srv-1", "srv-2", "srv-3"] {
        assert_eq!(
            fake.reply(id)["result"],
            json!({"action": "accept", "content": {}}),
            "{id}"
        );
    }
    // Another server, a near-miss name, URL / free-form / openai forms, a tool rozu doesn't
    // run through its policy, another approval kind, malformed params, a message with extra
    // text: all declined.
    for n in 4..=12 {
        let id = format!("srv-{n}");
        assert_eq!(
            fake.reply(&id)["result"],
            json!({"action": "decline"}),
            "{id}"
        );
    }
    assert!(gate.asked().is_empty(), "{:?}", gate.asked());

    let records = approvals(&events);
    assert_eq!(records.len(), 12);
    assert!(
        records[..3]
            .iter()
            .all(|r| r.source == DecisionSource::Delegated && r.decision == Decision::Approve)
    );
    assert!(
        records[3..]
            .iter()
            .all(|r| r.source == DecisionSource::Policy && !r.decision.is_approval())
    );

    let audit = fake.audit();
    assert_eq!(audit.len(), 12);
    assert!(audit.iter().all(|e| e["provider"] == "codex"), "{audit:?}");
    let expected = [
        ("rozu:list_dir", "read"),
        ("rozu:read_file", "read"),
        ("rozu:write_file", "write in workspace"),
    ];
    for (entry, (tool, risk)) in audit.iter().zip(expected) {
        assert_eq!(entry["tool"], tool, "{entry}");
        assert_eq!(entry["decision"], "allow", "{entry}");
        assert_eq!(entry["decided_by"], "delegated-to-rozu-mcp", "{entry}");
        assert_eq!(entry["risk"], risk, "{entry}");
    }
    assert_eq!(audit[0]["call_id"], "elicitation-srv-1");
    for entry in &audit[3..] {
        assert_eq!(entry["tool"], "codex:mcp_elicitation", "{entry}");
        assert_eq!(entry["decision"], "deny", "{entry}");
        assert_eq!(entry["decided_by"], "policy", "{entry}");
    }
}

#[tokio::test]
async fn not_logged_in_tells_the_user_to_log_in_with_codex() {
    let fake = Fake::new(&["not_logged_in.script"]);
    let err = fake
        .provider()
        .stream(
            request("default", vec![Message::user("hi")]),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("an error");
    assert!(matches!(err, ProviderError::NotLoggedIn { .. }), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("run `rozu auth login codex`"), "{text}");
    assert!(!text.contains("example.invalid"));
    assert!(fake.sent_request("thread/start").is_empty());
}

#[tokio::test]
async fn cancel_interrupts_the_turn_and_keeps_the_app_server() {
    let fake = Fake::new(&["handshake.script", "interrupt.script", "second_turn.script"]);
    let provider = fake.provider();
    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(
            request("default", vec![Message::user("one")]),
            cancel.clone(),
        )
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .unwrap();
    assert_eq!(
        first.unwrap().unwrap(),
        StreamEvent::TextDelta("Working on it".into())
    );
    cancel.cancel();
    let next = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .unwrap();
    assert_eq!(next.unwrap(), Err(ProviderError::Cancelled));
    drop(stream);

    let interrupt = &fake.sent_request("turn/interrupt")[0]["params"];
    assert_eq!(interrupt, &json!({"threadId": "thr_1", "turnId": "turn_1"}));

    let (events, err) = collect(
        &provider,
        request("default", vec![Message::user("one"), Message::user("two")]),
    )
    .await;
    assert_eq!(err, None);
    assert_eq!(events[0], StreamEvent::TextDelta("Second answer.".into()));
    assert_eq!(
        fake.starts().len(),
        1,
        "the app-server survived the interrupt"
    );
}

#[tokio::test]
async fn approval_records_from_rozu_mcp_serve_join_the_stream() {
    let fake = Fake::new(&["handshake.script", "interrupt.script"]);
    let provider = fake.provider();
    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(
            request("default", vec![Message::user("one")]),
            cancel.clone(),
        )
        .await
        .unwrap();
    let _text = stream.next().await;

    let record = ApprovalRecord {
        call_id: "mcp-1".into(),
        tool: "shell".into(),
        risk: Risk::Destructive,
        preview: "$ rm -rf build".into(),
        sandbox: SandboxMode::Enforced("seatbelt"),
        network: false,
        extra_writes: Vec::new(),
        blocked_retry: false,
        decision: Decision::Deny {
            reason: "no".into(),
        },
        source: DecisionSource::Gate,
    };
    SocketGate::new(Some(provider.runtime_dir().join("approval.sock")))
        .send_record(record.clone())
        .await;
    let next = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .unwrap();
    assert_eq!(next.unwrap(), Ok(StreamEvent::Approval(record)));
    cancel.cancel();
    let _ = stream.next().await;
}

#[tokio::test]
async fn missing_binary_is_cli_not_found() {
    let fake = Fake::new(&[]);
    let config = CodexConfig::new(
        fake.path("no-such-codex"),
        fake.workspace(),
        "rozu",
        Vec::new(),
        fake.exec(TestGate::new(|_| false), ApprovalMode::AutoWorkspace),
    );
    let provider = CodexAppServerProvider::new(config).unwrap();
    let err = provider
        .stream(
            request("default", vec![Message::user("hi")]),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("an error");
    assert!(matches!(err, ProviderError::CliNotFound { .. }), "{err:?}");
    assert!(err.to_string().contains("ROZU_CODEX_BIN"), "{err}");
}

#[tokio::test]
async fn agent_runs_a_codex_turn_without_cost() {
    let fake = Fake::new(&["handshake.script", "text_turn.script"]);
    let provider = Arc::new(fake.provider());
    let config = AgentConfig::new("default")
        .with_model_info(Some(ModelInfo::subscription("codex", "default", 272_000)));
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::standard()),
        fake.workspace(),
        config,
    );
    let mut session = Session::with_system_prompt("You are rozu.");
    let (tx, mut rx) = mpsc::channel(64);
    let outcome = agent
        .run_turn("hi", &mut session, &tx, CancellationToken::new())
        .await
        .unwrap();
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }
    assert_eq!(outcome, TurnOutcome::Completed);
    assert!(events.contains(&AgentEvent::TextDelta("Hello ".into())));
    assert_eq!(
        session.messages.last().unwrap(),
        &Message::assistant("Hello from Codex!")
    );
    assert_eq!(session.usage.cache_read_tokens, 1000);
    assert_eq!(session.last_turn.cost_usd, None);
}

/// A wrapper that runs the fake with `vars` set (tests can't change their own environment).
fn wrapper(fake: &Fake, vars: &[(&str, String)]) -> PathBuf {
    let path = fake.path("codex-wrapper");
    let exports: String = vars.iter().map(|(k, v)| format!("{k}='{v}' ")).collect();
    std::fs::write(
        &path,
        format!(
            "#!/bin/sh\n{exports}exec '{}' \"$@\"\n",
            fake.path("codex").display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    path
}

#[tokio::test]
async fn version_login_status_and_model_list() {
    let fake = Fake::new(&["models.script"]);
    assert_eq!(codex_version(&fake.path("codex")).await.unwrap(), "0.147.0");

    let chatgpt = wrapper(
        &fake,
        &[("FAKE_CODEX_LOGIN_STATUS", "Logged in using ChatGPT".into())],
    );
    assert_eq!(
        codex_login_status(&chatgpt).await.unwrap(),
        LoginStatus::ChatGpt
    );
    let logged_out = wrapper(
        &fake,
        &[("FAKE_CODEX_LOGIN_STATUS", "Not logged in".into())],
    );
    assert_eq!(
        codex_login_status(&logged_out).await.unwrap(),
        LoginStatus::NotLoggedIn
    );

    let models = wrapper(
        &fake,
        &[
            (
                "FAKE_CODEX_SCRIPT",
                fake.path("script.txt").display().to_string(),
            ),
            ("FAKE_CODEX_LOG", fake.path("log").display().to_string()),
        ],
    );
    let list = list_models(&models).await.unwrap();
    let ids: Vec<&str> = list.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["gpt-6.1-sol", "gpt-6-luna"],
        "hidden models are left out"
    );
    assert!(list[0].is_default);
}
