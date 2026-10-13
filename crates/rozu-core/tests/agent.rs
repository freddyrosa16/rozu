//! Agent loop tests against a scripted mock provider and fake tools.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use rozu_core::{
    Agent, AgentConfig, AgentError, AgentEvent, ApprovalGate, ApprovalMode, ApprovalPolicy,
    ApprovalRecord, ApprovalRequest, ApprovalRules, Billing, ChatRequest, ContentPart, Decision,
    DecisionSource, EventStream, LongContextPrices, Message, ModelInfo, PrefixRule, Provider,
    ProviderError, ProviderKind, ProviderResult, Role, SandboxMode, Session, StopReason,
    StreamEvent, TurnOutcome, Usage, approvals_path,
};
use rozu_tools::{
    AllowRule, NoSandbox, Risk, Shell, Tool, ToolCtx, ToolOutput, ToolRegistry, Workspace,
};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

// ---------------------------------------------------------------------------------------------
// Scripted provider

enum Script {
    /// Yields these items, then ends.
    Events(Vec<ProviderResult<StreamEvent>>),
    /// Yields these events, then never yields again (a stalled connection).
    Hang(Vec<StreamEvent>),
}

#[derive(Default)]
struct ScriptedProvider {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<ChatRequest>>,
    /// Report [`ProviderKind::AgentBackend`] (the provider runs the tools itself).
    backend: AtomicBool,
}

impl ScriptedProvider {
    fn new(scripts: impl IntoIterator<Item = Script>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            requests: Mutex::default(),
            backend: AtomicBool::new(false),
        })
    }

    fn requests(&self) -> Vec<ChatRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl Provider for ScriptedProvider {
    fn id(&self) -> &str {
        "scripted"
    }

    fn kind(&self) -> ProviderKind {
        if self.backend.load(Ordering::SeqCst) {
            ProviderKind::AgentBackend
        } else {
            ProviderKind::Model
        }
    }

    async fn stream(
        &self,
        req: ChatRequest,
        _cancel: CancellationToken,
    ) -> ProviderResult<EventStream> {
        self.requests.lock().unwrap().push(req);
        let script = self
            .scripts
            .lock()
            .unwrap()
            .pop_front()
            .expect("provider called more often than scripted");
        Ok(match script {
            Script::Events(items) => stream::iter(items).boxed(),
            Script::Hang(events) => stream::iter(events.into_iter().map(Ok))
                .chain(stream::pending())
                .boxed(),
        })
    }
}

fn ok(events: Vec<StreamEvent>) -> Script {
    Script::Events(events.into_iter().map(Ok).collect())
}

fn text(s: &str) -> StreamEvent {
    StreamEvent::TextDelta(s.into())
}

/// A complete tool call: start, args split into two deltas, end.
fn tool_call(id: &str, name: &str, args: &str) -> Vec<StreamEvent> {
    let (a, b) = args.split_at(args.len() / 2);
    vec![
        StreamEvent::ToolCallStart {
            id: id.into(),
            name: name.into(),
        },
        StreamEvent::ToolCallArgsDelta {
            id: id.into(),
            json: a.into(),
        },
        StreamEvent::ToolCallArgsDelta {
            id: id.into(),
            json: b.into(),
        },
        StreamEvent::ToolCallEnd { id: id.into() },
    ]
}

fn tool_step(calls: &[(&str, &str, &str)]) -> Script {
    let mut events: Vec<_> = calls
        .iter()
        .flat_map(|(id, name, args)| tool_call(id, name, args))
        .collect();
    events.push(StreamEvent::Done(StopReason::ToolUse));
    ok(events)
}

// ---------------------------------------------------------------------------------------------
// Fake tools

/// Returns its arguments as JSON text.
struct Echo;

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &'static str {
        "echo"
    }
    fn description(&self) -> &'static str {
        "Echo the arguments"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn risk(&self, _: &Value, _: &ToolCtx) -> Risk {
        Risk::Read
    }
    async fn call(&self, args: Value, _: &ToolCtx) -> ToolOutput {
        ToolOutput::ok(args.to_string())
    }
}

/// Never finishes on its own.
struct Slow;

#[async_trait]
impl Tool for Slow {
    fn name(&self) -> &'static str {
        "slow"
    }
    fn description(&self) -> &'static str {
        "Sleep for an hour"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn risk(&self, _: &Value, _: &ToolCtx) -> Risk {
        Risk::Read
    }
    async fn call(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        ToolOutput::ok("woke up")
    }
}

/// Returns 100 KB of output.
struct Big;

#[async_trait]
impl Tool for Big {
    fn name(&self) -> &'static str {
        "big"
    }
    fn description(&self) -> &'static str {
        "Produce a lot of output"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn risk(&self, _: &Value, _: &ToolCtx) -> Risk {
        Risk::Read
    }
    async fn call(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        ToolOutput::ok(format!("HEAD{}TAIL", "x".repeat(100_000)))
    }
}

/// A write tool that records whether it ran.
#[derive(Default)]
struct Write(Arc<AtomicBool>);

#[async_trait]
impl Tool for Write {
    fn name(&self) -> &'static str {
        "write"
    }
    fn description(&self) -> &'static str {
        "Write something"
    }
    fn schema(&self) -> Value {
        json!({"type": "object"})
    }
    fn risk(&self, _: &Value, _: &ToolCtx) -> Risk {
        Risk::WriteWorkspace
    }
    async fn call(&self, _: Value, _: &ToolCtx) -> ToolOutput {
        self.0.store(true, Ordering::SeqCst);
        ToolOutput::ok("wrote")
    }
}

// ---------------------------------------------------------------------------------------------
// Harness

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
        tools.register(Echo);
        tools.register(Slow);
        tools.register(Big);
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

    /// Runs a turn, calling `on_event` for each event (with the turn's cancel token).
    async fn run_with(
        &mut self,
        input: &str,
        mut on_event: impl FnMut(&AgentEvent, &CancellationToken),
    ) -> (Result<TurnOutcome, AgentError>, Vec<AgentEvent>) {
        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let consumer_cancel = cancel.clone();
        let consumer = async move {
            let mut seen = Vec::new();
            while let Some(event) = rx.recv().await {
                on_event(&event, &consumer_cancel);
                seen.push(event);
            }
            seen
        };
        let turn = async {
            let result = self
                .agent
                .run_turn(input, &mut self.session, &tx, cancel)
                .await;
            drop(tx);
            result
        };
        tokio::join!(turn, consumer)
    }

    async fn run(&mut self, input: &str) -> (Result<TurnOutcome, AgentError>, Vec<AgentEvent>) {
        self.run_with(input, |_, _| {}).await
    }

    fn last(&self) -> &Message {
        self.session.messages.last().unwrap()
    }

    fn tool_results(&self) -> Vec<(String, String, bool)> {
        self.session
            .messages
            .iter()
            .flat_map(|m| &m.content)
            .filter_map(|part| match part {
                ContentPart::ToolResult {
                    call_id,
                    output,
                    is_error,
                } => Some((call_id.clone(), output.clone(), *is_error)),
                _ => None,
            })
            .collect()
    }
}

// ---------------------------------------------------------------------------------------------
// Tests

#[tokio::test]
async fn plain_text_turn_completes() {
    let usage = Usage {
        input_tokens: 10,
        output_tokens: 2,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
    };
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
    let usage = |input, output, read, write| {
        StreamEvent::Usage(Usage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: read,
            cache_write_tokens: write,
        })
    };
    let done = StreamEvent::Done(StopReason::EndTurn);
    let mut h = Harness::with_config(
        [
            tool_step(&[("call_1", "echo", r#"{"path":"a"}"#)]),
            ok(vec![usage(1_000_000, 0, 0, 0), done.clone()]),
            ok(vec![usage(1_000_000, 100_000, 1_000_000, 0), done.clone()]),
            ok(vec![usage(1_000_000, 0, 0, 1_000_000), done.clone()]),
        ],
        |config| {
            config.model_info = Some(ModelInfo {
                id: "test-model".into(),
                provider: "test".into(),
                context_window: 1_000_000,
                supports_tools: true,
                price_in_per_mtok: 2.0,
                price_out_per_mtok: 10.0,
                price_cached_in_per_mtok: 0.5,
                price_cache_write_per_mtok: 2.5,
                long_context: None,
                billing: Billing::Metered,
            });
        },
        |_| {},
    );

    // Turn 1: a tool step (no usage reported) then 1M uncached input tokens.
    h.run("first").await.0.unwrap();
    assert_eq!(h.session.last_turn.usage.input_tokens, 1_000_000);
    assert!((h.session.last_turn.cost_usd.unwrap() - 2.0).abs() < 1e-9);

    // Turn 2: 1M cached input + 100k output = 0.5 + 1.0.
    h.run("second").await.0.unwrap();
    let last = h.session.last_turn;
    assert_eq!(
        last.usage,
        Usage {
            input_tokens: 1_000_000,
            output_tokens: 100_000,
            cache_read_tokens: 1_000_000,
            cache_write_tokens: 0
        }
    );
    assert!((last.cost_usd.unwrap() - 1.5).abs() < 1e-9);
    assert_eq!(h.session.usage.input_tokens, 2_000_000);
    assert!((h.session.cost_usd - 3.5).abs() < 1e-9);

    // Turn 3: 1M input written to the cache, at the cache-write price.
    h.run("third").await.0.unwrap();
    assert_eq!(h.session.last_turn.usage.cache_write_tokens, 1_000_000);
    assert!((h.session.last_turn.cost_usd.unwrap() - 2.5).abs() < 1e-9);
    assert_eq!(h.session.usage.cache_read_tokens, 1_000_000);
    assert!((h.session.cost_usd - 6.0).abs() < 1e-9);
}

#[tokio::test]
async fn long_context_tier_applies_per_response_not_per_turn() {
    let usage = |input| {
        StreamEvent::Usage(Usage {
            input_tokens: input,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        })
    };
    let mut tool = vec![usage(150_000)];
    tool.extend(tool_call("call_1", "echo", r#"{"path":"a"}"#));
    tool.push(StreamEvent::Done(StopReason::ToolUse));
    let mut h = Harness::with_config(
        [
            ok(tool),
            ok(vec![usage(150_000), StreamEvent::Done(StopReason::EndTurn)]),
            ok(vec![usage(250_000), StreamEvent::Done(StopReason::EndTurn)]),
        ],
        |config| {
            config.model_info = Some(
                ModelInfo {
                    id: "test-model".into(),
                    provider: "test".into(),
                    context_window: 1_000_000,
                    supports_tools: true,
                    price_in_per_mtok: 1.0,
                    price_out_per_mtok: 1.0,
                    price_cached_in_per_mtok: 1.0,
                    price_cache_write_per_mtok: 1.0,
                    long_context: None,
                    billing: Billing::Metered,
                }
                .with_long_context(LongContextPrices {
                    threshold_tokens: 200_000,
                    price_in_per_mtok: 2.0,
                    price_out_per_mtok: 2.0,
                    price_cached_in_per_mtok: 2.0,
                    price_cache_write_per_mtok: 2.0,
                }),
            );
        },
        |_| {},
    );

    // Two 150k-token requests: 300k tokens in the turn, but neither request is long.
    h.run("first").await.0.unwrap();
    assert_eq!(h.session.last_turn.usage.input_tokens, 300_000);
    assert!((h.session.last_turn.cost_usd.unwrap() - 0.3).abs() < 1e-9);

    // One 250k-token request: every token at the long-context price.
    h.run("second").await.0.unwrap();
    assert!((h.session.last_turn.cost_usd.unwrap() - 0.5).abs() < 1e-9);
}

#[tokio::test]
async fn unknown_prices_leave_cost_empty() {
    let mut h = Harness::new([ok(vec![
        StreamEvent::Usage(Usage {
            input_tokens: 5,
            output_tokens: 1,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        }),
        StreamEvent::Done(StopReason::EndTurn),
    ])]);
    h.run("hi").await.0.unwrap();
    assert_eq!(h.session.last_turn.usage.input_tokens, 5);
    assert_eq!(h.session.last_turn.cost_usd, None);
    assert_eq!(h.session.cost_usd, 0.0);
}

#[tokio::test]
async fn tool_call_round_trip() {
    let mut h = Harness::new([
        tool_step(&[("call_1", "echo", r#"{"path":"README.md"}"#)]),
        ok(vec![text("done"), StreamEvent::Done(StopReason::EndTurn)]),
    ]);

    let (result, events) = h.run("read it").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    assert!(events.contains(&AgentEvent::ToolCallStarted {
        id: "call_1".into(),
        name: "echo".into(),
        args_json: r#"{"path":"README.md"}"#.into(),
    }));
    assert!(events.contains(&AgentEvent::ToolCallFinished {
        id: "call_1".into(),
        name: "echo".into(),
        output: r#"{"path":"README.md"}"#.into(),
        is_error: false,
    }));

    // user, assistant tool call, tool result, assistant answer (after the system prompt).
    let msgs = &h.session.messages[1..];
    assert_eq!(msgs.len(), 4);
    assert_eq!(
        msgs[1].tool_calls().collect::<Vec<_>>(),
        [("call_1", "echo", r#"{"path":"README.md"}"#)]
    );
    assert_eq!(
        msgs[2],
        Message::tool_result("call_1", r#"{"path":"README.md"}"#, false)
    );
    assert_eq!(msgs[3].text_content(), "done");

    // The second request carries the tool result back to the model.
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
        ok(vec![text("ok"), StreamEvent::Done(StopReason::EndTurn)]),
    ]);

    let (result, _) = h.run("go").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    let results = h.tool_results();
    assert_eq!(results.len(), 3);
    assert!(results[0].2 && results[0].1.contains("unknown tool: nope"));
    assert!(results[1].2 && results[1].1.contains("invalid JSON"));
    // Empty arguments are treated as `{}`.
    assert_eq!(results[2], ("c3".into(), "{}".into(), false));
}

#[tokio::test]
async fn stops_at_max_steps() {
    let scripts = (0..3).map(|i| tool_step(&[(&*format!("c{i}"), "echo", "{}")]));
    let mut h = Harness::with_config(scripts, |c| c.max_steps = 3, |_| {});

    let (result, events) = h.run("loop forever").await;

    assert_eq!(result.unwrap(), TurnOutcome::MaxSteps);
    assert_eq!(h.provider.requests().len(), 3);
    // Every call still got its result.
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
        [
            tool_step(&[("c1", "slow", "{}")]),
            ok(vec![StreamEvent::Done(StopReason::EndTurn)]),
        ],
        |c| c.tool_timeout = Duration::from_millis(20),
        |_| {},
    );

    let (result, _) = h.run("go").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    let results = h.tool_results();
    assert!(results[0].2);
    assert!(results[0].1.contains("timed out"), "{}", results[0].1);
}

#[tokio::test]
async fn large_tool_output_is_truncated() {
    let mut h = Harness::new([
        tool_step(&[("c1", "big", "{}")]),
        ok(vec![StreamEvent::Done(StopReason::EndTurn)]),
    ]);

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
    let tool = Write(ran.clone());
    let mut h = Harness::with_config(
        [
            tool_step(&[("c1", "write", "{}")]),
            ok(vec![StreamEvent::Done(StopReason::EndTurn)]),
        ],
        |_| {},
        move |tools| tools.register(tool),
    );

    h.run("write").await.0.unwrap();

    assert!(!ran.load(Ordering::SeqCst));
    let (_, output, is_error) = &h.tool_results()[0];
    assert!(is_error);
    assert!(output.contains("denied"), "{output}");
}

/// Answers every prompt with a fixed decision and records what it was asked.
struct ScriptedGate {
    decision: Decision,
    asked: Mutex<Vec<ApprovalRequest>>,
}

impl ScriptedGate {
    fn new(decision: Decision) -> Arc<Self> {
        Arc::new(Self {
            decision,
            asked: Mutex::default(),
        })
    }

    fn asked(&self) -> Vec<ApprovalRequest> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl ApprovalGate for ScriptedGate {
    async fn decide(&self, req: ApprovalRequest) -> Decision {
        self.asked.lock().unwrap().push(req);
        self.decision.clone()
    }
}

fn approvals(events: &[AgentEvent]) -> Vec<ApprovalRecord> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Approval(record) => Some(record.clone()),
            _ => None,
        })
        .collect()
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

fn done() -> Script {
    ok(vec![StreamEvent::Done(StopReason::EndTurn)])
}

#[tokio::test]
async fn every_decision_is_logged_as_an_event() {
    let gate = ScriptedGate::new(Decision::Approve);
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
    let gate = ScriptedGate::new(Decision::Deny {
        reason: "not on my machine".into(),
    });
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
    // The reason goes back to the model, and the command never ran.
    let (_, output, is_error) = &h.tool_results()[0];
    assert!(is_error);
    assert_eq!(output, "denied: not on my machine");
    assert!(!h.dir.path().join("keep").exists());
    let records = approvals(&events);
    assert_eq!(records[0].source, DecisionSource::Gate);
}

#[tokio::test]
async fn network_commands_prompt_in_auto_mode() {
    let gate = ScriptedGate::new(Decision::Deny {
        reason: "no".into(),
    });
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

    // Without an OS sandbox even the workspace write asks (with one it would run on its own;
    // see the policy's unit tests); the network call asks either way.
    let asked = gate.asked();
    assert_eq!(asked.len(), 2);
    assert_eq!(
        (asked[0].risk, asked[0].sandbox),
        (Risk::WriteWorkspace, SandboxMode::Off)
    );
    assert_eq!(asked[1].risk, Risk::Network);
    assert!(asked[1].network, "the prompt says the network would be on");
    // "Always allow" for a network client names its host.
    assert_eq!(
        asked[1].allow_rule,
        Some(AllowRule::with_host("curl", "example.com"))
    );
}

#[tokio::test]
async fn always_allow_is_persisted_and_reused() {
    let gate = ScriptedGate::new(Decision::AlwaysAllowPrefix);
    let mut h = shell_harness(
        vec![
            // Unsandboxed (NoSandbox), so these workspace writes ask in auto mode.
            tool_step(&[("c1", "shell", r#"{"command":"touch one.txt"}"#)]),
            tool_step(&[("c2", "shell", r#"{"command":"touch two.txt"}"#)]),
            done(),
        ],
        ApprovalMode::AutoWorkspace,
        gate.clone(),
    );

    let (_, events) = h.run("go").await;

    // Only the first call prompted; the rule covered the second.
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
    let gate = ScriptedGate::new(Decision::Approve);
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
    let mut session = Session::new();
    let (tx, _rx) = mpsc::channel(4);
    let cancel = CancellationToken::new();
    cancel.cancel();

    let outcome = agent.run_turn("hi", &mut session, &tx, cancel).await;

    assert_eq!(outcome.unwrap(), TurnOutcome::Cancelled);
    assert!(provider.requests().is_empty());
}

#[tokio::test]
async fn agent_backend_turn_forwards_tool_events_without_executing() {
    let usage = Usage {
        input_tokens: 1_000,
        output_tokens: 20,
        cache_read_tokens: 600,
        cache_write_tokens: 300,
    };
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
    let tool = Write(wrote.clone());
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
    // One request for the whole turn, no tools offered, and only the text kept in the session.
    let requests = h.provider.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].tools.is_empty());
    assert_eq!(h.last(), &Message::assistant("Looking. Done."));
    // Subscription: tokens are counted, cost is not.
    assert_eq!(h.session.usage, usage);
    assert_eq!(h.session.last_turn.cost_usd, None);
    assert_eq!(h.session.cost_usd, 0.0);
}
