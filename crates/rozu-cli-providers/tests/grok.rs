//! `GrokSession` (ACP) against a fake `grok` binary that follows scripted ACP exchanges. No
//! real Grok Build CLI is run.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rozu_cli_providers::grok::install::grok_version;
use rozu_cli_providers::{EpochReason, GrokConfig, GrokSession};
use rozu_core::{
    Agent, AgentConfig, AgentEvent, ApprovalMode, ApprovalPolicy, ApprovalRecord, AuditLog,
    ChatRequest, Decision, DecisionSource, Message, ModelInfo, Provider, ProviderError,
    ReadOnlyGate, SandboxMode, Session, StopReason, StreamEvent, ToolExecutor, TurnOutcome, Usage,
};
use rozu_mcp::SocketGate;
use rozu_tools::{NoSandbox, Risk, ToolRegistry, Workspace};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/grok");

/// The fake grok, its script, log directory and a workspace.
struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    /// The fake agent follows `scripts` (fixture names), concatenated.
    fn new(scripts: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["log", "workspace"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        std::fs::write(dir.path().join("workspace/README.md"), "# demo\n").unwrap();
        let bin = dir.path().join("grok");
        std::fs::copy(Path::new(FIXTURES).join("fake_grok.sh"), &bin).unwrap();
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

    fn exec(&self) -> ToolExecutor {
        let audit = AuditLog::open(self.path("approvals.jsonl"), "s-test").unwrap();
        ToolExecutor::new(
            Arc::new(ToolRegistry::standard()),
            Arc::new(ReadOnlyGate),
            Arc::new(ApprovalPolicy::new(ApprovalMode::AutoWorkspace)),
            Arc::new(NoSandbox),
        )
        .with_audit(Some(Arc::new(audit)))
        .with_provider(Some("grok".into()))
    }

    fn config(&self) -> GrokConfig {
        let mut config = GrokConfig::new(
            self.path("grok"),
            self.workspace(),
            "/opt/rozu/bin/rozu",
            vec![
                "mcp-serve".into(),
                "--session-id".into(),
                "s-test".into(),
                "--provider".into(),
                "grok".into(),
            ],
            self.exec(),
        );
        config.env = vec![
            (
                "FAKE_GROK_SCRIPT".into(),
                self.path("script.txt").display().to_string(),
            ),
            (
                "FAKE_GROK_LOG".into(),
                self.path("log").display().to_string(),
            ),
            // Inherited from a user's shell: must never reach Grok.
            ("XAI_API_KEY".into(), "xai-should-not-pass".into()),
        ];
        config.cancel_grace = Duration::from_secs(5);
        config.handshake_timeout = Duration::from_secs(10);
        config
    }

    fn provider(&self) -> GrokSession {
        GrokSession::new(self.config()).unwrap()
    }

    /// argv of each agent start.
    fn starts(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.path("log/args.txt"))
            .unwrap_or_default()
            .split("--\n")
            .filter(|s| !s.is_empty())
            .map(|s| s.lines().map(str::to_owned).collect())
            .collect()
    }

    /// Everything rozu wrote to the agent.
    fn sent(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("log/stdin.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }

    fn sent_method(&self, method: &str) -> Vec<Value> {
        self.sent()
            .into_iter()
            .filter(|m| m["method"] == method)
            .collect()
    }

    /// rozu's answer to the agent request `id`.
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
    provider: &GrokSession,
    req: ChatRequest,
) -> (Vec<StreamEvent>, Option<ProviderError>) {
    let mut stream = match provider.stream(req, CancellationToken::new()).await {
        Ok(stream) => stream,
        Err(err) => return (Vec::new(), Some(err)),
    };
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
            StreamEvent::TextDelta("from Grok!".into()),
            // ACP's inputTokens already includes the cached part; no cache writes.
            StreamEvent::Usage(Usage {
                input_tokens: 4591,
                output_tokens: 111,
                cache_read_tokens: 1152,
                cache_write_tokens: 0,
            }),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );

    // The native binary with the agent profile, no model flag for `default`.
    let starts = fake.starts();
    assert_eq!(starts.len(), 1);
    let profile = provider.profile_path().display().to_string();
    assert_eq!(
        starts[0],
        [
            "--no-auto-update",
            "agent",
            "--agent-profile",
            &profile,
            "stdio"
        ]
    );
    let written = std::fs::read_to_string(fake.path("log/profile.md")).unwrap();
    assert!(written.contains("\ntools: use_tool\n"), "{written}");
    // No API key, no chat proxy; the workspace is the working directory.
    let env = std::fs::read_to_string(fake.path("log/env.txt")).unwrap();
    assert!(env.contains("XAI_API_KEY=unset"), "{env}");
    assert!(env.contains("GROK_CLI_CHAT_PROXY_BASE_URL=unset"), "{env}");
    let cwd = std::fs::read_to_string(fake.path("log/cwd.txt")).unwrap();
    assert_eq!(
        Path::new(cwd.trim()),
        std::fs::canonicalize(fake.path("workspace")).unwrap()
    );

    // initialize: no fs, no terminal; only the cached login is used.
    let init = &fake.sent_method("initialize")[0];
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(
        init["params"]["clientCapabilities"],
        json!({"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false})
    );
    let auth = fake.sent_method("authenticate");
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0]["params"], json!({"methodId": "cached_token"}));

    // session/new: rozu's MCP server is the only toolset, the profile is the override.
    let new = &fake.sent_method("session/new")[0]["params"];
    let servers = new["mcpServers"].as_array().unwrap();
    assert_eq!(servers.len(), 1);
    assert_eq!(servers[0]["name"], "rozu");
    assert_eq!(servers[0]["command"], "/opt/rozu/bin/rozu");
    assert_eq!(
        servers[0]["args"],
        json!(["mcp-serve", "--session-id", "s-test", "--provider", "grok"])
    );
    assert_eq!(servers[0]["env"][0]["name"], "ROZU_APPROVAL_SOCKET");
    let socket = servers[0]["env"][0]["value"].as_str().unwrap();
    assert!(socket.ends_with("approval.sock"), "{socket}");
    let system = new["_meta"]["systemPromptOverride"].as_str().unwrap();
    assert!(system.starts_with("You are rozu.\n\n"), "{system}");
    assert!(system.contains("- rozu__read_file: "), "{system}");
    assert!(system.contains("- rozu__shell: "), "{system}");

    let prompt = &fake.sent_method("session/prompt")[0]["params"];
    assert_eq!(
        prompt,
        &json!({"sessionId": "sess-0001", "prompt": [{"type": "text", "text": "hi"}]})
    );
    assert_eq!(provider.epoch().await, 1);
}

#[tokio::test]
async fn follow_up_turns_reuse_the_session() {
    let fake = Fake::new(&["handshake.script", "text_turn.script", "second_turn.script"]);
    let provider = fake.provider();
    let (_, err) = collect(&provider, request("default", vec![Message::user("one")])).await;
    assert_eq!(err, None);
    let (events, err) = collect(
        &provider,
        request(
            "default",
            vec![
                Message::user("one"),
                Message::assistant("Hello from Grok!"),
                Message::user("two"),
            ],
        ),
    )
    .await;
    assert_eq!(err, None);
    assert_eq!(events[0], StreamEvent::TextDelta("Second answer.".into()));
    assert_eq!(
        events[1],
        StreamEvent::Usage(Usage {
            input_tokens: 18139,
            output_tokens: 20,
            cache_read_tokens: 15872,
            cache_write_tokens: 0,
        })
    );
    // One process, one session; only the new message is sent (Grok has the rest).
    assert_eq!(fake.starts().len(), 1);
    assert_eq!(fake.sent_method("session/new").len(), 1);
    let prompts = fake.sent_method("session/prompt");
    assert_eq!(prompts[1]["params"]["prompt"][0]["text"], "two");
}

#[tokio::test]
async fn epoch_rotation_keeps_the_process_and_opens_a_new_session() {
    let fake = Fake::new(&["handshake.script", "text_turn.script", "new_epoch.script"]);
    let provider = fake.provider();
    let (_, err) = collect(&provider, request("default", vec![Message::user("one")])).await;
    assert_eq!(err, None);
    assert_eq!(provider.session_id().await.as_deref(), Some("sess-0001"));

    provider.rotate_epoch(EpochReason::Compaction).await;
    let (events, err) = collect(
        &provider,
        request(
            "default",
            vec![
                Message::system("You are rozu (refreshed)."),
                Message::user("[summary of earlier turns]"),
                Message::assistant("ok"),
                Message::user("next"),
            ],
        ),
    )
    .await;
    assert_eq!(err, None);
    // The old session's updates are ignored.
    assert_eq!(events[0], StreamEvent::TextDelta("Fresh epoch.".into()));
    assert_eq!(provider.epoch().await, 2);
    assert_eq!(provider.session_id().await.as_deref(), Some("sess-0002"));

    assert_eq!(fake.starts().len(), 1, "the process survived the rotation");
    assert_eq!(fake.sent_method("initialize").len(), 1);
    let sessions = fake.sent_method("session/new");
    assert_eq!(sessions.len(), 2);
    let system = sessions[1]["params"]["_meta"]["systemPromptOverride"]
        .as_str()
        .unwrap();
    assert!(system.starts_with("You are rozu (refreshed)."), "{system}");
    // The new session gets rozu's (rewritten) history.
    let prompt = &fake.sent_method("session/prompt")[1]["params"];
    assert_eq!(prompt["sessionId"], "sess-0002");
    let text = prompt["prompt"][0]["text"].as_str().unwrap();
    assert!(
        text.starts_with("Earlier in this conversation:\n")
            && text.contains("User: [summary of earlier turns]")
            && text.ends_with("The user's new message:\nnext"),
        "{text}"
    );
}

#[tokio::test]
async fn use_tool_calls_reach_rozus_mcp_and_only_they_are_approved() {
    let fake = Fake::new(&["handshake.script", "tool_turn.script"]);
    let provider = fake.provider();
    let (events, err) = collect(&provider, request("default", vec![Message::user("read")])).await;
    assert_eq!(err, None);

    // use_tool → rozu__read_file shows as rozu's read_file, with its result.
    assert_eq!(
        events[..3],
        [
            StreamEvent::ToolCallStart {
                id: "call-0001".into(),
                name: "read_file".into()
            },
            StreamEvent::ToolCallArgsDelta {
                id: "call-0001".into(),
                json: r#"{"path":"README.md"}"#.into()
            },
            StreamEvent::ToolCallEnd {
                id: "call-0001".into()
            },
        ]
    );
    assert!(events.contains(&StreamEvent::ToolResult {
        id: "call-0001".into(),
        output: "# demo".into(),
        is_error: false
    }));
    assert!(events.contains(&StreamEvent::ToolResult {
        id: "call-0002".into(),
        output: "User rejected the tool call".into(),
        is_error: true
    }));
    assert!(events.contains(&StreamEvent::TextDelta("The README says demo.".into())));

    // Permission for rozu's MCP tool: approved once (never "always"), delegated.
    assert_eq!(
        fake.reply("perm-1")["result"],
        json!({"outcome": {"outcome": "selected", "optionId": "allow-once"}})
    );
    // Grok's own shell: denied by default.
    assert_eq!(
        fake.reply("perm-2")["result"],
        json!({"outcome": {"outcome": "selected", "optionId": "reject-once"}})
    );
    // A file-system request: refused.
    assert_eq!(fake.reply("fs-1")["error"]["code"], -32601);

    let records = approvals(&events);
    assert_eq!(records.len(), 3);
    assert_eq!(records[0].tool, "rozu:read_file");
    assert_eq!(records[0].source, DecisionSource::Delegated);
    assert_eq!(records[0].decision, Decision::Approve);
    assert_eq!(records[1].tool, "grok:run_terminal_cmd");
    assert_eq!(records[1].source, DecisionSource::Policy);
    assert!(!records[1].decision.is_approval());

    let audit = fake.audit();
    assert_eq!(audit.len(), 3);
    assert!(audit.iter().all(|e| e["provider"] == "grok"), "{audit:?}");
    assert_eq!(audit[0]["decided_by"], "delegated-to-rozu-mcp");
    assert_eq!(audit[0]["decision"], "allow");
    assert_eq!(audit[1]["decided_by"], "policy");
    assert_eq!(audit[1]["decision"], "deny");
    assert_eq!(audit[2]["tool"], "grok:fs/read_text_file");
}

#[tokio::test]
async fn approval_records_from_rozu_mcp_serve_join_the_stream() {
    let fake = Fake::new(&["handshake.script", "cancel.script"]);
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
    // What `rozu mcp-serve --provider grok` sends when it decides a tool call.
    let record = ApprovalRecord {
        call_id: "mcp-1".into(),
        tool: "shell".into(),
        risk: Risk::Destructive,
        preview: "$ git push --force".into(),
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
async fn cancel_sends_session_cancel_and_keeps_process_and_session() {
    let fake = Fake::new(&["handshake.script", "cancel.script", "second_turn.script"]);
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

    let cancels = fake.sent_method("session/cancel");
    assert_eq!(cancels.len(), 1);
    assert_eq!(cancels[0]["params"], json!({"sessionId": "sess-0001"}));
    assert!(cancels[0].get("id").is_none(), "a notification");

    let (events, err) = collect(
        &provider,
        request("default", vec![Message::user("one"), Message::user("two")]),
    )
    .await;
    assert_eq!(err, None);
    assert_eq!(events[0], StreamEvent::TextDelta("Second answer.".into()));
    assert_eq!(fake.starts().len(), 1, "grok survived the cancel");
    assert_eq!(fake.sent_method("session/new").len(), 1, "same session");
}

#[tokio::test]
async fn failed_cached_login_says_run_grok_login() {
    let fake = Fake::new(&["not_logged_in.script"]);
    let provider = fake.provider();
    let (_, err) = collect(&provider, request("default", vec![Message::user("hi")])).await;
    let err = err.expect("an error");
    assert!(matches!(err, ProviderError::NotLoggedIn { .. }), "{err:?}");
    assert!(err.to_string().contains("grok login"), "{err}");
    assert!(fake.sent_method("session/new").is_empty());
}

#[tokio::test]
async fn other_auth_methods_are_never_used() {
    let fake = Fake::new(&["no_cached_login.script"]);
    let provider = fake.provider();
    let (_, err) = collect(&provider, request("default", vec![Message::user("hi")])).await;
    let err = err.expect("an error");
    assert!(matches!(err, ProviderError::NotLoggedIn { .. }), "{err:?}");
    // Neither the browser login nor an API key was picked.
    assert!(fake.sent_method("authenticate").is_empty());
}

#[tokio::test]
async fn model_flag_is_passed_and_a_switch_restarts_grok() {
    let fake = Fake::new(&["handshake.script", "text_turn.script"]);
    let provider = fake.provider();
    let (_, err) = collect(&provider, request("grok-4.7", vec![Message::user("hi")])).await;
    assert_eq!(err, None);
    let starts = fake.starts();
    assert_eq!(
        starts[0][..4],
        ["--no-auto-update", "agent", "-m", "grok-4.7"]
    );

    // `-m` is fixed per process: another model means a new Grok (each start replays the
    // script), which gets the conversation so far.
    let (_, err) = collect(
        &provider,
        request(
            "grok-4.6",
            vec![
                Message::user("hi"),
                Message::assistant("Hello from Grok!"),
                Message::user("again"),
            ],
        ),
    )
    .await;
    assert_eq!(err, None);
    let starts = fake.starts();
    assert_eq!(starts.len(), 2);
    assert_eq!(starts[1][2..4], ["-m", "grok-4.6"]);
    let prompts = fake.sent_method("session/prompt");
    let text = prompts[1]["params"]["prompt"][0]["text"].as_str().unwrap();
    assert!(text.contains("Assistant: Hello from Grok!"), "{text}");
}

#[tokio::test]
async fn missing_binary_is_cli_not_found() {
    let fake = Fake::new(&[]);
    let mut config = fake.config();
    config.binary = fake.path("no-such-grok");
    let provider = GrokSession::new(config).unwrap();
    let (_, err) = collect(&provider, request("default", vec![Message::user("hi")])).await;
    let err = err.expect("an error");
    assert!(matches!(err, ProviderError::CliNotFound { .. }), "{err:?}");
    assert!(err.to_string().contains("ROZU_GROK_BIN"), "{err}");
}

#[tokio::test]
async fn agent_runs_a_grok_turn_without_cost() {
    let fake = Fake::new(&["handshake.script", "text_turn.script"]);
    let provider = Arc::new(fake.provider());
    let config = AgentConfig::new("default")
        .with_model_info(Some(ModelInfo::subscription("grok", "default", 500_000)));
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
        &Message::assistant("Hello from Grok!")
    );
    assert_eq!(session.usage.input_tokens, 4591);
    assert_eq!(session.usage.cache_read_tokens, 1152);
    assert_eq!(session.last_turn.cost_usd, None);
}

#[tokio::test]
async fn version_comes_from_the_binary() {
    let fake = Fake::new(&[]);
    assert_eq!(grok_version(&fake.path("grok")).await.unwrap(), "1.0.52");
}
