#![cfg(unix)]

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    Fake, HeldUser, approvals, assert_mcp_record_joins, cancel_after, collect, next, request,
    run_agent_turn, socket_gate, start, start_err,
};
use rozu_cli_providers::grok::install::grok_version;
use rozu_cli_providers::{EpochReason, GrokConfig, GrokSession};
use rozu_core::{
    AgentEvent, ApprovalGate, ApprovalMode, ApprovalRequest, ApprovalResolution, Decision,
    DecisionSource, Message, ModelInfo, PendingApproval, ProviderError, ReadOnlyGate, SandboxMode,
    StopReason, StreamEvent, TurnOutcome, Usage,
};
use rozu_tools::Risk;
use serde_json::json;
use tokio_util::sync::CancellationToken;

fn fake(scripts: &[&str]) -> Fake {
    Fake::scripted("grok", scripts)
}

fn config(fake: &Fake) -> GrokConfig {
    config_with_gate(fake, Arc::new(ReadOnlyGate))
}

fn config_with_gate(fake: &Fake, gate: Arc<dyn ApprovalGate>) -> GrokConfig {
    let mut config = GrokConfig::new(
        fake.binary(),
        fake.workspace(),
        "/opt/rozu/bin/rozu",
        vec![
            "mcp-serve".into(),
            "--session-id".into(),
            "s-test".into(),
            "--provider".into(),
            "grok".into(),
        ],
        fake.exec(gate, ApprovalMode::AutoWorkspace),
    );
    config.env = fake.env("FAKE_GROK");
    // Inherited from a user's shell: must never reach Grok.
    config
        .env
        .push(("XAI_API_KEY".into(), "xai-should-not-pass".into()));
    config.cancel_grace = Duration::from_secs(5);
    config.handshake_timeout = Duration::from_secs(10);
    config
}

fn provider(fake: &Fake) -> GrokSession {
    GrokSession::new(config(fake)).unwrap()
}

#[tokio::test]
async fn handshake_and_text_turn() {
    let fake = fake(&["handshake.script", "text_turn.script"]);
    let provider = provider(&fake);

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

    // No model flag for `default`.
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
    let written = fake.read("log/profile.md");
    assert!(written.contains("\ntools: use_tool\n"), "{written}");
    let env = fake.read("log/env.txt");
    assert!(env.contains("XAI_API_KEY=unset"), "{env}");
    assert!(env.contains("GROK_CLI_CHAT_PROXY_BASE_URL=unset"), "{env}");
    let cwd = fake.read("log/cwd.txt");
    assert_eq!(
        Path::new(cwd.trim()),
        std::fs::canonicalize(fake.path("workspace")).unwrap()
    );

    let init = &fake.sent_method("initialize")[0];
    assert_eq!(init["jsonrpc"], "2.0");
    assert_eq!(
        init["params"]["clientCapabilities"],
        json!({"fs": {"readTextFile": false, "writeTextFile": false}, "terminal": false})
    );
    let auth = fake.sent_method("authenticate");
    assert_eq!(auth.len(), 1);
    assert_eq!(auth[0]["params"], json!({"methodId": "cached_token"}));

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
    let fake = fake(&["handshake.script", "text_turn.script", "second_turn.script"]);
    let provider = provider(&fake);
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
    assert_eq!(fake.starts().len(), 1);
    assert_eq!(fake.sent_method("session/new").len(), 1);
    let prompts = fake.sent_method("session/prompt");
    assert_eq!(prompts[1]["params"]["prompt"][0]["text"], "two");
}

#[tokio::test]
async fn epoch_rotation_keeps_the_process_and_opens_a_new_session() {
    let fake = fake(&["handshake.script", "text_turn.script", "new_epoch.script"]);
    let provider = provider(&fake);
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
    let fake = fake(&["handshake.script", "tool_turn.script"]);
    let provider = provider(&fake);
    let (events, err) = collect(&provider, request("default", vec![Message::user("read")])).await;
    assert_eq!(err, None);

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

    // rozu's MCP tool: approved once (never "always"), delegated.
    assert_eq!(
        fake.reply("perm-1")["result"],
        json!({"outcome": {"outcome": "selected", "optionId": "allow-once"}})
    );
    assert_eq!(
        fake.reply("perm-2")["result"],
        json!({"outcome": {"outcome": "selected", "optionId": "reject-once"}})
    );
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

    assert!(
        !events.iter().any(|e| matches!(
            e,
            StreamEvent::ApprovalPending(_) | StreamEvent::ApprovalResolved { .. }
        )),
        "{events:?}"
    );
}

#[tokio::test]
async fn prompts_from_rozu_mcp_serve_are_reported_while_open() {
    let fake = fake(&["handshake.script", "cancel.script"]);
    let user = HeldUser::new(Decision::Approve);
    let provider = GrokSession::new(config_with_gate(&fake, user.clone())).unwrap();
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "default", &cancel).await;
    let _text = next(&mut stream).await;

    let gate = socket_gate(provider.runtime_dir());
    let asking = tokio::spawn(async move {
        gate.decide(ApprovalRequest {
            call_id: "mcp-3".into(),
            tool: "write_file".into(),
            args: json!({"path": "notes.md"}),
            risk: Risk::WriteWorkspace,
            preview: "notes.md\n+hello".into(),
            sandbox: SandboxMode::InProcess,
            network: false,
            extra_writes: Vec::new(),
            outside_unknown: false,
            blocked_retry: false,
            allow_rule: None,
        })
        .await
    });
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Ok(StreamEvent::ApprovalPending(PendingApproval {
            request_id: "mcp-3".into(),
            tool: "write_file".into(),
            summary: "notes.md".into(),
            provider: "grok".into(),
        }))
    );
    user.release.notify_one();
    assert_eq!(
        next(&mut stream).await.unwrap(),
        Ok(StreamEvent::ApprovalResolved {
            request_id: "mcp-3".into(),
            resolution: ApprovalResolution::Approved,
        })
    );
    assert_eq!(asking.await.unwrap(), Decision::Approve);
    cancel.cancel();
    let _ = next(&mut stream).await;
}

#[tokio::test]
async fn approval_records_from_rozu_mcp_serve_join_the_stream() {
    let fake = fake(&["handshake.script", "cancel.script"]);
    let provider = provider(&fake);
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "default", &cancel).await;
    let _text = next(&mut stream).await;
    assert_mcp_record_joins(&mut stream, provider.runtime_dir(), "$ git push --force").await;
    cancel.cancel();
    let _ = next(&mut stream).await;
}

#[tokio::test]
async fn cancel_sends_session_cancel_and_keeps_process_and_session() {
    let fake = fake(&["handshake.script", "cancel.script", "second_turn.script"]);
    let provider = provider(&fake);
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "default", &cancel).await;
    cancel_after(&mut stream, &cancel, "Working on it").await;
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
    let fake = fake(&["not_logged_in.script"]);
    let provider = provider(&fake);
    let err = start_err(&provider, "default").await;
    assert!(matches!(err, ProviderError::NotLoggedIn { .. }), "{err:?}");
    assert!(err.to_string().contains("grok login"), "{err}");
    assert!(fake.sent_method("session/new").is_empty());
}

#[tokio::test]
async fn other_auth_methods_are_never_used() {
    let fake = fake(&["no_cached_login.script"]);
    let provider = provider(&fake);
    let err = start_err(&provider, "default").await;
    assert!(matches!(err, ProviderError::NotLoggedIn { .. }), "{err:?}");
    assert!(fake.sent_method("authenticate").is_empty());
}

#[tokio::test]
async fn model_flag_is_passed_and_a_switch_restarts_grok() {
    let fake = fake(&["handshake.script", "text_turn.script"]);
    let provider = provider(&fake);
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
    let fake = fake(&[]);
    let mut config = config(&fake);
    config.binary = fake.path("no-such-grok");
    let provider = GrokSession::new(config).unwrap();
    let err = start_err(&provider, "default").await;
    assert!(matches!(err, ProviderError::CliNotFound { .. }), "{err:?}");
    assert!(err.to_string().contains("ROZU_GROK_BIN"), "{err}");
}

#[tokio::test]
async fn agent_runs_a_grok_turn_without_cost() {
    let fake = fake(&["handshake.script", "text_turn.script"]);
    let turn = run_agent_turn(
        provider(&fake),
        fake.workspace(),
        ModelInfo::subscription("grok", "default", 500_000),
        "hi",
    )
    .await;
    assert_eq!(turn.outcome, TurnOutcome::Completed);
    assert!(
        turn.events
            .contains(&AgentEvent::TextDelta("Hello ".into()))
    );
    assert_eq!(
        turn.session.messages.last().unwrap(),
        &Message::assistant("Hello from Grok!")
    );
    assert_eq!(turn.session.usage.input_tokens, 4591);
    assert_eq!(turn.session.usage.cache_read_tokens, 1152);
    assert_eq!(turn.session.last_turn.cost_usd, None);
}

#[tokio::test]
async fn version_comes_from_the_binary() {
    let fake = fake(&[]);
    assert_eq!(grok_version(&fake.binary()).await.unwrap(), "1.0.52");
}
