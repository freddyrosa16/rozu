mod common;

use std::sync::Arc;

use common::{AgentHarness, body, collect, fast_retry, request, sse, stream_err};
use rozu_core::{
    AgentError, AgentEvent, Message, Provider, ProviderError, StopReason, StreamEvent, TurnOutcome,
    Usage,
};
use rozu_providers::{AnthropicConfig, AnthropicProvider, registry};
use secrecy::SecretString;
use serde_json::json;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEXT: &str = include_str!("fixtures/anthropic/text.sse");
const TOOL_USE: &str = include_str!("fixtures/anthropic/tool_use.sse");
const AFTER_TOOL: &str = include_str!("fixtures/anthropic/after_tool.sse");
const OVERLOADED: &str = include_str!("fixtures/anthropic/overloaded.sse");

const API_KEY: &str = "sk-ant-test-0123456789abcdef";
const MODEL: &str = "claude-sonnet-4-6";

fn provider(base_url: &str) -> Arc<AnthropicProvider> {
    let config = AnthropicConfig::new(SecretString::from(API_KEY))
        .with_base_url(base_url)
        .with_retry(fast_retry());
    Arc::new(AnthropicProvider::new(config).unwrap())
}

#[tokio::test]
async fn streams_plain_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", API_KEY))
        .and(header("anthropic-version", "2023-06-01"))
        .and(header("accept", "text/event-stream"))
        .respond_with(sse(TEXT))
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider(&server.uri());
    assert_eq!(provider.id(), "anthropic");
    let events: Vec<_> = collect(provider.as_ref(), request(MODEL, "say hello"))
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
                cache_read_tokens: 0,
                cache_write_tokens: 0
            }),
            StreamEvent::Done(StopReason::EndTurn),
        ]
    );
    let requests = server.received_requests().await.unwrap();
    // No Bearer auth: the key goes only in x-api-key.
    assert!(requests[0].headers.get("authorization").is_none());
    let sent = body(&requests[0]);
    assert_eq!(sent["model"], MODEL);
    assert_eq!(sent["stream"], true);
    assert_eq!(sent["max_tokens"], 8192);
    assert_eq!(
        sent["messages"],
        json!([{"role": "user", "content": [{"type": "text", "text": "say hello"}]}])
    );
}

#[tokio::test]
async fn tool_use_round_trip_through_agent() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse(TOOL_USE))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(sse(AFTER_TOOL))
        .expect(1)
        .mount(&server)
        .await;

    let info = registry::lookup("anthropic/claude-sonnet-4-6");
    let mut h = AgentHarness::new(provider(&server.uri()), MODEL, info.clone());
    let (result, events) = h.run("what is the answer?").await;

    assert_eq!(result.unwrap(), TurnOutcome::Completed);
    assert!(events.contains(&AgentEvent::ToolCallFinished {
        id: "toolu_01abc".into(),
        name: "lookup".into(),
        output: "42".into(),
        is_error: false,
    }));
    assert_eq!(
        h.session.messages.last().unwrap().text_content(),
        "The answer is 42."
    );
    // Input includes the first request's cache write (300) and the second's cache read (300).
    let usage = h.session.last_turn.usage;
    assert_eq!(
        usage,
        Usage {
            input_tokens: 320 + 325,
            output_tokens: 36,
            cache_read_tokens: 300,
            cache_write_tokens: 300
        }
    );
    let cost = h.session.last_turn.cost_usd.unwrap();
    assert!((cost - info.unwrap().cost_usd(&usage)).abs() < 1e-12);
    // Sonnet 4.5: 45 uncached * $3 + 300 read * $0.30 + 300 written * $3.75 + 36 out * $15, per 1M.
    let expected = (45.0 * 3.0 + 300.0 * 0.30 + 300.0 * 3.75 + 36.0 * 15.0) / 1_000_000.0;
    assert!((cost - expected).abs() < 1e-12, "{cost}");

    let requests = server.received_requests().await.unwrap();
    assert_eq!(requests.len(), 2);
    let first = body(&requests[0]);
    assert_eq!(
        first["system"],
        json!([{"type": "text", "text": "You are a test.", "cache_control": {"type": "ephemeral"}}])
    );
    assert_eq!(first["tools"][0]["name"], "lookup");
    assert_eq!(
        first["tools"][0]["input_schema"]["required"],
        json!(["key"])
    );
    assert_eq!(
        first["tools"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );

    let second = body(&requests[1]);
    let messages = second["messages"].as_array().unwrap();
    assert_eq!(
        messages[messages.len() - 2..],
        [
            json!({"role": "assistant", "content": [
                {"type": "text", "text": "Let me check. "},
                {"type": "tool_use", "id": "toolu_01abc", "name": "lookup", "input": {"key": "answer"}},
            ]}),
            json!({"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "toolu_01abc", "content": "42"},
            ]}),
        ]
    );
}

#[tokio::test]
async fn stream_error_event_is_typed_and_keeps_partial_text() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse(OVERLOADED))
        .mount(&server)
        .await;

    let events = collect(provider(&server.uri()).as_ref(), request(MODEL, "hi")).await;
    assert_eq!(
        events,
        [
            Ok(StreamEvent::TextDelta("Partial".into())),
            Err(ProviderError::Api {
                code: Some("overloaded_error".into()),
                message: "Overloaded".into()
            }),
        ]
    );

    let mut h = AgentHarness::new(provider(&server.uri()), MODEL, None);
    let (result, _) = h.run("hi").await;
    assert!(
        matches!(result, Err(AgentError::Provider(ProviderError::Api { .. }))),
        "{result:?}"
    );
    assert_eq!(
        h.session.messages.last(),
        Some(&Message::assistant("Partial"))
    );
}

fn overloaded() -> ResponseTemplate {
    ResponseTemplate::new(529).set_body_json(
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
    )
}

#[tokio::test]
async fn retries_529_overloaded_then_succeeds() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(overloaded())
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse(TEXT))
        .expect(1)
        .mount(&server)
        .await;

    let events = collect(provider(&server.uri()).as_ref(), request(MODEL, "hi")).await;

    assert!(events.iter().all(Result::is_ok), "{events:?}");
    assert_eq!(
        events.last().unwrap(),
        &Ok(StreamEvent::Done(StopReason::EndTurn))
    );
    assert_eq!(server.received_requests().await.unwrap().len(), 3);
}

#[tokio::test]
async fn persistent_529_becomes_a_server_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(overloaded())
        .mount(&server)
        .await;

    let err = stream_err(provider(&server.uri()).as_ref(), request(MODEL, "hi")).await;

    assert_eq!(
        err,
        ProviderError::Server {
            status: 529,
            attempts: 4,
            message: "Overloaded".into()
        }
    );
}

#[tokio::test]
async fn auth_and_bad_request_errors_are_typed_and_not_retried() {
    for (status, message) in [(401, "invalid x-api-key"), (400, "max_tokens: too large")] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(status).set_body_json(
                json!({"type": "error", "error": {"type": "some_error", "message": message}}),
            ))
            .mount(&server)
            .await;

        let err = stream_err(provider(&server.uri()).as_ref(), request(MODEL, "hi")).await;

        let expected = if status == 401 {
            ProviderError::Auth {
                status,
                message: message.into(),
            }
        } else {
            ProviderError::BadRequest {
                status,
                message: message.into(),
            }
        };
        assert_eq!(err, expected);
        assert_eq!(server.received_requests().await.unwrap().len(), 1);
    }
}
