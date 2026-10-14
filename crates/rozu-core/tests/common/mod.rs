#![allow(dead_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::stream::{self, StreamExt};
use rozu_core::{
    Agent, AgentError, AgentEvent, ApprovalGate, ApprovalRecord, ApprovalRequest, ChatRequest,
    ContentPart, Decision, EventStream, Provider, ProviderKind, ProviderResult, Session,
    StopReason, StreamEvent, TurnOutcome,
};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub enum Script {
    Events(Vec<ProviderResult<StreamEvent>>),
    /// Yields these events, then stalls forever.
    Hang(Vec<StreamEvent>),
}

#[derive(Default)]
pub struct ScriptedProvider {
    scripts: Mutex<VecDeque<Script>>,
    requests: Mutex<Vec<ChatRequest>>,
    pub backend: AtomicBool,
}

impl ScriptedProvider {
    pub fn new(scripts: impl IntoIterator<Item = Script>) -> Arc<Self> {
        Arc::new(Self {
            scripts: Mutex::new(scripts.into_iter().collect()),
            ..Self::default()
        })
    }

    pub fn requests(&self) -> Vec<ChatRequest> {
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

    async fn stream(&self, req: ChatRequest, _: CancellationToken) -> ProviderResult<EventStream> {
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

pub fn ok(events: Vec<StreamEvent>) -> Script {
    Script::Events(events.into_iter().map(Ok).collect())
}

pub fn text(s: &str) -> StreamEvent {
    StreamEvent::TextDelta(s.into())
}

pub fn done() -> Script {
    ok(vec![StreamEvent::Done(StopReason::EndTurn)])
}

pub fn reply(s: &str) -> Script {
    ok(vec![text(s), StreamEvent::Done(StopReason::EndTurn)])
}

/// A complete tool call with its args split across two deltas.
pub fn tool_call(id: &str, name: &str, args: &str) -> Vec<StreamEvent> {
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

pub fn tool_step(calls: &[(&str, &str, &str)]) -> Script {
    let mut events: Vec<_> = calls
        .iter()
        .flat_map(|(id, name, args)| tool_call(id, name, args))
        .collect();
    events.push(StreamEvent::Done(StopReason::ToolUse));
    ok(events)
}

/// Answers from a script, then with `fallback`, recording every request.
pub struct ScriptedGate {
    answers: Mutex<VecDeque<Decision>>,
    fallback: Decision,
    asked: Mutex<Vec<ApprovalRequest>>,
}

impl ScriptedGate {
    pub fn new(answers: impl IntoIterator<Item = Decision>) -> Arc<Self> {
        Self::build(answers, deny("script ran out"))
    }

    pub fn always(decision: Decision) -> Arc<Self> {
        Self::build([], decision)
    }

    fn build(answers: impl IntoIterator<Item = Decision>, fallback: Decision) -> Arc<Self> {
        Arc::new(Self {
            answers: Mutex::new(answers.into_iter().collect()),
            fallback,
            asked: Mutex::default(),
        })
    }

    pub fn asked(&self) -> Vec<ApprovalRequest> {
        self.asked.lock().unwrap().clone()
    }
}

#[async_trait]
impl ApprovalGate for ScriptedGate {
    async fn decide(&self, req: ApprovalRequest) -> Decision {
        self.asked.lock().unwrap().push(req);
        let next = self.answers.lock().unwrap().pop_front();
        next.unwrap_or_else(|| self.fallback.clone())
    }
}

pub fn deny(reason: &str) -> Decision {
    Decision::Deny {
        reason: reason.into(),
    }
}

pub fn approvals(events: &[AgentEvent]) -> Vec<ApprovalRecord> {
    events
        .iter()
        .filter_map(|e| match e {
            AgentEvent::Approval(record) => Some(record.clone()),
            _ => None,
        })
        .collect()
}

/// `(call_id, output, is_error)` for every tool result in the session.
pub fn tool_results(session: &Session) -> Vec<(String, String, bool)> {
    session
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

/// Runs a turn, calling `on_event` for each event with the turn's cancel token.
pub async fn run_turn(
    agent: &Agent,
    session: &mut Session,
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
        let result = agent.run_turn(input, session, &tx, cancel).await;
        drop(tx);
        result
    };
    tokio::join!(turn, consumer)
}

/// Whether `sandbox-exec` works here (it can't nest inside another sandbox).
pub fn seatbelt_usable() -> bool {
    std::process::Command::new(rozu_tools::SANDBOX_EXEC)
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .status()
        .is_ok_and(|s| s.success())
}
