//! OpenAI adapter tests against recorded Responses API SSE fixtures served by wiremock.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use rozu_core::{
    Agent, AgentConfig, AgentError, AgentEvent, ChatRequest, Message, Provider, ProviderError,
    Session, StopReason, StreamEvent, TurnOutcome, Usage,
};
use rozu_providers::{OpenAiConfig, OpenAiProvider, RetryPolicy};
use rozu_tools::{Risk, Tool, ToolCtx, ToolOutput, ToolRegistry, Workspace};
use secrecy::SecretString;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

const TEXT: &str = include_str!("fixtures/openai/text.sse");
const TOOL_CALL: &str = include_str!("fixtures/openai/tool_call.sse");
const AFTER_TOOL: &str = include_str!("fixtures/openai/after_tool.sse");
const TRUNCATED: &str = include_str!("fixtures/openai/truncated.sse");

const API_KEY: &str = "sk-test-0123456789abcdef";

fn sse(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        max_retries: 3,
        base_delay: Duration::from_millis(1),
        max_delay: Duration::from_millis(20),
    }
}

fn provider(base_url: &str) -> Arc<OpenAiProvider> {
    let config = OpenAiConfig::new(SecretString::from(API_KEY))
        .with_base_url(base_url)
        .with_retry(fast_retry());
    Arc::new(OpenAiProvider::new(config).unwrap())
}

fn request(text: &str) -> ChatRequest {
    ChatRequest {
        model: "gpt-5.1".into(),
        messages: vec![Message::user(text)],
        tools: vec![],
        max_output_tokens: None,
    }
}

async fn collect(
    provider: &OpenAiProvider,
    req: ChatRequest,
) -> Vec<Result<StreamEvent, ProviderError>> {
    provider
        .stream(req, CancellationToken::new())
        .await
        .unwrap()
        .collect()
        .await
}

fn body(request: &Request) -> Value {
    serde_json::from_slice(&request.body).unwrap()
}

/// A fake tool that looks up a fixed value.
struct Lookup;

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

struct AgentHarness {
    _dir: tempfile::TempDir,
    agent: Agent,
    session: Session,
}

impl AgentHarness {
    fn new(provider: Arc<OpenAiProvider>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let mut tools = ToolRegistry::new();
        tools.register(Lookup);
        let agent = Agent::new(
            provider,
            Arc::new(tools),
            Workspace::new(dir.path()).unwrap(),
            AgentConfig::new("gpt-5.1"),
        );
        Self {
            _dir: dir,
            agent,
            session: Session::with_system_prompt("You are a test."),
        }
    }

    /// Runs a turn; cancels it as soon as the first text delta arrives if `cancel_on_text`.
    async fn run(
        &mut self,
        input: &str,
        cancel_on_text: bool,
    ) -> (Result<TurnOutcome, AgentError>, Vec<AgentEvent>) {
        let (tx, mut rx) = mpsc::channel(16);
        let cancel = CancellationToken::new();
        let consumer_cancel = cancel.clone();
        let consumer = async move {
            let mut seen = Vec::new();
            while let Some(event) = rx.recv().await {
                if cancel_on_text && matches!(event, AgentEvent::TextDelta(_)) {
                    consumer_cancel.cancel();
                }
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
}

#[tokio::test]
async fn streams_plain_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header(
            "authorization",
            format!("Bearer {API_KEY}").as_str(),
        ))
        .and(header("accept", "text/event-stream"))
        .respond_with(sse(TEXT))
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider(&format!("{}/v1", server.uri()));
    let events: Vec<_> = collect(&provider, request("say hello"))
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();

    assert_eq!(
        events,
        [
            StreamEvent::TextDelta("Hello".into()),
            StreamEvent::TextDelta(", world".into()),
            StreamEvent::TextDelta("!".into()),
            StreamEvent::Usage(Usage {
                input_tokens: 12,
                output_tokens: 4,
                cached_input_tokens: 0
            }),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );
    let sent = body(&server.received_requests().await.unwrap()[0]);
    assert_eq!(sent["model"], "gpt-5.1");
    assert_eq!(sent["stream"], true);
    assert_eq!(
        sent["input"],
        json!([{"role": "user", "content": "say hello"}])
    );
}

#[tokio::test]
async fn tool_call_round_trip_through_agent() {
    let server = MockServer::start().await;
    // The first request gets a tool call; the follow-up (with the tool result) gets the answer.
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(sse(TOOL_CALL))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/responses"))
        .respond_with(sse(AFTER_TOOL))
        .expect(1)
        .mount(&server)
        .await;

    let mut h = AgentHarness::new(provider(&server.uri()));
    let (result, events) = h.run("what is the answer?", false).await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    assert!(events.contains(&AgentEvent::ToolCallFinished {
        id: "call_abc123".into(),
        name: "lookup".into(),
        output: "42".into(),
        is_error: false,
    }));
    assert_eq!(
        h.session.messages.last().unwrap().text_content(),
        "The answer is 42."
    );
    assert_eq!(h.session.usage.input_tokens, 200);
    assert_eq!(h.session.usage.cached_input_tokens, 64);

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let first = body(&requests[0]);
    assert_eq!(first["tools"][0]["name"], "lookup");
    assert_eq!(first["tools"][0]["type"], "function");

    let second = body(&requests[1]);
    let input = second["input"].as_array().unwrap();
    assert_eq!(
        input[input.len() - 2..],
        [
            json!({"type": "function_call", "call_id": "call_abc123", "name": "lookup", "arguments": "{\"key\":\"answer\"}"}),
            json!({"type": "function_call_output", "call_id": "call_abc123", "output": "42"}),
        ]
    );
}

#[tokio::test]
async fn interrupted_stream_keeps_partial_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(TRUNCATED))
        .mount(&server)
        .await;

    let mut h = AgentHarness::new(provider(&server.uri()));
    let (result, _) = h.run("list the files", false).await;

    assert!(
        matches!(
            result,
            Err(AgentError::Provider(ProviderError::UnexpectedEof))
        ),
        "{result:?}"
    );
    assert_eq!(
        h.session.messages.last(),
        Some(&Message::assistant("The files are"))
    );
}

#[tokio::test]
async fn retries_429_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "0")
                .set_body_json(
                    json!({"error": {"message": "Rate limit reached", "type": "requests"}}),
                ),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse(TEXT))
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider(&server.uri());
    let events = collect(&provider, request("hi")).await;

    assert_eq!(events.len(), 5);
    assert_eq!(
        events.last().unwrap(),
        &Ok(StreamEvent::Done(StopReason::EndTurn))
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 2);
}

#[tokio::test]
async fn retries_server_errors_with_backoff() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse(TEXT))
        .mount(&server)
        .await;

    let events = collect(&provider(&server.uri()), request("hi")).await;

    assert!(events.iter().all(Result::is_ok));
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn gives_up_after_max_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after-ms", "1")
                .set_body_json(json!({"error": {"message": "Slow down"}})),
        )
        .mount(&server)
        .await;

    let err = provider(&server.uri())
        .stream(request("hi"), CancellationToken::new())
        .await
        .err()
        .unwrap();

    assert_eq!(
        err,
        ProviderError::RateLimited {
            attempts: 4,
            message: "Slow down".into()
        }
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 4);
}

#[tokio::test]
async fn auth_errors_are_typed_and_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "Incorrect API key provided"}})),
        )
        .mount(&server)
        .await;

    let err = provider(&server.uri())
        .stream(request("hi"), CancellationToken::new())
        .await
        .err()
        .unwrap();

    assert_eq!(
        err,
        ProviderError::Auth {
            status: 401,
            message: "Incorrect API key provided".into()
        }
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 1);
}

#[tokio::test]
async fn stream_error_event_is_typed() {
    let server = MockServer::start().await;
    let body = "event: error\ndata: {\"type\":\"error\",\"code\":\"server_error\",\"message\":\"overloaded\"}\n\n";
    Mock::given(method("POST"))
        .respond_with(sse(body))
        .mount(&server)
        .await;

    let events = collect(&provider(&server.uri()), request("hi")).await;

    assert_eq!(
        events,
        [Err(ProviderError::Api {
            code: Some("server_error".into()),
            message: "overloaded".into()
        })]
    );
}

/// Serves one HTTP response that sends `chunk` and then stalls, like a slow model. Signals on
/// the returned channel once the client closes the connection.
async fn stalling_server(chunk: &'static str) -> (String, oneshot::Receiver<()>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let (closed_tx, closed_rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        // Read the request head and body.
        let mut buf = Vec::new();
        let mut tmp = [0u8; 4096];
        loop {
            let n = socket.read(&mut tmp).await.unwrap();
            buf.extend_from_slice(&tmp[..n]);
            let text = String::from_utf8_lossy(&buf);
            if let Some(head_end) = text.find("\r\n\r\n") {
                let length = text[..head_end]
                    .lines()
                    .find_map(|l| {
                        let (k, v) = l.split_once(':')?;
                        k.eq_ignore_ascii_case("content-length")
                            .then(|| v.trim().parse::<usize>().ok())?
                    })
                    .unwrap_or(0);
                if buf.len() >= head_end + 4 + length {
                    break;
                }
            }
        }
        let head = "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ntransfer-encoding: chunked\r\n\r\n";
        socket.write_all(head.as_bytes()).await.unwrap();
        socket
            .write_all(format!("{:x}\r\n{chunk}\r\n", chunk.len()).as_bytes())
            .await
            .unwrap();
        socket.flush().await.unwrap();
        // Never finish the response; wait for the client to hang up.
        while socket.read(&mut tmp).await.is_ok_and(|n| n > 0) {}
        let _ = closed_tx.send(());
    });
    (format!("http://{addr}"), closed_rx)
}

#[tokio::test]
async fn cancel_mid_stream_keeps_partial_text_and_closes_connection() {
    let chunk = "event: response.output_text.delta\ndata: {\"type\":\"response.output_text.delta\",\"item_id\":\"msg_1\",\"output_index\":0,\"content_index\":0,\"delta\":\"Partial answ\"}\n\n";
    let (base_url, closed) = stalling_server(chunk).await;

    let mut h = AgentHarness::new(provider(&base_url));
    let (result, events) = tokio::time::timeout(Duration::from_secs(10), h.run("hi", true))
        .await
        .expect("cancel stops the turn");

    assert_eq!(result.unwrap(), TurnOutcome::Cancelled);
    assert_eq!(
        events,
        [
            AgentEvent::TextDelta("Partial answ".into()),
            AgentEvent::TurnFinished(TurnOutcome::Cancelled),
        ]
    );
    assert_eq!(
        h.session.messages.last(),
        Some(&Message::assistant("Partial answ"))
    );
    tokio::time::timeout(Duration::from_secs(5), closed)
        .await
        .expect("connection is closed after cancel")
        .unwrap();
}
