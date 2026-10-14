#![cfg(unix)]

mod common;

use std::path::Path;
use std::sync::Arc;

use common::{
    HeldUser, assert_mcp_record_joins, cancel_after, collect, gate_record, next, request,
    run_agent_turn, socket_gate, start, start_err,
};
use rozu_cli_providers::claude::install::{claude_auth_status, claude_version};
use rozu_cli_providers::{ClaudeCliConfig, ClaudeCliProvider};
use rozu_core::{
    AgentEvent, ApprovalGate, ApprovalRequest, ApprovalResolution, Decision, Message, ModelInfo,
    PendingApproval, ProviderError, ReadOnlyGate, SandboxMode, StopReason, StreamEvent,
    TurnOutcome, Usage,
};
use rozu_tools::Risk;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

/// `turns[i]` is the fixture replayed for the (i+1)th user message.
fn fake(turns: &[&str]) -> common::Fake {
    let fake = common::Fake::new("claude", &["turns", "log", "workspace"]);
    std::fs::copy(fake.fixture("auth.json"), fake.path("turns/auth.json")).unwrap();
    for (i, fixture) in turns.iter().enumerate() {
        let to = fake.path(&format!("turns/turn{}.jsonl", i + 1));
        std::fs::copy(fake.fixture(fixture), to).unwrap();
    }
    fake
}

/// The first turn streams some text, then hangs until cancelled.
fn hanging_fake() -> common::Fake {
    let fake = fake(&["partial_then_hang.jsonl"]);
    fake.write("turns/hang1", "");
    fake
}

fn provider(fake: &common::Fake) -> ClaudeCliProvider {
    provider_with_gate(fake, Arc::new(ReadOnlyGate))
}

fn provider_with_gate(fake: &common::Fake, gate: Arc<dyn ApprovalGate>) -> ClaudeCliProvider {
    let mut config = ClaudeCliConfig::new(
        fake.binary(),
        fake.path("workspace"),
        "/nonexistent/rozu",
        vec!["mcp-serve".into()],
        gate,
    );
    config.env = vec![
        (
            "FAKE_CLAUDE_TURNS".into(),
            fake.path("turns").display().to_string(),
        ),
        (
            "FAKE_CLAUDE_LOG".into(),
            fake.path("log").display().to_string(),
        ),
    ];
    ClaudeCliProvider::new(config).unwrap()
}

fn arg_after<'a>(args: &'a [String], flag: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn shell_request(call_id: &str) -> ApprovalRequest {
    ApprovalRequest {
        call_id: call_id.into(),
        tool: "shell".into(),
        args: serde_json::json!({"command": "cargo test"}),
        risk: Risk::Network,
        preview: "$ cargo test".into(),
        sandbox: SandboxMode::Enforced("seatbelt"),
        network: true,
        extra_writes: Vec::new(),
        outside_unknown: false,
        blocked_retry: false,
        allow_rule: None,
    }
}

#[tokio::test]
async fn tool_use_turn_maps_to_stream_events() {
    let fake = fake(&["tool_use.jsonl"]);
    let messages = vec![
        Message::system("You are rozu."),
        Message::user("summarize README"),
    ];
    // Kept alive: dropping it removes the MCP config and socket checked below.
    let provider = provider(&fake);
    let (events, err) = collect(&provider, request("sonnet", messages)).await;

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
    assert_eq!(
        arg_after(args, "--append-system-prompt"),
        Some("You are rozu.")
    );
    // The appended prompt spans several lines of the argv log.
    let argv_log = fake.read("log/args.txt");
    assert!(argv_log.contains("mcp__rozu__apply_patch"), "{argv_log}");

    let config: Value =
        serde_json::from_slice(&std::fs::read(arg_after(args, "--mcp-config").unwrap()).unwrap())
            .unwrap();
    let server = &config["mcpServers"]["rozu"];
    assert_eq!(server["type"], "stdio");
    assert_eq!(server["command"], "/nonexistent/rozu");
    assert_eq!(server["args"], serde_json::json!(["mcp-serve"]));
    let socket = server["env"]["ROZU_APPROVAL_SOCKET"].as_str().unwrap();
    assert!(Path::new(socket).exists(), "{socket}");

    let stdin = fake.sent();
    assert_eq!(stdin.len(), 1);
    assert_eq!(stdin[0]["type"], "user");
    assert_eq!(stdin[0]["message"]["role"], "user");
    assert_eq!(
        stdin[0]["message"]["content"][0]["text"],
        "summarize README"
    );
    // The subscription login is used, not an API key from the environment.
    assert_eq!(fake.read("log/env.txt").trim(), "ANTHROPIC_API_KEY=unset");
}

#[tokio::test]
async fn follow_up_turns_reuse_the_process_and_send_only_the_new_message() {
    let fake = fake(&["tool_use.jsonl", "text.jsonl"]);
    let provider = provider(&fake);
    let first = vec![Message::system("s"), Message::user("one")];
    collect(&provider, request("sonnet", first.clone())).await;
    let mut second = first;
    second.push(Message::assistant("The README says hello."));
    second.push(Message::user("two"));

    let (events, err) = collect(&provider, request("sonnet", second)).await;

    assert_eq!(err, None);
    assert_eq!(events[0], StreamEvent::TextDelta("Hello again!".into()));
    assert_eq!(fake.starts().len(), 1, "one long-lived process");
    let stdin = fake.sent();
    assert_eq!(stdin.len(), 2);
    assert_eq!(stdin[1]["message"]["content"][0]["text"], "two");
}

#[tokio::test]
async fn not_logged_in_is_a_typed_error_with_the_login_hint() {
    let fake = fake(&["not_logged_in.jsonl"]);
    let (_, err) = collect(
        &provider(&fake),
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
    let fake = fake(&[]);
    fake.write(
        "turns/stderr.txt",
        "Error: Not logged in. Please run /login\n",
    );
    let (_, err) = collect(
        &provider(&fake),
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
    let fake = fake(&["rate_limit.jsonl"]);
    let (_, err) = collect(&provider(&fake), request("opus", vec![Message::user("hi")])).await;
    match err {
        Some(ProviderError::UsageLimit { message }) => {
            assert!(message.contains("usage limit reached"), "{message}")
        }
        other => panic!("expected a usage limit, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_binary_is_cli_not_found() {
    let fake = fake(&[]);
    let mut config = ClaudeCliConfig::new(
        fake.path("no-such-claude"),
        fake.path("workspace"),
        "rozu",
        Vec::new(),
        Arc::new(ReadOnlyGate),
    );
    config.env.clear();
    let err = start_err(&ClaudeCliProvider::new(config).unwrap(), "sonnet").await;
    assert!(matches!(err, ProviderError::CliNotFound { .. }), "{err:?}");
    assert!(err.to_string().contains("ROZU_CLAUDE_BIN"), "{err}");
}

#[tokio::test]
async fn cancel_kills_the_cli_and_the_next_turn_resumes_the_session() {
    let fake = hanging_fake();
    std::fs::copy(fake.fixture("text.jsonl"), fake.path("turns/turn2.jsonl")).unwrap();
    let provider = provider(&fake);
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "sonnet", &cancel).await;
    cancel_after(&mut stream, &cancel, "Thinking about it").await;
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
    let fake = hanging_fake();
    let provider = provider(&fake);
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "sonnet", &cancel).await;
    let _text = next(&mut stream).await;
    assert_mcp_record_joins(&mut stream, provider.runtime_dir(), "$ rm -rf build").await;
    cancel.cancel();
}

#[tokio::test]
async fn prompts_forwarded_by_the_mcp_server_are_reported_while_open() {
    let fake = hanging_fake();
    let user = HeldUser::new(Decision::Approve);
    let provider = provider_with_gate(&fake, user.clone());
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "sonnet", &cancel).await;
    let _text = next(&mut stream).await;

    let gate = socket_gate(provider.runtime_dir());
    let asking = tokio::spawn(async move { gate.decide(shell_request("mcp-7")).await });
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Ok(StreamEvent::ApprovalPending(PendingApproval {
            request_id: "mcp-7".into(),
            tool: "shell".into(),
            summary: "$ cargo test".into(),
            provider: "claude-cli".into(),
        }))
    );
    assert!(!asking.is_finished());

    user.release.notify_one();
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Ok(StreamEvent::ApprovalResolved {
            request_id: "mcp-7".into(),
            resolution: ApprovalResolution::Approved,
        })
    );
    assert_eq!(asking.await.unwrap(), Decision::Approve);
    cancel.cancel();
}

#[tokio::test]
async fn a_gate_that_answers_by_itself_opens_no_prompt() {
    let fake = hanging_fake();
    let provider = provider(&fake);
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "sonnet", &cancel).await;
    let _text = next(&mut stream).await;

    let gate = socket_gate(provider.runtime_dir());
    assert!(matches!(
        gate.decide(shell_request("mcp-8")).await,
        Decision::Deny { .. }
    ));
    // The next thing on the stream is the decision record, not a prompt.
    let record = gate_record("mcp-8", Risk::Network, "$ cargo test", true, "no one");
    gate.send_record(record.clone()).await;
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Ok(StreamEvent::Approval(record))
    );
    cancel.cancel();
}

#[tokio::test]
async fn agent_runs_a_claude_turn_without_cost() {
    let fake = fake(&["tool_use.jsonl"]);
    let turn = run_agent_turn(
        provider(&fake),
        fake.workspace(),
        ModelInfo::subscription("claude-cli", "sonnet", 200_000),
        "summarize README",
    )
    .await;

    assert_eq!(turn.outcome, TurnOutcome::Completed);
    assert!(turn.events.contains(&AgentEvent::ToolCallStarted {
        id: "toolu_01".into(),
        name: "read_file".into(),
        args_json: "{\"path\": \"README.md\"}".into(),
    }));
    assert!(turn.events.iter().any(|e| matches!(e,
        AgentEvent::ToolCallFinished { output, .. } if output == "# demo\nhello")));
    assert_eq!(
        turn.session.messages.last().unwrap(),
        &Message::assistant("Let me look.The README says hello.")
    );
    assert_eq!(turn.session.usage.cache_read_tokens, 20_500);
    assert_eq!(turn.session.last_turn.cost_usd, None);
}

#[tokio::test]
async fn version_and_auth_status_read_only_what_they_need() {
    let fake = fake(&[]);
    // `auth status` reads auth.json from the scenario dir.
    let wrapper = fake.wrapper(&[(
        "FAKE_CLAUDE_TURNS".into(),
        fake.path("turns").display().to_string(),
    )]);

    assert_eq!(claude_version(&wrapper).await.unwrap(), "9.9.9");
    let status = claude_auth_status(&wrapper).await.unwrap();
    assert_eq!(status.logged_in, Some(true));
    assert_eq!(status.auth_method.as_deref(), Some("claude.ai"));
    assert!(!format!("{status:?}").contains("example.invalid"));
}
