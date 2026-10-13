//! The M2 acceptance check: one scripted agent task (call the `lookup` tool, then answer with its
//! result) passes against mocked OpenAI, Anthropic and xAI servers, with each provider built
//! through the registry exactly as the CLI builds it.

mod common;

use common::{AgentHarness, body, fast_retry, sse};
use rozu_core::{AgentEvent, ContentPart, Role, TurnOutcome};
use rozu_providers::registry;
use secrecy::SecretString;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer};

struct Case {
    spec: &'static str,
    /// Base URL suffix the adapter expects after the mock server root.
    base_suffix: &'static str,
    endpoint: &'static str,
    tool_call: &'static str,
    answer: &'static str,
    call_id: &'static str,
}

const CASES: [Case; 3] = [
    Case {
        spec: "openai/gpt-5.1",
        base_suffix: "/v1",
        endpoint: "/v1/responses",
        tool_call: include_str!("fixtures/openai/tool_call.sse"),
        answer: include_str!("fixtures/openai/after_tool.sse"),
        call_id: "call_abc123",
    },
    Case {
        spec: "anthropic/claude-sonnet-4-5",
        base_suffix: "",
        endpoint: "/v1/messages",
        tool_call: include_str!("fixtures/anthropic/tool_use.sse"),
        answer: include_str!("fixtures/anthropic/after_tool.sse"),
        call_id: "toolu_01abc",
    },
    Case {
        spec: "xai/grok-4.7",
        base_suffix: "/v1",
        endpoint: "/v1/responses",
        tool_call: include_str!("fixtures/xai/tool_call.sse"),
        answer: include_str!("fixtures/xai/after_tool.sse"),
        call_id: "call_xai_1",
    },
];

/// Where the tool result sits in the follow-up request body, per wire format.
fn sent_tool_result(provider: &str, body: &serde_json::Value) -> (String, String) {
    if provider == "anthropic" {
        let last = body["messages"].as_array().unwrap().last().unwrap();
        let block = &last["content"][0];
        assert_eq!(last["role"], "user");
        assert_eq!(block["type"], "tool_result");
        (
            block["tool_use_id"].as_str().unwrap().to_owned(),
            block["content"].as_str().unwrap().to_owned(),
        )
    } else {
        let last = body["input"].as_array().unwrap().last().unwrap();
        assert_eq!(last["type"], "function_call_output");
        (
            last["call_id"].as_str().unwrap().to_owned(),
            last["output"].as_str().unwrap().to_owned(),
        )
    }
}

#[tokio::test]
async fn same_task_passes_on_every_provider() {
    for case in CASES {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path(case.endpoint))
            .respond_with(sse(case.tool_call))
            .up_to_n_times(1)
            .expect(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path(case.endpoint))
            .respond_with(sse(case.answer))
            .expect(1)
            .mount(&server)
            .await;

        let choice = registry::resolve(None, Some(case.spec)).unwrap();
        let provider = registry::build(
            choice.provider,
            SecretString::from("test-key"),
            Some(&format!("{}{}", server.uri(), case.base_suffix)),
            Some(fast_retry()),
        )
        .unwrap();
        assert_eq!(provider.id(), choice.provider.id);

        let mut h = AgentHarness::new(provider, &choice.model, choice.info.clone());
        let (result, events) = h.run("What is the value for key `answer`?").await;

        let spec = case.spec;
        assert_eq!(result.unwrap(), TurnOutcome::Completed, "{spec}");
        assert!(
            events.contains(&AgentEvent::ToolCallFinished {
                id: case.call_id.into(),
                name: "lookup".into(),
                output: "42".into(),
                is_error: false,
            }),
            "{spec}: {events:?}"
        );
        let last = h.session.messages.last().unwrap();
        assert_eq!(last.role, Role::Assistant, "{spec}");
        assert!(last.text_content().ends_with("The answer is 42."), "{spec}");
        let roles: Vec<_> = h.session.messages.iter().map(|m| m.role).collect();
        assert_eq!(
            roles,
            [
                Role::System,
                Role::User,
                Role::Assistant,
                Role::Tool,
                Role::Assistant
            ],
            "{spec}"
        );
        assert!(
            h.session.messages[2]
                .content
                .iter()
                .any(|p| matches!(p, ContentPart::ToolCall { args_json, .. } if args_json.replace(' ', "") == r#"{"key":"answer"}"#)),
            "{spec}"
        );

        // Usage and cost were accounted for both model requests.
        assert!(h.session.last_turn.usage.input_tokens > 0, "{spec}");
        assert!(h.session.last_turn.usage.output_tokens > 0, "{spec}");
        assert!(h.session.last_turn.cost_usd.unwrap() > 0.0, "{spec}");

        let requests = server.received_requests().await.unwrap();
        assert_eq!(requests.len(), 2, "{spec}");
        let first = body(&requests[0]);
        assert_eq!(first["model"], choice.model.as_str(), "{spec}");
        assert_eq!(
            sent_tool_result(choice.provider.id, &body(&requests[1])),
            (case.call_id.to_owned(), "42".to_owned()),
            "{spec}"
        );
    }
}
