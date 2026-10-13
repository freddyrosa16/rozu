//! xAI through the OpenAI Responses adapter with a custom base URL, against wiremock.

mod common;

use common::{body, collect, fast_retry, request, sse};
use rozu_core::{Provider, ProviderError, StopReason, StreamEvent};
use rozu_providers::{OpenAiProvider, xai};
use secrecy::SecretString;
use serde_json::json;
use tokio_util::sync::CancellationToken;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEXT: &str = include_str!("fixtures/openai/text.sse");
const API_KEY: &str = "xai-test-0123456789abcdef";

fn provider(base_url: &str) -> OpenAiProvider {
    let config = xai::config(SecretString::from(API_KEY))
        .with_base_url(base_url)
        .with_retry(fast_retry());
    OpenAiProvider::new(config).unwrap()
}

#[tokio::test]
async fn streams_from_xai_base_url_with_its_own_key() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .and(header(
            "authorization",
            format!("Bearer {API_KEY}").as_str(),
        ))
        .respond_with(sse(TEXT))
        .expect(1)
        .mount(&server)
        .await;

    let provider = provider(&format!("{}/v1/", server.uri()));
    assert_eq!(provider.id(), "xai");
    let events = collect(&provider, request("grok-4", "say hello")).await;

    assert_eq!(
        events.last().unwrap(),
        &Ok(StreamEvent::Done(StopReason::EndTurn))
    );
    let text: String = events
        .iter()
        .filter_map(|e| match e {
            Ok(StreamEvent::TextDelta(t)) => Some(t.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(text, "Hello, world!");
    let sent = body(&server.received_requests().await.unwrap()[0]);
    assert_eq!(sent["model"], "grok-4");
    assert_eq!(
        sent["input"],
        json!([{"role": "user", "content": "say hello"}])
    );
}

#[tokio::test]
async fn retries_429_and_types_auth_errors() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse(TEXT))
        .mount(&server)
        .await;
    let events = collect(&provider(&server.uri()), request("grok-4", "hi")).await;
    assert!(events.iter().all(Result::is_ok));
    assert_eq!(server.received_requests().await.unwrap().len(), 2);

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(401)
                .set_body_json(json!({"error": {"message": "Incorrect API key provided"}})),
        )
        .mount(&server)
        .await;
    let err = provider(&server.uri())
        .stream(request("grok-4", "hi"), CancellationToken::new())
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
}
