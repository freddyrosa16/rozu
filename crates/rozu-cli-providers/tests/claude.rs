//! `ClaudeCliProvider` against a fake `claude` binary that replays recorded stream-json.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use futures::StreamExt;
use rozu_cli_providers::claude::install::{claude_auth_status, claude_version};
use rozu_cli_providers::{ClaudeCliConfig, ClaudeCliProvider};
use rozu_core::{
    Agent, AgentConfig, AgentEvent, ApprovalMode, ApprovalRecord, ChatRequest, Decision,
    DecisionSource, Message, ModelInfo, Provider, ProviderError, ReadOnlyGate, SandboxMode,
    Session, StopReason, StreamEvent, TurnOutcome, Usage,
};
use rozu_mcp::SocketGate;
use rozu_tools::{Risk, ToolRegistry, Workspace};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/claude");

/// A scenario directory, a log directory and the fake binary.
struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    /// `turns[i]` is the fixture replayed for the (i+1)th user message.
    fn new(turns: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["turns", "log", "workspace"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let bin = dir.path().join("claude");
        std::fs::copy(Path::new(FIXTURES).join("fake_claude.sh"), &bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::copy(
            Path::new(FIXTURES).join("auth.json"),
            dir.path().join("turns/auth.json"),
        )
        .unwrap();
        let fake = Self { dir };
        for (i, fixture) in turns.iter().enumerate() {
            fake.turn_file(i + 1, fixture);
        }
        fake
    }

    fn turn_file(&self, n: usize, fixture: &str) {
        std::fs::copy(
            Path::new(FIXTURES).join(fixture),
            self.path(&format!("turns/turn{n}.jsonl")),
        )
        .unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn mark(&self, rel: &str, contents: &str) {
        std::fs::write(self.path(rel), contents).unwrap();
    }

    fn binary(&self) -> PathBuf {
        self.path("claude")
    }

    fn provider(&self) -> ClaudeCliProvider {
        let mut config = ClaudeCliConfig::new(
            self.binary(),
            self.path("workspace"),
            "/nonexistent/rozu",
            vec!["mcp-serve".into()],
            Arc::new(ReadOnlyGate),
        );
        config.env = vec![
            (
                "FAKE_CLAUDE_TURNS".into(),
                self.path("turns").display().to_string(),
            ),
            (
                "FAKE_CLAUDE_LOG".into(),
                self.path("log").display().to_string(),
            ),
        ];
        ClaudeCliProvider::new(config).unwrap()
    }

    /// argv of each start of the fake.
    fn starts(&self) -> Vec<Vec<String>> {
        let text = std::fs::read_to_string(self.path("log/args.txt")).unwrap_or_default();
        text.split("--\n")
            .filter(|s| !s.is_empty())
            .map(|s| s.lines().map(str::to_owned).collect())
            .collect()
    }

    fn stdin(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("log/stdin.jsonl"))
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
    provider: &ClaudeCliProvider,
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

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

#[tokio::test]
async fn tool_use_turn_maps_to_stream_events() {
    let fake = Fake::new(&["tool_use.jsonl"]);
    let provider = fake.provider();

    let (events, err) = collect(
        &provider,
        request(
            "sonnet",
            vec![
                Message::system("You are rozu."),
                Message::user("summarize README"),
            ],
        ),
    )
    .await;

    assert_eq!(err, None);
    assert_eq!(
        events,
        [
            StreamEvent::TextDelta("Let me ".into()),
            StreamEvent::TextDelta("look.".into()),
            StreamEvent::ToolCallStart {
                id: "toolu_01".into(),
                name: "read_file".into()
            },
            StreamEvent::ToolCallArgsDelta {
                id: "toolu_01".into(),
                json: "{\"path\": \"READ".into()
            },
            StreamEvent::ToolCallArgsDelta {
                id: "toolu_01".into(),
                json: "ME.md\"}".into()
            },
            StreamEvent::ToolCallEnd {
                id: "toolu_01".into()
            },
            StreamEvent::ToolResult {
                id: "toolu_01".into(),
                output: "# demo\nhello".into(),
                is_error: false
            },
            StreamEvent::TextDelta("The README says hello.".into()),
            // 7 uncached + 2100 cache writes + 20500 cache reads.
            StreamEvent::Usage(Usage {
                input_tokens: 22_607,
                output_tokens: 62,
                cache_read_tokens: 20_500,
                cache_write_tokens: 2_100,
            }),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );

    // The exact command line.
    let starts = fake.starts();
    assert_eq!(starts.len(), 1);
    let args = &starts[0];
    assert_eq!(
        args[..7],
        [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--verbose"
        ]
    );
    assert_eq!(arg_after(args, "--model"), Some("sonnet"));
    assert_eq!(arg_after(args, "--permission-mode"), Some("default"));
    assert_eq!(
        arg_after(args, "--permission-prompt-tool"),
        Some("mcp__rozu__approve")
    );
    assert!(args.contains(&"--strict-mcp-config".to_owned()));
    assert!(!args.iter().any(|a| a.contains("bypassPermissions")));
    assert!(!args.contains(&"--resume".to_owned()));
    let disallowed = arg_after(args, "--disallowedTools").unwrap();
    for tool in [
        "Bash",
        "Edit",
        "Write",
        "MultiEdit",
        "NotebookEdit",
        "WebFetch",
        "WebSearch",
        "Task",
    ] {
        assert!(
            disallowed.split(',').any(|t| t == tool),
            "{tool} in {disallowed}"
        );
    }
    // The prompt spans several lines of the argv log.
    let prompt = arg_after(args, "--append-system-prompt").unwrap();
    assert_eq!(prompt, "You are rozu.");
    let argv_log = std::fs::read_to_string(fake.path("log/args.txt")).unwrap();
    assert!(argv_log.contains("mcp__rozu__apply_patch"), "{argv_log}");

    // The MCP config names rozu's server and the approval socket.
    let config: Value =
        serde_json::from_slice(&std::fs::read(arg_after(args, "--mcp-config").unwrap()).unwrap())
            .unwrap();
    let server = &config["mcpServers"]["rozu"];
    assert_eq!(server["type"], "stdio");
    assert_eq!(server["command"], "/nonexistent/rozu");
    assert_eq!(server["args"], serde_json::json!(["mcp-serve"]));
    let socket = server["env"]["ROZU_APPROVAL_SOCKET"].as_str().unwrap();
    assert!(Path::new(socket).exists(), "{socket}");

    // The user turn as a stream-json input message.
    let stdin = fake.stdin();
    assert_eq!(stdin.len(), 1);
    assert_eq!(stdin[0]["type"], "user");
    assert_eq!(stdin[0]["message"]["role"], "user");
    assert_eq!(
        stdin[0]["message"]["content"][0]["text"],
        "summarize README"
    );
    // The subscription login is used, not an API key from the environment.
    let env = std::fs::read_to_string(fake.path("log/env.txt")).unwrap();
    assert_eq!(env.trim(), "ANTHROPIC_API_KEY=unset");
}

#[tokio::test]
async fn follow_up_turns_reuse_the_process_and_send_only_the_new_message() {
    let fake = Fake::new(&["tool_use.jsonl", "text.jsonl"]);
    let provider = fake.provider();
    let first = vec![Message::system("s"), Message::user("one")];
    collect(&provider, request("sonnet", first.clone())).await;
    let mut second = first;
    second.push(Message::assistant("The README says hello."));
    second.push(Message::user("two"));

    let (events, err) = collect(&provider, request("sonnet", second)).await;

    assert_eq!(err, None);
    assert_eq!(events[0], StreamEvent::TextDelta("Hello again!".into()));
    assert_eq!(fake.starts().len(), 1, "one long-lived process");
    let stdin = fake.stdin();
    assert_eq!(stdin.len(), 2);
    assert_eq!(stdin[1]["message"]["content"][0]["text"], "two");
}

#[tokio::test]
async fn not_logged_in_is_a_typed_error_with_the_login_hint() {
    let fake = Fake::new(&["not_logged_in.jsonl"]);
    let (_, err) = collect(
        &fake.provider(),
        request("sonnet", vec![Message::user("hi")]),
    )
    .await;
    let err = err.expect("an error");
    assert!(matches!(err, ProviderError::NotLoggedIn { .. }), "{err:?}");
    let text = err.to_string();
    assert!(
        text.contains("run `claude`") && text.contains("/login"),
        "{text}"
    );
}

#[tokio::test]
async fn cli_dying_with_a_login_message_is_not_logged_in() {
    let fake = Fake::new(&[]);
    fake.mark(
        "turns/stderr.txt",
        "Error: Not logged in. Please run /login\n",
    );
    let (_, err) = collect(
        &fake.provider(),
        request("sonnet", vec![Message::user("hi")]),
    )
    .await;
    assert!(
        matches!(err, Some(ProviderError::NotLoggedIn { .. })),
        "{err:?}"
    );
}

#[tokio::test]
async fn usage_limit_result_is_a_typed_error() {
    let fake = Fake::new(&["rate_limit.jsonl"]);
    let (_, err) = collect(&fake.provider(), request("opus", vec![Message::user("hi")])).await;
    match err {
        Some(ProviderError::UsageLimit { message }) => {
            assert!(message.contains("usage limit reached"), "{message}")
        }
        other => panic!("expected a usage limit, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_binary_is_cli_not_found() {
    let fake = Fake::new(&[]);
    let mut config = ClaudeCliConfig::new(
        fake.path("no-such-claude"),
        fake.path("workspace"),
        "rozu",
        Vec::new(),
        Arc::new(ReadOnlyGate),
    );
    config.env.clear();
    let provider = ClaudeCliProvider::new(config).unwrap();
    let err = provider
        .stream(
            request("sonnet", vec![Message::user("hi")]),
            CancellationToken::new(),
        )
        .await
        .err()
        .expect("an error");
    assert!(matches!(err, ProviderError::CliNotFound { .. }), "{err:?}");
    assert!(err.to_string().contains("ROZU_CLAUDE_BIN"), "{err}");
}

#[tokio::test]
async fn cancel_kills_the_cli_and_the_next_turn_resumes_the_session() {
    let fake = Fake::new(&["partial_then_hang.jsonl", "text.jsonl"]);
    fake.mark("turns/hang1", "");
    let provider = fake.provider();
    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(
            request("sonnet", vec![Message::user("one")]),
            cancel.clone(),
        )
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .unwrap();
    assert_eq!(
        first.unwrap().unwrap(),
        StreamEvent::TextDelta("Thinking about it".into())
    );
    cancel.cancel();
    let next = tokio::time::timeout(Duration::from_secs(10), stream.next())
        .await
        .unwrap();
    assert_eq!(next.unwrap(), Err(ProviderError::Cancelled));
    drop(stream);

    let (events, err) = collect(
        &provider,
        request("haiku", vec![Message::user("one"), Message::user("two")]),
    )
    .await;
    assert_eq!(err, None);
    assert_eq!(events[0], StreamEvent::TextDelta("Hello again!".into()));
    let starts = fake.starts();
    assert_eq!(starts.len(), 2, "restarted after the cancel");
    assert_eq!(arg_after(&starts[1], "--resume"), Some("sess-123"));
    assert_eq!(arg_after(&starts[1], "--model"), Some("haiku"));
}

#[tokio::test]
async fn approval_records_from_the_mcp_server_join_the_stream() {
    let fake = Fake::new(&["partial_then_hang.jsonl"]);
    fake.mark("turns/hang1", "");
    let provider = fake.provider();
    let cancel = CancellationToken::new();
    let mut stream = provider
        .stream(
            request("sonnet", vec![Message::user("one")]),
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
}

#[tokio::test]
async fn agent_runs_a_claude_turn_without_cost() {
    let fake = Fake::new(&["tool_use.jsonl"]);
    let provider = Arc::new(fake.provider());
    let workspace = Workspace::new(fake.path("workspace")).unwrap();
    let config = AgentConfig::new("sonnet").with_model_info(Some(ModelInfo::subscription(
        "claude-cli",
        "sonnet",
        200_000,
    )));
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::standard()),
        workspace,
        config,
    );
    let mut session = Session::with_system_prompt("You are rozu.");
    let (tx, mut rx) = mpsc::channel(64);
    let outcome = agent
        .run_turn(
            "summarize README",
            &mut session,
            &tx,
            CancellationToken::new(),
        )
        .await
        .unwrap();
    drop(tx);
    let mut events = Vec::new();
    while let Some(event) = rx.recv().await {
        events.push(event);
    }

    assert_eq!(outcome, TurnOutcome::Completed);
    assert!(events.contains(&AgentEvent::ToolCallStarted {
        id: "toolu_01".into(),
        name: "read_file".into(),
        args_json: "{\"path\": \"README.md\"}".into(),
    }));
    assert!(events.iter().any(|e| matches!(e,
        AgentEvent::ToolCallFinished { output, .. } if output == "# demo\nhello")));
    assert_eq!(
        session.messages.last().unwrap(),
        &Message::assistant("Let me look.The README says hello.")
    );
    assert_eq!(session.usage.cache_read_tokens, 20_500);
    assert_eq!(session.last_turn.cost_usd, None);
    let _ = ApprovalMode::default();
}

#[tokio::test]
async fn version_and_auth_status_read_only_what_they_need() {
    let fake = Fake::new(&[]);
    // `auth status` reads auth.json from the scenario dir.
    let bin = fake.binary();
    let wrapper = fake.path("claude-wrapper");
    std::fs::write(
        &wrapper,
        format!(
            "#!/bin/sh\nFAKE_CLAUDE_TURNS='{}' exec '{}' \"$@\"\n",
            fake.path("turns").display(),
            bin.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();

    assert_eq!(claude_version(&wrapper).await.unwrap(), "9.9.9");
    let status = claude_auth_status(&wrapper).await.unwrap();
    assert_eq!(status.logged_in, Some(true));
    assert_eq!(status.auth_method.as_deref(), Some("claude.ai"));
    assert!(!format!("{status:?}").contains("example.invalid"));
}
