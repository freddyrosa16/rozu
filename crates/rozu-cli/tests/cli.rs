use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEXT_FIXTURE: &str = include_str!("../../rozu-providers/tests/fixtures/openai/text.sse");

fn rozu() -> Command {
    Command::cargo_bin("rozu").expect("rozu binary is built")
}

fn sse(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

/// `rozu chat` pointed at a mock OpenAI server with a throwaway key.
fn chat(server: &MockServer, workspace: &std::path::Path) -> Command {
    let mut cmd = rozu();
    cmd.env("OPENAI_API_KEY", "sk-test-0123456789abcdef")
        .env("OPENAI_BASE_URL", server.uri())
        .env("NO_COLOR", "1")
        .arg("chat")
        .arg("--workspace")
        .arg(workspace);
    cmd
}

#[test]
fn chat_streams_answer_to_stdout() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(TEXT_FIXTURE))
            .mount(&server)
            .await;
        server
    });
    let dir = tempfile::tempdir().unwrap();

    chat(&server, dir.path())
        .arg("say hello")
        .assert()
        .success()
        .stdout("Hello, world!\n")
        .stderr(contains("12 in / 4 out tokens"));
}

#[test]
fn chat_runs_tools_in_the_workspace() {
    let tool_call = concat!(
        "data: {\"type\":\"response.output_item.added\",\"item\":{\"type\":\"function_call\",\"id\":\"fc_1\",\"call_id\":\"call_1\",\"name\":\"list_dir\",\"arguments\":\"\"}}\n\n",
        "data: {\"type\":\"response.function_call_arguments.done\",\"item_id\":\"fc_1\",\"arguments\":\"{}\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":10,\"output_tokens\":5}}}\n\n",
    );
    let answer = concat!(
        "data: {\"type\":\"response.output_text.delta\",\"delta\":\"There is one file.\"}\n\n",
        "data: {\"type\":\"response.completed\",\"response\":{\"usage\":{\"input_tokens\":20,\"output_tokens\":5}}}\n\n",
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(tool_call))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(sse(answer))
            .mount(&server)
            .await;
        server
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "# hi").unwrap();

    chat(&server, dir.path())
        .args(["list", "the", "files"])
        .assert()
        .success()
        .stdout("There is one file.\n")
        .stderr(contains("→ list_dir {}"))
        .stderr(contains("✓ 1 line"));

    let requests = runtime.block_on(server.received_requests()).unwrap();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let input = second["input"].as_array().unwrap();
    assert_eq!(input[0]["role"], "system");
    assert_eq!(input[1]["content"], "list the files");
    assert_eq!(input.last().unwrap()["type"], "function_call_output");
    assert_eq!(input.last().unwrap()["output"], "README.md");
}

#[test]
fn chat_reports_provider_errors() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(401)
                    .set_body_string(r#"{"error":{"message":"Incorrect API key provided"}}"#),
            )
            .mount(&server)
            .await;
        server
    });
    let dir = tempfile::tempdir().unwrap();

    chat(&server, dir.path())
        .arg("hi")
        .assert()
        .failure()
        .stderr(contains("authentication failed (HTTP 401)"))
        .stderr(contains("sk-test-0123456789abcdef").not());
}

#[test]
fn auth_set_rejects_empty_input() {
    rozu()
        .args(["auth", "set", "openai"])
        .write_stdin("\n")
        .assert()
        .failure()
        .stderr(contains("no API key given"));
}

#[test]
fn prints_version() {
    rozu()
        .arg("--version")
        .assert()
        .success()
        .stdout(contains(format!("rozu {}", env!("CARGO_PKG_VERSION"))));
}

#[test]
fn doctor_lists_every_crate() {
    let output = rozu().arg("doctor").assert().success().get_output().clone();
    let stdout = String::from_utf8(output.stdout).expect("utf-8 output");
    for name in [
        "rozu-cli",
        "rozu-core",
        "rozu-providers",
        "rozu-tools",
        "rozu-pty",
        "rozu-secrets",
        "rozu-store",
    ] {
        assert!(stdout.contains(name), "missing {name} in:\n{stdout}");
    }
}
