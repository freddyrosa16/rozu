#![cfg(unix)]

mod common;

use std::path::Path;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use common::{
    Fake, HeldUser, approvals, assert_mcp_record_joins, cancel_after, collect, next, request,
    run_agent_turn, start, start_err,
};
use rozu_cli_providers::codex::install::{
    LoginStatus, codex_login_status, codex_version, list_models,
};
use rozu_cli_providers::{CodexAppServerProvider, CodexConfig};
use rozu_core::{
    AgentEvent, ApprovalGate, ApprovalMode, ApprovalRequest, ApprovalResolution, Decision,
    DecisionSource, Message, ModelInfo, Provider, ProviderError, SandboxMode, StopReason,
    StreamEvent, TurnOutcome, Usage,
};
use rozu_tools::Risk;
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Approves what `approve` accepts (with `u`, the strongest answer on the card), denies the
/// rest, and remembers what it was asked.
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

fn fake(scripts: &[&str]) -> Fake {
    Fake::scripted("codex", scripts)
}

fn provider_with(
    fake: &Fake,
    gate: Arc<dyn ApprovalGate>,
    mode: ApprovalMode,
) -> CodexAppServerProvider {
    let mut config = CodexConfig::new(
        fake.binary(),
        fake.workspace(),
        "/nonexistent/rozu",
        vec!["mcp-serve".into(), "--provider".into(), "codex".into()],
        fake.exec(gate, mode),
    );
    config.env = fake.env("FAKE_CODEX");
    config.interrupt_grace = std::time::Duration::from_secs(5);
    CodexAppServerProvider::new(config).unwrap()
}

fn provider(fake: &Fake) -> CodexAppServerProvider {
    provider_with(fake, TestGate::new(|_| false), ApprovalMode::AutoWorkspace)
}

fn override_value<'a>(args: &'a [String], key: &str) -> Option<&'a str> {
    args.windows(2)
        .filter(|w| w[0] == "-c")
        .find_map(|w| w[1].strip_prefix(&format!("{key}=")))
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

    let starts = fake.starts();
    assert_eq!(starts.len(), 1);
    let args = &starts[0];
    assert_eq!(args.last().map(String::as_str), Some("app-server"));
    assert_eq!(
        override_value(args, "approval_policy"),
        Some("\"on-request\"")
    );
    assert_eq!(override_value(args, "sandbox_mode"), Some("\"read-only\""));
    assert_eq!(
        override_value(args, "sandbox_workspace_write.network_access"),
        Some("false")
    );
    assert_eq!(
        override_value(args, "sandbox_workspace_write.writable_roots"),
        Some("[]")
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
    let forwarded: Vec<String> =
        serde_json::from_str(override_value(args, "mcp_servers.rozu.env_vars").unwrap()).unwrap();
    assert_eq!(
        forwarded,
        [
            "ROZU_AUDIT_LOG",
            "ROZU_LOG_FILE",
            "ROZU_LOG_STDERR",
            "RUST_LOG"
        ]
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

    let init = &fake.sent_method("initialize")[0];
    assert_eq!(init["params"]["clientInfo"]["name"], "rozu");
    assert_eq!(fake.sent_method("initialized").len(), 1);
    assert_eq!(
        fake.sent_method("account/read")[0]["params"],
        json!({"refreshToken": false})
    );
    let thread = &fake.sent_method("thread/start")[0]["params"];
    let root = fake.workspace().root().display().to_string();
    assert_eq!(thread["cwd"], root.as_str());
    assert_eq!(thread["approvalPolicy"], "untrusted");
    assert_eq!(thread["approvalsReviewer"], "user");
    assert_eq!(thread["sandbox"], "workspace-write");
    assert_eq!(thread["ephemeral"], true);
    assert!(thread.get("model").is_none(), "default model: Codex picks");
    let instructions = thread["developerInstructions"].as_str().unwrap();
    assert!(instructions.starts_with("You are rozu."), "{instructions}");
    assert!(instructions.contains("`rozu` MCP"), "{instructions}");
    let turn = &fake.sent_method("turn/start")[0]["params"];
    assert_eq!(turn["threadId"], "thr_1");
    assert_eq!(turn["approvalPolicy"], "untrusted");
    assert_eq!(turn["approvalsReviewer"], "user");
    assert_eq!(
        turn["input"],
        json!([{"type": "text", "text": "hi", "text_elements": []}])
    );
    assert!(turn.get("model").is_none());

    let cwd = fake.read("log/cwd.txt");
    assert_eq!(
        Path::new(cwd.trim()).canonicalize().unwrap(),
        fake.workspace().root()
    );
    assert_eq!(
        fake.read("log/env.txt").trim(),
        "OPENAI_API_KEY=unset\nCODEX_API_KEY=unset"
    );
}

#[tokio::test]
async fn follow_up_turns_reuse_the_thread_and_report_their_own_usage() {
    let fake = fake(&["handshake.script", "text_turn.script", "second_turn.script"]);
    let provider = provider(&fake);
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
    let turns = fake.sent_method("turn/start");
    assert_eq!(turns.len(), 2);
    assert_eq!(turns[1]["params"]["input"][0]["text"], "two");
    assert_eq!(turns[1]["params"]["model"], "gpt-6-luna");
    for turn in turns {
        assert_eq!(turn["params"]["approvalPolicy"], "untrusted");
        assert_eq!(turn["params"]["approvalsReviewer"], "user");
        assert!(turn["params"].get("sandboxPolicy").is_none());
    }
}

#[tokio::test]
async fn refuses_unconfirmed_thread_permissions_before_starting_a_turn() {
    for (from, to) in [
        (
            "\"approvalPolicy\":\"untrusted\"",
            "\"approvalPolicy\":\"on-request\"",
        ),
        ("\"approvalPolicy\":\"untrusted\",", ""),
        (
            "\"approvalsReviewer\":\"user\"",
            "\"approvalsReviewer\":\"auto_review\"",
        ),
        ("\"approvalsReviewer\":\"user\",", ""),
        (
            "\"type\":\"workspaceWrite\"",
            "\"type\":\"dangerFullAccess\"",
        ),
        ("\"type\":\"workspaceWrite\",", ""),
        ("\"networkAccess\":false", "\"networkAccess\":true"),
        ("\"networkAccess\":false,", ""),
        ("\"writableRoots\":[]", "\"writableRoots\":[\"/outside\"]"),
        ("\"writableRoots\":[],", ""),
        ("\"writableRoots\":[]", "\"writableRoots\":[false]"),
    ] {
        let fake = fake(&["handshake_0162.script"]);
        let script = fake.read("script.txt");
        assert!(script.contains(from), "{from}");
        fake.write("script.txt", &script.replacen(from, to, 1));
        let err = start_err(&provider(&fake), "default").await;
        assert!(
            matches!(err, ProviderError::Api { ref code, .. }
            if code.as_deref() == Some("unsafe_codex_session")),
            "{err:?}"
        );
        assert!(fake.sent_method("turn/start").is_empty(), "{from}");
    }
}

#[tokio::test]
async fn refuses_persistent_or_unconfirmed_threads_before_starting_a_turn() {
    for replacement in [
        "\"ephemeral\":false",
        "\"ephemeral\":null",
        "\"ephemeral\":\"true\"",
        "\"unused\":true",
    ] {
        let fake = fake(&["handshake_0162.script", "text_turn.script"]);
        let script = fake.read("script.txt");
        assert!(script.contains("\"ephemeral\":true"));
        fake.write(
            "script.txt",
            &script.replacen("\"ephemeral\":true", replacement, 1),
        );
        let err = start_err(&provider(&fake), "default").await;
        assert!(
            matches!(err, ProviderError::Api { ref code, .. }
            if code.as_deref() == Some("persistent_codex_session")),
            "{err:?}"
        );
        assert!(fake.sent_method("turn/start").is_empty(), "{replacement}");
    }
}

#[tokio::test]
async fn reopening_a_chat_replays_history_into_a_new_ephemeral_thread() {
    let fake = fake(&["handshake_0162.script", "text_turn.script"]);
    let first = provider(&fake);
    let (events, err) = collect(
        &first,
        request("default", vec![Message::user("remember the blue bird")]),
    )
    .await;
    assert_eq!(err, None);
    assert!(events.contains(&StreamEvent::Done(StopReason::EndTurn)));
    drop(first);

    let (_, err) = collect(
        &provider(&fake),
        request(
            "default",
            vec![
                Message::user("remember the blue bird"),
                Message::assistant("Hello from Codex!"),
                Message::user("what did I ask you to remember?"),
            ],
        ),
    )
    .await;
    assert_eq!(err, None);
    assert_eq!(fake.starts().len(), 2);
    assert!(fake.sent_method("thread/resume").is_empty());
    assert!(
        fake.sent_method("thread/start")
            .iter()
            .all(|message| message["params"]["ephemeral"] == true)
    );
    let turns = fake.sent_method("turn/start");
    let text = turns[1]["params"]["input"][0]["text"].as_str().unwrap();
    assert!(text.contains("User: remember the blue bird"));
    assert!(text.contains("Assistant: Hello from Codex!"));
    assert!(text.ends_with("what did I ask you to remember?"));
}

#[tokio::test]
async fn codex_0162_thread_policy_is_confirmed_before_the_turn() {
    let fake = fake(&["handshake_0162.script", "text_turn.script"]);
    let (events, err) = collect(
        &provider(&fake),
        request("default", vec![Message::user("hi")]),
    )
    .await;
    assert_eq!(err, None);
    assert!(events.contains(&StreamEvent::Done(StopReason::EndTurn)));
    assert_eq!(fake.sent_method("turn/start").len(), 1);
}

#[tokio::test]
async fn codex_accepts_only_the_workspace_as_an_explicit_writable_root() {
    let fake = fake(&["handshake_0162.script", "text_turn.script"]);
    let roots = json!([fake.workspace().root()]);
    fake.write(
        "script.txt",
        &fake.read("script.txt").replace(
            "\"writableRoots\":[]",
            &format!("\"writableRoots\":{roots}"),
        ),
    );
    let (_, err) = collect(
        &provider(&fake),
        request("default", vec![Message::user("hi")]),
    )
    .await;
    assert_eq!(err, None);
    assert_eq!(fake.sent_method("turn/start").len(), 1);
}

#[tokio::test]
async fn command_approvals_go_through_rozu_policy() {
    let fake = fake(&["handshake.script", "exec_approvals.script"]);
    let gate = TestGate::new(|req| req.preview.contains("curl"));
    let provider = provider_with(&fake, gate.clone(), ApprovalMode::AutoWorkspace);

    let (events, err) = collect(&provider, request("default", vec![Message::user("go")])).await;
    assert_eq!(err, None);

    for (id, decision) in [
        // cargo test: a workspace command under Codex's sandbox, auto-approved.
        ("srv-1", "accept"),
        // Force push: destructive, the user said no.
        ("srv-2", "decline"),
        // curl: the user approved; a one-time accept, never for the session.
        ("srv-3", "accept"),
        // Legacy exec request for a read.
        ("srv-4", "approved"),
        // An escalation (Codex gives a reason): always the user, even for `ls`.
        ("srv-5", "decline"),
        // A working directory outside the workspace.
        ("srv-6", "decline"),
    ] {
        assert_eq!(
            fake.reply(id)["result"],
            json!({ "decision": decision }),
            "{id}"
        );
    }

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

    // Only the four questions the user saw opened a prompt, each closed before its record.
    let prompts: Vec<_> = events
        .iter()
        .filter_map(|e| match e {
            StreamEvent::ApprovalPending(p) => Some((p.request_id.clone(), None)),
            StreamEvent::ApprovalResolved {
                request_id,
                resolution,
            } => Some((request_id.clone(), Some(*resolution))),
            _ => None,
        })
        .collect();
    let mut expected = Vec::new();
    for (req, resolution) in asked.iter().zip([
        ApprovalResolution::Denied,
        ApprovalResolution::Approved,
        ApprovalResolution::Denied,
        ApprovalResolution::Denied,
    ]) {
        expected.push((req.call_id.clone(), None));
        expected.push((req.call_id.clone(), Some(resolution)));
    }
    assert_eq!(prompts, expected);
    let pending = events
        .iter()
        .find_map(|e| match e {
            StreamEvent::ApprovalPending(p) => Some(p),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (pending.tool.as_str(), pending.provider.as_str()),
        ("codex:shell", "codex")
    );
    assert_eq!(pending.summary, "$ git push --force origin main");
    let resolved_at = events
        .iter()
        .position(|e| matches!(e, StreamEvent::ApprovalResolved { .. }))
        .unwrap();
    let record_at = events
        .iter()
        .position(|e| matches!(e, StreamEvent::Approval(r) if r.source == DecisionSource::Gate))
        .unwrap();
    assert!(resolved_at < record_at);

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
async fn ambiguous_commands_reach_the_gate_even_in_full_mode() {
    let fake = fake(&["handshake.script", "unsafe_exec.script"]);
    let gate = TestGate::new(|_| false);
    let provider = provider_with(&fake, gate.clone(), ApprovalMode::FullAccess);
    let (events, err) = collect(&provider, request("default", vec![Message::user("go")])).await;
    assert_eq!(err, None);
    let asked = gate.asked();
    assert_eq!(asked.len(), 6);
    assert!(
        asked
            .iter()
            .all(|req| req.risk >= Risk::Opaque && req.allow_rule.is_none())
    );
    let records = approvals(&events);
    assert_eq!(records.len(), 6);
    assert!(
        records
            .iter()
            .all(|record| !record.decision.is_approval() && record.source == DecisionSource::Gate)
    );
    for id in 1..=5 {
        assert_eq!(
            fake.reply(&format!("unsafe-{id}"))["result"]["decision"],
            "decline"
        );
    }
    assert!(
        fake.reply("unsafe-6")["result"]["decision"]
            .get("denied")
            .is_some()
    );
}

/// The prompt is on the stream while the user is still thinking, not after the answer.
#[tokio::test]
async fn codex_prompts_are_reported_before_the_user_answers() {
    let fake = fake(&["handshake.script", "exec_approvals.script"]);
    let user = HeldUser::new(Decision::Deny {
        reason: "later".into(),
    });
    let provider = provider_with(&fake, user.clone(), ApprovalMode::AutoWorkspace);
    let mut stream = provider
        .stream(
            request("default", vec![Message::user("go")]),
            CancellationToken::new(),
        )
        .await
        .unwrap();
    let mut prompts = 0;
    // A prompt that only showed after the answer would deadlock (and time out) here.
    while let Some(item) = next(&mut stream).await {
        if let StreamEvent::ApprovalPending(pending) = item.unwrap() {
            prompts += 1;
            if prompts == 1 {
                assert_eq!(pending.summary, "$ git push --force origin main");
                // Codex is still waiting for rozu's answer to srv-2.
                assert!(
                    !fake
                        .sent()
                        .iter()
                        .any(|m| m["id"] == "srv-2" && m.get("method").is_none())
                );
            }
            user.release.notify_one();
        }
    }
    assert_eq!(prompts, 4);
}

#[tokio::test]
async fn read_only_mode_runs_codex_read_only_and_asks_for_workspace_commands() {
    let fake = fake(&["handshake_read_only.script", "exec_approvals.script"]);
    let gate = TestGate::new(|_| false);
    let provider = provider_with(&fake, gate.clone(), ApprovalMode::ReadOnly);
    collect(&provider, request("default", vec![Message::user("go")])).await;

    let args = &fake.starts()[0];
    assert_eq!(override_value(args, "sandbox_mode"), Some("\"read-only\""));
    assert_eq!(
        fake.sent_method("thread/start")[0]["params"]["sandbox"],
        "read-only"
    );
    assert_eq!(
        fake.reply("srv-1")["result"],
        json!({"decision": "decline"})
    );
    assert_eq!(gate.asked()[0].risk, Risk::WriteWorkspace);
}

#[tokio::test]
async fn patch_approvals_check_every_path() {
    let fake = fake(&["handshake.script", "patch_approvals.script"]);
    let gate = TestGate::new(|_| false);
    let provider = provider_with(&fake, gate.clone(), ApprovalMode::AutoWorkspace);

    let (events, err) = collect(&provider, request("default", vec![Message::user("go")])).await;
    assert_eq!(err, None);

    for (id, decision) in [
        // A new file in the workspace; the diff came from the fileChange item.
        ("srv-1", "accept"),
        // .rozu/ is the user's: denied without asking.
        ("srv-2", "decline"),
        // Deleting a file: destructive, the user said no.
        ("srv-4", "decline"),
        // An item rozu never saw: it can't tell what changes.
        ("srv-5", "decline"),
    ] {
        assert_eq!(
            fake.reply(id)["result"],
            json!({ "decision": decision }),
            "{id}"
        );
    }
    // Legacy patch outside the workspace: the user said no.
    let legacy = &fake.reply("srv-3")["result"]["decision"]["denied"]["rejection"];
    assert!(
        legacy.as_str().unwrap().contains("not this one"),
        "{legacy}"
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
    let fake = fake(&["handshake.script", "unmapped.script"]);
    let gate = TestGate::new(|_| false);
    let provider = provider_with(&fake, gate.clone(), ApprovalMode::FullAccess);

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
    let fake = fake(&["handshake.script", "elicitations.script"]);
    // A user who would approve anything: none of this may reach them.
    let gate = TestGate::new(|_| true);
    let provider = provider_with(&fake, gate.clone(), ApprovalMode::AutoWorkspace);

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
    let fake = fake(&["not_logged_in.script"]);
    let err = start_err(&provider(&fake), "default").await;
    assert!(matches!(err, ProviderError::NotLoggedIn { .. }), "{err:?}");
    let text = err.to_string();
    assert!(text.contains("run `rozu auth login codex`"), "{text}");
    assert!(!text.contains("example.invalid"));
    assert!(fake.sent_method("thread/start").is_empty());
}

#[tokio::test]
async fn cancel_interrupts_the_turn_and_keeps_the_app_server() {
    let fake = fake(&["handshake.script", "interrupt.script", "second_turn.script"]);
    let provider = provider(&fake);
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "default", &cancel).await;
    cancel_after(&mut stream, &cancel, "Working on it").await;
    drop(stream);

    let interrupt = &fake.sent_method("turn/interrupt")[0]["params"];
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
    let fake = fake(&["handshake.script", "interrupt.script"]);
    let provider = provider(&fake);
    let cancel = CancellationToken::new();
    let mut stream = start(&provider, "default", &cancel).await;
    let _text = next(&mut stream).await;

    assert_mcp_record_joins(&mut stream, provider.runtime_dir(), "$ rm -rf build").await;
    cancel.cancel();
    let _ = next(&mut stream).await;
}

#[tokio::test]
async fn missing_binary_is_cli_not_found() {
    let fake = fake(&[]);
    let config = CodexConfig::new(
        fake.path("no-such-codex"),
        fake.workspace(),
        "rozu",
        Vec::new(),
        fake.exec(TestGate::new(|_| false), ApprovalMode::AutoWorkspace),
    );
    let err = start_err(&CodexAppServerProvider::new(config).unwrap(), "default").await;
    assert!(matches!(err, ProviderError::CliNotFound { .. }), "{err:?}");
    assert!(err.to_string().contains("ROZU_CODEX_BIN"), "{err}");
}

#[tokio::test]
async fn agent_runs_a_codex_turn_without_cost() {
    let fake = fake(&["handshake.script", "text_turn.script"]);
    let turn = run_agent_turn(
        provider(&fake),
        fake.workspace(),
        ModelInfo::subscription("codex", "default", 272_000),
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
        &Message::assistant("Hello from Codex!")
    );
    assert_eq!(turn.session.usage.cache_read_tokens, 1000);
    assert_eq!(turn.session.last_turn.cost_usd, None);
}

#[tokio::test]
async fn version_login_status_and_model_list() {
    let fake = fake(&["models.script"]);
    assert_eq!(codex_version(&fake.binary()).await.unwrap(), "0.147.0");

    let status = |text: &str| fake.wrapper(&[("FAKE_CODEX_LOGIN_STATUS".into(), text.into())]);
    assert_eq!(
        codex_login_status(&status("Logged in using ChatGPT"))
            .await
            .unwrap(),
        LoginStatus::ChatGpt
    );
    assert_eq!(
        codex_login_status(&status("Not logged in")).await.unwrap(),
        LoginStatus::NotLoggedIn
    );

    let models = fake.wrapper(&fake.env("FAKE_CODEX"));
    let list = list_models(&models).await.unwrap();
    let ids: Vec<&str> = list.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(
        ids,
        ["gpt-6.1-sol", "gpt-6-luna"],
        "hidden models are left out"
    );
    assert!(list[0].is_default);
}
