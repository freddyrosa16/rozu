//! Helpers shared by the provider integration tests.

// Each test binary uses a different subset.
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rozu_core::{
    Agent, AgentConfig, AgentError, AgentEvent, ChatRequest, Message, ModelInfo, Provider,
    ProviderError, Session, StreamEvent, TurnOutcome,
};
use rozu_providers::RetryPolicy;
use rozu_tools::{Risk, Tool, ToolCtx, ToolOutput, ToolRegistry, Workspace};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use wiremock::{Request, ResponseTemplate};

pub fn sse(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

pub fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(20),
    }
}

pub fn request(model: &str, text: &str) -> ChatRequest {
    ChatRequest {
        model: model.into(),
        messages: vec![Message::user(text)],
        tools: vec![],
        max_output_tokens: None,
    }
}

pub async fn collect(
    provider: &dyn Provider,
    req: ChatRequest,
) -> Vec<Result<StreamEvent, ProviderError>> {
    provider
        .stream(req, CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await
}

pub fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

/// A fake tool that looks up a fixed value.
pub struct Lookup;

#[async_trait]
impl Tool for Lookup {
    fn name(&self) -> &'static str {
        "lookup"
    }
    fn description(&self) -> &'static str {
        "Look up a value by key"
    }
    fn schema(&self) -> Value {
        json!({"type": "object", "properties": {"key": {"type": "string"}}, "required": ["key"]})
    }
    fn risk(&self, _: &Value) -> Risk {
        Risk::Read
    }
    async fn call(&self, args: Value, _: &ToolCtx) -> ToolOutput {
        match args["key"].as_str() {
            Some("answer") => ToolOutput::ok("42"),
            other => ToolOutput::error(format!("no value for {other:?}")),
        }
    }
}

/// An agent with only the [`Lookup`] tool, in a throwaway workspace.
pub struct AgentHarness {
    _dir: tempfile::TempDir,
    pub agent: Agent,
    pub session: Session,
}

impl AgentHarness {
    pub fn new(provider: Arc<dyn Provider>, model: &str, info: Option<ModelInfo>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Lookup);
        let agent = Agent::new(
            provider,
            Arc::new(tools),
            Workspace::new(dir.path()).unwrap(),
            AgentConfig::new(model).with_model_info(info),
        );
        Self {
            _dir: dir,
            agent,
            session: Session::with_system_prompt("You are a test."),
        }
    }

    pub async fn run(&mut self, input: &str) -> (Result<TurnOutcome, AgentError>, Vec<AgentEvent>) {
        let (tx, mut rx) = mpsc::channel(64);
        let consumer = async move {
            let mut seen = Vec::new();
            while let Some(event) = rx.recv().await {
                seen.push(event);
            }
            seen
        };
        let turn = async {
            let result = self
                .agent
                .run_turn(input, &mut self.session, &tx, CancellationToken::new())
                .await;
            drop(tx);
            result
        };
        tokio::join!(turn, consumer)
    }
}
