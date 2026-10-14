mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use common::*;
use rozu_core::{
    Agent, AgentConfig, AgentError, AgentEvent, ApprovalGate, ApprovalMode, ApprovalPolicy,
    ApprovalRecord, ApprovalRequest, ApprovalResolution, ApprovalRules, Billing, Decision,
    DecisionSource, LongContextPrices, Message, ModelInfo, PendingApproval, PrefixRule,
    ProviderError, Role, SandboxMode, Session, StopReason, StreamEvent, TurnOutcome, Usage,
    approvals_path,
};
use rozu_tools::{
    AllowRule, NoSandbox, Risk, Shell, Tool, ToolCtx, ToolOutput, ToolRegistry, Workspace,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

enum Fake {
    /// Returns its arguments as JSON text.
    Echo,
    Slow,
    Big,
    /// Records whether it ran.
    Write(Arc<AtomicBool>),
}

#[async_trait]
impl Tool for Fake {
    fn name(&self) -> &'static str {
        match self {
            Fake::Echo => "echo",
            Fake::Slow => "slow",
            Fake::Big => "big",
            Fake::Write(_) => "write",
        }
    }
    fn description(&self) -> &'static str {
        "fake"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn risk(&self, _: &Value, _: &ToolCtx) -> Risk {
        match self {
            Fake::Write(_) => Risk::WriteWorkspace,
            _ => Risk::Read,
        }
    }
    async fn call(&self, args: Value, _: &ToolCtx) -> ToolOutput {
        match self {
            Fake::Echo => ToolOutput::ok(args.to_string()),
            Fake::Slow => {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                ToolOutput::ok("woke up")
            }
            Fake::Big => ToolOutput::ok(format!("HEAD{}TAIL", "x".repeat(100_000))),
            Fake::Write(ran) => {
                ran.store(true, Ordering::SeqCst);
                ToolOutput::ok("wrote")
            }
        }
    }
}

struct Harness {
    dir: tempfile::TempDir,
    provider: Arc<ScriptedProvider>,
    agent: Agent,
    session: Session,
}

impl Harness {
    fn new(scripts: impl IntoIterator<Item = Script>) -> Self {
        Self::with_config(scripts, |_| {}, |_| {})
    }

    fn with_config(
        scripts: impl IntoIterator<Item = Script>,
        configure: impl FnOnce(&mut AgentConfig),
        register: impl FnOnce(&mut ToolRegistry),
    ) -> Self {
        Self::build(scripts, configure, register, |agent| agent)
    }

    fn build(
        scripts: impl IntoIterator<Item = Script>,
        configure: impl FnOnce(&mut AgentConfig),
        register: impl FnOnce(&mut ToolRegistry),
        customize: impl FnOnce(Agent) -> Agent,
    ) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let provider = ScriptedProvider::new(scripts);
        let mut tools = ToolRegistry::new();
        tools.register(Fake::Echo);
        tools.register(Fake::Slow);
        tools.register(Fake::Big);
        register(&mut tools);
        let mut config = AgentConfig::new("test-model");
        configure(&mut config);
        let agent = customize(Agent::new(
            provider.clone(),
            Arc::new(tools),
            Workspace::new(dir.path()).unwrap(),
            config,
        ));
        Self {
            dir,
            provider,
            agent,
            session: Session::with_system_prompt("be brief"),
        }
    }

    async fn run_with(
        &mut self,
        input: &str,
        on_event: impl FnMut(&AgentEvent, &CancellationToken),
    ) -> (Result<TurnOutcome, AgentError>, Vec<AgentEvent>) {
        run_turn(&self.agent, &mut self.session, input, on_event).await
    }

    async fn run(&mut self, input: &str) -> (Result<TurnOutcome, AgentError>, Vec<AgentEvent>) {
        self.run_with(input, |_, _| {}).await
    }

    fn last(&self) -> &Message {
        self.session.messages.last().unwrap()
    }

    fn tool_results(&self) -> Vec<(String, String, bool)> {
        tool_results(&self.session)
    }

    fn last_cost(&self) -> f64 {
        self.session.last_turn.cost_usd.unwrap()
    }
}

fn shell_harness(steps: Vec<Script>, mode: ApprovalMode, gate: Arc<ScriptedGate>) -> Harness {
    Harness::build(
        steps,
        |_| {},
        |tools| tools.register(Shell),
        |agent| {
            let policy = ApprovalPolicy::for_project(mode, agent.workspace().root()).unwrap();
            // No OS sandbox, so the test behaves the same on every platform.
            agent
                .with_approval_policy(Arc::new(policy))
                .with_approval_gate(gate)
                .with_sandbox(Arc::new(NoSandbox))
        },
    )
}

fn prompts(events: &[AgentEvent]) -> Vec<AgentEvent> {
    events
        .iter()
        .filter(|e| {
            matches!(
                e,
                AgentEvent::ApprovalPending(_) | AgentEvent::ApprovalResolved { .. }
            )
        })
        .cloned()
        .collect()
}

fn usage(input: u64, output: u64, read: u64, write: u64) -> Usage {
    Usage {
        input_tokens: input,
        output_tokens: output,
        cache_read_tokens: read,
        cache_write_tokens: write,
    }
}

fn metered(price_in: f64, price_out: f64, price_read: f64, price_write: f64) -> ModelInfo {
    ModelInfo {
        id: "test-model".into(),
        provider: "test".into(),
        context_window: 1_000_000,
        supports_tools: true,
        price_in_per_mtok: price_in,
        price_out_per_mtok: price_out,
        price_cached_in_per_mtok: price_read,
        price_cache_write_per_mtok: price_write,
        long_context: None,
        billing: Billing::Metered,
        caps: Default::default(),
    }
}

#[track_caller]
fn assert_close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() < 1e-9, "{actual} != {expected}");
}

#[tokio::test]
async fn plain_text_turn_completes() {
    let usage = usage(10, 2, 0, 0);
    let mut h = Harness::new([ok(vec![
        text("Hel"),
        text("lo"),
        StreamEvent::Usage(usage),
        StreamEvent::Done(StopReason::EndTurn),
    ])]);

    let (result, events) = h.run("hi").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    assert_eq!(
        events,
        [
            AgentEvent::TextDelta("Hel".into()),
            AgentEvent::TextDelta("lo".into()),
            AgentEvent::Usage(usage),
            AgentEvent::TurnFinished(TurnOutcome::Completed),
        ]
    );
    let roles: Vec<_> = h.session.messages.iter().map(|m| m.role).collect();
    assert_eq!(roles, [Role::System, Role::User, Role::Assistant]);
    assert_eq!(h.last().text_content(), "Hello");
    assert_eq!(h.session.usage, usage);

    let requests = h.provider.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].model, "test-model");
    let tool_names: Vec<_> = requests[0].tools.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(tool_names, ["big", "echo", "slow"]);
}

#[tokio::test]
async fn usage_and_cost_aggregate_per_turn() {
    let step = |u| {
        ok(vec![
            StreamEvent::Usage(u),
            StreamEvent::Done(StopReason::EndTurn),
        ])
    };
    let mut h = Harness::with_config(
        [
            tool_step(&[("call_1", "echo", r#"{"path":"a"}"#)]),
            step(usage(1_000_000, 0, 0, 0)),
            step(usage(1_000_000, 100_000, 1_000_000, 0)),
            step(usage(1_000_000, 0, 0, 1_000_000)),
        ],
        |config| config.model_info = Some(metered(2.0, 10.0, 0.5, 2.5)),
        |_| {},
    );

    // A tool step without usage, then 1M uncached input.
    h.run("first").await.0.unwrap();
    assert_eq!(h.session.last_turn.usage.input_tokens, 1_000_000);
    assert_close(h.last_cost(), 2.0);

    // 1M cached input + 100k output = 0.5 + 1.0.
    h.run("second").await.0.unwrap();
    assert_eq!(
        h.session.last_turn.usage,
        usage(1_000_000, 100_000, 1_000_000, 0)
    );
    assert_close(h.last_cost(), 1.5);
    assert_eq!(h.session.usage.input_tokens, 2_000_000);
    assert_close(h.session.cost_usd, 3.5);

    h.run("third").await.0.unwrap();
    assert_eq!(h.session.last_turn.usage.cache_write_tokens, 1_000_000);
    assert_close(h.last_cost(), 2.5);
    assert_eq!(h.session.usage.cache_read_tokens, 1_000_000);
    assert_close(h.session.cost_usd, 6.0);
}

#[tokio::test]
async fn long_context_tier_applies_per_response_not_per_turn() {
    let input = |n| StreamEvent::Usage(usage(n, 0, 0, 0));
    let mut tool = vec![input(150_000)];
    tool.extend(tool_call("call_1", "echo", r#"{"path":"a"}"#));
    tool.push(StreamEvent::Done(StopReason::ToolUse));
    let mut h = Harness::with_config(
        [
            ok(tool),
            ok(vec![input(150_000), StreamEvent::Done(StopReason::EndTurn)]),
            ok(vec![input(250_000), StreamEvent::Done(StopReason::EndTurn)]),
        ],
        |config| {
            config.model_info = Some(metered(1.0, 1.0, 1.0, 1.0).with_long_context(
                LongContextPrices {
                    threshold_tokens: 200_000,
                    price_in_per_mtok: 2.0,
                    price_out_per_mtok: 2.0,
                    price_cached_in_per_mtok: 2.0,
                    price_cache_write_per_mtok: 2.0,
                },
            ));
        },
        |_| {},
    );

    // 300k tokens in the turn, but neither 150k request is long.
    h.run("first").await.0.unwrap();
    assert_eq!(h.session.last_turn.usage.input_tokens, 300_000);
    assert_close(h.last_cost(), 0.3);

    h.run("second").await.0.unwrap();
    assert_close(h.last_cost(), 0.5);
}

#[tokio::test]
async fn unknown_prices_leave_cost_empty() {
    let mut h = Harness::new([ok(vec![
        StreamEvent::Usage(usage(5, 1, 0, 0)),
        StreamEvent::Done(StopReason::EndTurn),
    ])]);
    h.run("hi").await.0.unwrap();
    assert_eq!(h.session.last_turn.usage.input_tokens, 5);
    assert_eq!(h.session.last_turn.cost_usd, None);
    assert_eq!(h.session.cost_usd, 0.0);
}

#[tokio::test]
async fn tool_call_round_trip() {
    let args = r#"{"path":"README.md"}"#;
    let mut h = Harness::new([tool_step(&[("call_1", "echo", args)]), reply("done")]);

    let (result, events) = h.run("read it").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    assert!(events.contains(&AgentEvent::ToolCallStarted {
        id: "call_1".into(),
        name: "echo".into(),
        args_json: args.into(),
    }));
    assert!(events.contains(&AgentEvent::ToolCallFinished {
        id: "call_1".into(),
        name: "echo".into(),
        output: args.into(),
        is_error: false,
    }));

    // user, assistant tool call, tool result, assistant answer.
    let msgs = &h.session.messages[1..];
    assert_eq!(msgs.len(), 4);
    assert_eq!(
        msgs[1].tool_calls().collect::<Vec<_>>(),
        [("call_1", "echo", args)]
    );
    assert_eq!(msgs[2], Message::tool_result("call_1", args, false));
    assert_eq!(msgs[3].text_content(), "done");

    let requests = h.provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[1].messages.last(), Some(&msgs[2]));
}

#[tokio::test]
async fn bad_tool_calls_become_error_results() {
    let mut h = Harness::new([
        tool_step(&[
            ("c1", "nope", "{}"),
            ("c2", "echo", "{not json"),
            ("c3", "echo", ""),
        ]),
        reply("ok"),
    ]);

    let (result, _) = h.run("go").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    let results = h.tool_results();
    assert_eq!(results.len(), 3);
    assert!(results[0].2 && results[0].1.contains("unknown tool: nope"));
    assert!(results[1].2 && results[1].1.contains("invalid JSON"));
    assert_eq!(results[2], ("c3".into(), "{}".into(), false));
}

#[tokio::test]
async fn stops_at_max_steps() {
    let scripts = (0..3).map(|i| tool_step(&[(&*format!("c{i}"), "echo", "{}")]));
    let mut h = Harness::with_config(scripts, |c| c.max_steps = 3, |_| {});

    let (result, events) = h.run("loop forever").await;

    assert_eq!(result.unwrap(), TurnOutcome::MaxSteps);
    assert_eq!(h.provider.requests().len(), 3);
    assert_eq!(h.tool_results().len(), 3);
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnFinished(TurnOutcome::MaxSteps))
    );
}

#[tokio::test]
async fn cancel_mid_stream_keeps_partial_text() {
    let mut h = Harness::new([Script::Hang(vec![text("partial ans")])]);

    let (result, events) = h
        .run_with("hi", |event, cancel| {
            if matches!(event, AgentEvent::TextDelta(_)) {
                cancel.cancel();
            }
        })
        .await;

    assert_eq!(result.unwrap(), TurnOutcome::Cancelled);
    assert_eq!(h.last(), &Message::assistant("partial ans"));
    assert_eq!(
        events.last(),
        Some(&AgentEvent::TurnFinished(TurnOutcome::Cancelled))
    );
}

#[tokio::test]
async fn cancel_during_tool_still_answers_every_call() {
    let mut h = Harness::new([tool_step(&[("c1", "slow", "{}"), ("c2", "echo", "{}")])]);

    let (result, _) = h
        .run_with("go", |event, cancel| {
            if matches!(event, AgentEvent::ToolCallStarted { name, .. } if name == "slow") {
                cancel.cancel();
            }
        })
        .await;

    assert_eq!(result.unwrap(), TurnOutcome::Cancelled);
    let results = h.tool_results();
    assert_eq!(results.len(), 2);
    for (_, output, is_error) in results {
        assert!(is_error);
        assert!(output.contains("cancelled"), "{output}");
    }
    assert_eq!(h.provider.requests().len(), 1);
}

#[tokio::test]
async fn tool_timeout_is_reported() {
    let mut h = Harness::with_config(
        [tool_step(&[("c1", "slow", "{}")]), done()],
        |c| c.tool_timeout = Duration::from_millis(20),
        |_| {},
    );

    let (result, _) = h.run("go").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    let (_, output, is_error) = &h.tool_results()[0];
    assert!(is_error);
    assert!(output.contains("timed out"), "{output}");
}

#[tokio::test]
async fn large_tool_output_is_truncated() {
    let mut h = Harness::new([tool_step(&[("c1", "big", "{}")]), done()]);

    h.run("go").await.0.unwrap();

    let (_, output, is_error) = &h.tool_results()[0];
    assert!(!is_error);
    assert!(output.len() < 31 * 1024, "{}", output.len());
    assert!(output.starts_with("HEAD") && output.ends_with("TAIL"));
    assert!(output.contains("bytes truncated"));
}

#[tokio::test]
async fn write_tools_are_denied_without_approval() {
    let ran = Arc::new(AtomicBool::new(false));
    let tool = Fake::Write(ran.clone());
    let mut h = Harness::with_config(
        [tool_step(&[("c1", "write", "{}")]), done()],
        |_| {},
        move |tools| tools.register(tool),
    );

    let events = h.run("write").await.1;

    assert!(!ran.load(Ordering::SeqCst));
    let (_, output, is_error) = &h.tool_results()[0];
    assert!(is_error);
    assert!(output.contains("denied"), "{output}");
    // The default gate answers by itself: no prompt was ever open.
    assert!(prompts(&events).is_empty(), "{events:?}");
}

#[tokio::test]
async fn approval_prompts_are_reported_while_open() {
    for (decision, resolution) in [
        (Decision::Approve, ApprovalResolution::Approved),
        (deny("no"), ApprovalResolution::Denied),
    ] {
        let gate = ScriptedGate::always(decision);
        let mut h = shell_harness(
            vec![
                tool_step(&[("c1", "shell", r#"{"command":"touch made"}"#)]),
                done(),
            ],
            ApprovalMode::ReadOnly,
            gate.clone(),
        );

        let (_, events) = h.run("make it").await;

        assert_eq!(gate.asked().len(), 1);
        assert_eq!(
            prompts(&events),
            [
                AgentEvent::ApprovalPending(PendingApproval {
                    request_id: "c1".into(),
                    tool: "shell".into(),
                    summary: "$ touch made".into(),
                    provider: "scripted".into(),
                }),
                AgentEvent::ApprovalResolved {
                    request_id: "c1".into(),
                    resolution,
                },
            ]
        );
        let position = |pred: &dyn Fn(&AgentEvent) -> bool| events.iter().position(pred).unwrap();
        let order = [
            position(&|e| matches!(e, AgentEvent::ToolCallStarted { .. })),
            position(&|e| matches!(e, AgentEvent::ApprovalPending(_))),
            position(&|e| matches!(e, AgentEvent::ApprovalResolved { .. })),
            position(&|e| matches!(e, AgentEvent::Approval(_))),
            position(&|e| matches!(e, AgentEvent::ToolCallFinished { .. })),
        ];
        assert!(order.is_sorted(), "{events:?}");
    }
}

#[tokio::test]
async fn auto_decisions_open_no_prompt() {
    let gate = ScriptedGate::always(deny("should not be asked"));
    let mut h = shell_harness(
        vec![
            tool_step(&[
                ("c1", "echo", "{}"),
                ("c2", "shell", r#"{"command":"echo hi"}"#),
            ]),
            done(),
        ],
        ApprovalMode::FullAccess,
        gate.clone(),
    );

    let (_, events) = h.run("go").await;

    assert_eq!(approvals(&events).len(), 2);
    assert!(gate.asked().is_empty());
    assert!(prompts(&events).is_empty(), "{events:?}");
}

/// Never answers, like a user who walked away.
struct AbsentUser;

#[async_trait]
impl ApprovalGate for AbsentUser {
    async fn decide(&self, _req: ApprovalRequest) -> Decision {
        std::future::pending().await
    }
}

#[tokio::test]
async fn a_prompt_open_at_cancel_expires() {
    let mut h = Harness::build(
        vec![
            tool_step(&[("c1", "shell", r#"{"command":"touch x"}"#)]),
            done(),
        ],
        |_| {},
        |tools| tools.register(Shell),
        |agent| {
            agent
                .with_approval_gate(Arc::new(AbsentUser))
                .with_sandbox(Arc::new(NoSandbox))
        },
    );

    let (result, events) = h
        .run_with("go", |event, cancel| {
            if matches!(event, AgentEvent::ApprovalPending(_)) {
                cancel.cancel();
            }
        })
        .await;

    assert_eq!(result.unwrap(), TurnOutcome::Cancelled);
    assert_eq!(
        prompts(&events)[1..],
        [AgentEvent::ApprovalResolved {
            request_id: "c1".into(),
            resolution: ApprovalResolution::Expired,
        }]
    );
    assert_eq!(approvals(&events)[0].source, DecisionSource::Cancelled);
}

#[tokio::test]
async fn agent_backend_prompts_are_forwarded() {
    let pending = PendingApproval {
        request_id: "mcp-1".into(),
        tool: "shell".into(),
        summary: "$ cargo test".into(),
        provider: "claude-cli".into(),
    };
    let mut h = Harness::new([ok(vec![
        StreamEvent::ApprovalPending(pending.clone()),
        StreamEvent::ApprovalResolved {
            request_id: "mcp-1".into(),
            resolution: ApprovalResolution::Approved,
        },
        StreamEvent::Done(StopReason::EndTurn),
    ])]);
    h.provider.backend.store(true, Ordering::SeqCst);

    let (_, events) = h.run("test").await;

    assert_eq!(
        prompts(&events),
        [
            AgentEvent::ApprovalPending(pending),
            AgentEvent::ApprovalResolved {
                request_id: "mcp-1".into(),
                resolution: ApprovalResolution::Approved,
            },
        ]
    );
}

#[tokio::test]
async fn every_decision_is_logged_as_an_event() {
    let gate = ScriptedGate::always(Decision::Approve);
    let mut h = shell_harness(
        vec![
            tool_step(&[
                ("c1", "echo", "{}"),
                ("c2", "shell", r#"{"command":"echo hi"}"#),
            ]),
            done(),
        ],
        ApprovalMode::FullAccess,
        gate.clone(),
    );

    let (_, events) = h.run("go").await;

    let records = approvals(&events);
    assert_eq!(records.len(), 2);
    assert_eq!(records[0].tool, "echo");
    assert_eq!(
        records[0].source,
        DecisionSource::Mode(ApprovalMode::FullAccess)
    );
    assert_eq!(records[1].tool, "shell");
    assert_eq!(records[1].preview, "$ echo hi");
    assert_eq!(records[1].decision, Decision::Approve);
    assert_eq!(records[1].sandbox, SandboxMode::Off);
    assert!(gate.asked().is_empty());
}

#[tokio::test]
async fn destructive_commands_prompt_even_in_full_access() {
    let gate = ScriptedGate::always(deny("not on my machine"));
    let mut h = shell_harness(
        vec![
            tool_step(&[("c1", "shell", r#"{"command":"touch keep && rm -rf keep"}"#)]),
            done(),
        ],
        ApprovalMode::FullAccess,
        gate.clone(),
    );

    let (_, events) = h.run("clean up").await;

    let asked = gate.asked();
    assert_eq!(asked.len(), 1);
    assert_eq!(asked[0].risk, Risk::Destructive);
    assert_eq!(asked[0].preview, "$ touch keep && rm -rf keep");
    assert_eq!(asked[0].allow_rule, None);
    let (_, output, is_error) = &h.tool_results()[0];
    assert!(is_error);
    assert_eq!(output, "denied: not on my machine");
    assert!(!h.dir.path().join("keep").exists());
    assert_eq!(approvals(&events)[0].source, DecisionSource::Gate);
}

#[tokio::test]
async fn network_commands_prompt_in_auto_mode() {
    let gate = ScriptedGate::always(deny("no"));
    let mut h = shell_harness(
        vec![
            tool_step(&[
                ("c1", "shell", r#"{"command":"touch out.txt"}"#),
                ("c2", "shell", r#"{"command":"curl https://example.com"}"#),
            ]),
            done(),
        ],
        ApprovalMode::AutoWorkspace,
        gate.clone(),
    );

    h.run("go").await.0.unwrap();

    // Without an OS sandbox even the workspace write asks (with one it would run on its own).
    let asked = gate.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(
        (asked[0].risk, asked[0].sandbox),
        (Risk::WriteWorkspace, SandboxMode::Off)
    );
    assert_eq!(asked[1].risk, Risk::Network);
    assert!(asked[1].network, "the prompt says the network would be on");
    assert_eq!(
        asked[1].allow_rule,
        Some(AllowRule::with_host("curl", "example.com"))
    );
}

#[tokio::test]
async fn always_allow_is_persisted_and_reused() {
    let gate = ScriptedGate::always(Decision::AlwaysAllowPrefix);
    let mut h = shell_harness(
        vec![
            tool_step(&[("c1", "shell", r#"{"command":"touch one.txt"}"#)]),
            tool_step(&[("c2", "shell", r#"{"command":"touch two.txt"}"#)]),
            done(),
        ],
        ApprovalMode::AutoWorkspace,
        gate.clone(),
    );

    let (_, events) = h.run("go").await;

    assert_eq!(gate.asked().len(), 1);
    let records = approvals(&events);
    assert_eq!(records[0].decision, Decision::AlwaysAllowPrefix);
    assert_eq!(
        records[1].source,
        DecisionSource::Rule {
            prefix: "touch".into()
        }
    );
    let root = h.agent.workspace().root();
    let saved = ApprovalRules::load(&approvals_path(root)).unwrap();
    assert_eq!(
        saved.rules,
        [PrefixRule {
            tool: "shell".into(),
            prefix: "touch".into(),
            host: None,
        }]
    );
    assert!(root.join("one.txt").exists() && root.join("two.txt").exists());
}

#[tokio::test]
async fn invalid_calls_are_rejected_before_approval() {
    let gate = ScriptedGate::always(Decision::Approve);
    let mut h = shell_harness(
        vec![
            tool_step(&[("c1", "shell", r#"{"command":"ls","cwd":"../.."}"#)]),
            done(),
        ],
        ApprovalMode::ReadOnly,
        gate.clone(),
    );

    h.run("go").await.0.unwrap();

    assert!(gate.asked().is_empty());
    let (_, output, is_error) = &h.tool_results()[0];
    assert!(
        *is_error && output.contains("outside the workspace"),
        "{output}"
    );
}

#[tokio::test]
async fn provider_error_keeps_partial_text() {
    let mut h = Harness::new([Script::Events(vec![
        Ok(text("half an ")),
        Err(ProviderError::Network("connection reset".into())),
    ])]);

    let (result, _) = h.run("hi").await;

    assert!(matches!(
        result,
        Err(AgentError::Provider(ProviderError::Network(_)))
    ));
    assert_eq!(h.last(), &Message::assistant("half an "));
}

#[tokio::test]
async fn stream_without_done_is_an_error() {
    let mut h = Harness::new([ok(vec![text("abc")])]);

    let (result, _) = h.run("hi").await;

    assert!(matches!(
        result,
        Err(AgentError::Provider(ProviderError::UnexpectedEof))
    ));
    assert_eq!(h.last(), &Message::assistant("abc"));
}

#[tokio::test]
async fn already_cancelled_turn_does_not_call_provider() {
    let provider = ScriptedProvider::new([]);
    let dir = tempfile::tempdir().unwrap();
    let agent = Agent::new(
        provider.clone(),
        Arc::new(ToolRegistry::new()),
        Workspace::new(dir.path()).unwrap(),
        AgentConfig::new("m"),
    );
    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = agent.run_turn("hi", &mut Session::new(), &tx, cancel).await;

    assert_eq!(outcome.unwrap(), TurnOutcome::Cancelled);
    assert!(provider.requests().is_empty());
}

#[tokio::test]
async fn agent_backend_turn_forwards_tool_events_without_executing() {
    let usage = usage(1_000, 20, 600, 300);
    let record = ApprovalRecord {
        call_id: "mcp-1".into(),
        tool: "write".into(),
        risk: Risk::WriteWorkspace,
        preview: "x".into(),
        sandbox: SandboxMode::InProcess,
        network: false,
        extra_writes: Vec::new(),
        blocked_retry: false,
        decision: Decision::Approve,
        source: DecisionSource::Mode(ApprovalMode::AutoWorkspace),
    };
    let mut events = vec![text("Looking. ")];
    events.extend(tool_call("tu_1", "write", r#"{"a":1}"#));
    events.extend([
        StreamEvent::Approval(record.clone()),
        StreamEvent::ToolResult {
            id: "tu_1".into(),
            output: "wrote".into(),
            is_error: false,
        },
        text("Done."),
        StreamEvent::Usage(usage),
        StreamEvent::Done(StopReason::EndTurn),
    ]);
    let wrote = Arc::new(AtomicBool::new(false));
    let tool = Fake::Write(wrote.clone());
    let mut h = Harness::with_config(
        [ok(events)],
        |config| {
            config.model_info = Some(ModelInfo::subscription("cli", "test-model", 200_000));
        },
        |tools| tools.register(tool),
    );
    h.provider.backend.store(true, Ordering::SeqCst);

    let (result, seen) = h.run("edit it").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    assert!(
        !wrote.load(Ordering::SeqCst),
        "the backend runs tools, not rozu"
    );
    assert_eq!(
        seen,
        [
            AgentEvent::TextDelta("Looking. ".into()),
            AgentEvent::ToolCallStarted {
                id: "tu_1".into(),
                name: "write".into(),
                args_json: r#"{"a":1}"#.into(),
            },
            AgentEvent::Approval(record),
            AgentEvent::ToolCallFinished {
                id: "tu_1".into(),
                name: "write".into(),
                output: "wrote".into(),
                is_error: false,
            },
            AgentEvent::TextDelta("Done.".into()),
            AgentEvent::Usage(usage),
            AgentEvent::TurnFinished(TurnOutcome::Completed),
        ]
    );
    let requests = h.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.is_empty());
    assert_eq!(h.last(), &Message::assistant("Looking. Done."));
    // Subscription: tokens are counted, cost is not.
    assert_eq!(h.session.usage, usage);
    assert_eq!(h.session.last_turn.cost_usd, None);
    assert_eq!(h.session.cost_usd, 0.0);
}
