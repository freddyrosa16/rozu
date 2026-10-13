use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

const TEXT_FIXTURE: &str = include_str!("../../rozu-providers/tests/fixtures/openai/text.sse");

fn rozu() -> Command {
    let mut cmd = Command::cargo_bin("rozu").expect("rozu binary is built");
    // Keep test logs out of the developer's ~/Library/Logs.
    cmd.env(
        "ROZU_LOG_FILE",
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("rozu-cli-tests.log"),
    )
    // Keep test decisions out of the developer's audit log.
    .env(
        "ROZU_AUDIT_LOG",
        std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("rozu-cli-tests-approvals.jsonl"),
    )
    .env_remove("ROZU_LOG_STDERR")
    .env_remove("RUST_LOG")
    // Never run the developer's real Codex CLI (e.g. for `rozu models`).
    .env("ROZU_CODEX_BIN", "/nonexistent/codex");
    cmd
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
        // Don't let the developer's shell pick another model or endpoint.
        .env_remove("ROZU_PROVIDER")
        .env_remove("ROZU_MODEL")
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("XAI_BASE_URL")
        .arg("chat")
        .arg("--workspace")
        .arg(workspace);
    cmd
}

const ANTHROPIC_TEXT: &str = include_str!("../../rozu-providers/tests/fixtures/anthropic/text.sse");

fn mock_server(runtime: &tokio::runtime::Runtime, body: &'static str) -> MockServer {
    runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(body))
            .mount(&server)
            .await;
        server
    })
}

#[test]
fn chat_with_anthropic_provider() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let openai = mock_server(&runtime, TEXT_FIXTURE);
    let anthropic = mock_server(&runtime, ANTHROPIC_TEXT);
    let dir = tempfile::tempdir().unwrap();

    chat(&openai, dir.path())
        .env("ANTHROPIC_API_KEY", "sk-ant-test-0123456789")
        .env("ANTHROPIC_BASE_URL", anthropic.uri())
        .args(["--provider", "anthropic", "say hello"])
        .assert()
        .success()
        .stdout("Hello, world!\n")
        .stderr(contains("[12 in / 4 out tokens · ~$"));

    let requests = runtime.block_on(anthropic.received_requests()).unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/v1/messages");
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "claude-sonnet-5-5");
    assert!(body["system"][0]["text"].as_str().unwrap().contains("rozu"));
    assert!(
        runtime
            .block_on(openai.received_requests())
            .unwrap()
            .is_empty()
    );
}

#[test]
fn repl_model_switch_applies_to_the_next_turn() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let openai = mock_server(&runtime, TEXT_FIXTURE);
    let anthropic = mock_server(&runtime, ANTHROPIC_TEXT);
    let dir = tempfile::tempdir().unwrap();

    chat(&openai, dir.path())
        .env("ANTHROPIC_API_KEY", "sk-ant-test-0123456789")
        .env("ANTHROPIC_BASE_URL", anthropic.uri())
        .write_stdin(
            "first\n/model\n/model nope/model\n/model anthropic/claude-haiku-4-5\nsecond\n",
        )
        .assert()
        .success()
        .stdout("Hello, world!\nHello, world!\n")
        .stderr(contains("model: openai/gpt-6.1-sol"))
        .stderr(contains("unknown provider `nope`"))
        .stderr(contains("switched to anthropic/claude-haiku-4-5"));

    assert_eq!(
        runtime.block_on(openai.received_requests()).unwrap().len(),
        1
    );
    let requests = runtime.block_on(anthropic.received_requests()).unwrap();
    assert_eq!(requests.len(), 1);
    let body: Value = serde_json::from_slice(&requests[0].body).unwrap();
    assert_eq!(body["model"], "claude-haiku-4-5");
    // The conversation so far carries over to the new provider.
    let texts: Vec<_> = body["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| {
            (
                m["role"].as_str().unwrap(),
                m["content"][0]["text"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        texts,
        [
            ("user", "first"),
            ("assistant", "Hello, world!"),
            ("user", "second")
        ]
    );
}

#[test]
fn chat_rejects_zero_max_steps() {
    rozu()
        .args(["chat", "--max-steps", "0", "hi"])
        .assert()
        .code(2)
        .stderr(contains("invalid value '0' for '--max-steps <MAX_STEPS>'"))
        .stderr(contains("0 is not in 1..=200"));
}

#[test]
fn chat_rejects_unknown_provider() {
    rozu()
        .env_remove("ROZU_MODEL")
        .args(["chat", "--provider", "gemini", "hi"])
        .assert()
        .failure()
        .stderr(contains("unknown provider `gemini`"));
}

#[test]
fn models_lists_every_provider_with_prices() {
    let output = rozu().arg("models").assert().success().get_output().clone();
    let stdout = String::from_utf8(output.stdout).expect("utf-8 output");
    for line in [
        "openai/gpt-6.1-sol *",
        "anthropic/claude-sonnet-5-5 *",
        "xai/grok-4.7 *",
    ] {
        assert!(stdout.contains(line), "missing {line} in:\n{stdout}");
    }
    assert!(!stdout.contains("claude-opus-4-1"), "{stdout}");
    assert!(
        stdout.contains("CONTEXT") && stdout.contains("400K"),
        "{stdout}"
    );
    // Long-context tiers are shown.
    assert!(stdout.contains(">272K"), "{stdout}");
    assert!(stdout.contains("verify them"), "{stdout}");
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

/// A mock OpenAI server whose first response calls `tool` with `args`, then answers "ok".
fn tool_then_answer(runtime: &tokio::runtime::Runtime, tool: &str, args: Value) -> MockServer {
    let added = serde_json::json!({"type": "response.output_item.added", "item": {
        "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": tool, "arguments": ""}});
    let done = serde_json::json!({"type": "response.function_call_arguments.done",
        "item_id": "fc_1", "arguments": args.to_string()});
    let tool_call = format!(
        "data: {added}\n\ndata: {done}\n\ndata: {{\"type\":\"response.completed\",\"response\":{{}}}}\n\n"
    );
    let answer = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n\
                  data: {\"type\":\"response.completed\",\"response\":{}}\n\n";
    runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(sse(&tool_call))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .respond_with(sse(answer))
            .mount(&server)
            .await;
        server
    })
}

fn last_tool_output(runtime: &tokio::runtime::Runtime, server: &MockServer) -> String {
    let requests = runtime.block_on(server.received_requests()).unwrap();
    assert_eq!(requests.len(), 2);
    let second: Value = serde_json::from_slice(&requests[1].body).unwrap();
    let last = second["input"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["type"], "function_call_output");
    last["output"].as_str().unwrap().to_owned()
}

#[test]
fn network_command_prompts_and_is_denied_without_a_tty() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = tool_then_answer(
        &runtime,
        "shell",
        serde_json::json!({"command": "curl https://example.com"}),
    );
    let dir = tempfile::tempdir().unwrap();

    chat(&server, dir.path())
        .arg("fetch it")
        .write_stdin("")
        .assert()
        .success()
        .stdout("ok\n")
        .stderr(contains("approve shell? · risk: network"))
        .stderr(contains("│ $ curl https://example.com"))
        .stderr(contains("network: on"))
        .stderr(contains("✗ denied"));

    let output = last_tool_output(&runtime, &server);
    assert!(
        output.starts_with("denied:") && output.contains("non-interactively"),
        "{output}"
    );
}

/// BUG-8: one denial, one line (not the card's line plus the decision plus the tool result).
#[test]
fn denial_is_printed_once() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = tool_then_answer(
        &runtime,
        "shell",
        serde_json::json!({"command": "curl https://example.com"}),
    );
    let dir = tempfile::tempdir().unwrap();

    let output = chat(&server, dir.path())
        .arg("fetch it")
        .write_stdin("")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(stderr.matches("✗").count(), 1, "{stderr}");
    assert_eq!(stderr.matches("denied").count(), 1, "{stderr}");
    assert_eq!(
        stderr.matches("no terminal to ask on").count(),
        1,
        "{stderr}"
    );
}

/// BUG-5 end to end: full mode still asks before interpreter code, and (denied without a TTY)
/// the canary outside the workspace survives.
#[test]
fn full_mode_prompts_before_interpreter_code() {
    let outside = tempfile::tempdir().unwrap();
    let canary = outside.path().join("c1.txt");
    std::fs::write(&canary, "canary").unwrap();
    let command = format!(
        "python3 -c \"import os; os.remove('{}')\"",
        canary.display()
    );
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = tool_then_answer(&runtime, "shell", serde_json::json!({ "command": command }));
    let dir = tempfile::tempdir().unwrap();

    chat(&server, dir.path())
        .args(["--approval", "full", "clean up"])
        .write_stdin("")
        .assert()
        .success()
        .stderr(contains(
            "approve shell? · risk: opaque (interpreter code or output redirect)",
        ))
        .stderr(contains("auto-approved").not());

    assert_eq!(std::fs::read_to_string(&canary).unwrap(), "canary");
    assert!(last_tool_output(&runtime, &server).starts_with("denied:"));
}

/// BUG-7 (REG-6) end to end: the CLI's decisions land in ROZU_AUDIT_LOG.
#[test]
fn chat_decisions_go_to_the_audit_log() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = tool_then_answer(
        &runtime,
        "shell",
        serde_json::json!({"command": "curl https://example.com"}),
    );
    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let audit = logs.path().join("audit/approvals.jsonl");

    chat(&server, dir.path())
        .env("ROZU_AUDIT_LOG", &audit)
        .arg("fetch it")
        .write_stdin("")
        .assert()
        .success();

    let text = std::fs::read_to_string(&audit).unwrap();
    let entries: Vec<Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(entries.len(), 1, "{text}");
    let entry = &entries[0];
    assert_eq!(entry["tool"], "shell");
    assert_eq!(entry["decision"], "deny");
    assert_eq!(entry["decided_by"], "user");
    assert_eq!(entry["mode"], "auto");
    assert_eq!(entry["risk"], "network");
    assert_eq!(entry["preview"], "$ curl https://example.com");
    assert!(entry["session_id"].as_str().unwrap().len() > 10);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&audit).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
    }
}

#[test]
fn auto_mode_writes_files_without_prompting() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = tool_then_answer(
        &runtime,
        "write_file",
        serde_json::json!({"path": "notes/todo.md", "content": "- ship M3\n"}),
    );
    let dir = tempfile::tempdir().unwrap();

    chat(&server, dir.path())
        .arg("write a todo")
        .write_stdin("")
        .assert()
        .success()
        .stderr(contains("auto-approved (write in workspace, auto mode)"))
        .stderr(contains("approve write_file?").not());

    assert_eq!(
        std::fs::read_to_string(dir.path().join("notes/todo.md")).unwrap(),
        "- ship M3\n"
    );
    assert!(last_tool_output(&runtime, &server).starts_with("created"));
}

#[test]
fn read_only_mode_denies_writes_without_a_tty() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = tool_then_answer(
        &runtime,
        "write_file",
        serde_json::json!({"path": "x.txt", "content": "x"}),
    );
    let dir = tempfile::tempdir().unwrap();

    chat(&server, dir.path())
        .args(["--approval", "read-only", "write x"])
        .write_stdin("")
        .assert()
        .success()
        .stderr(contains("approve write_file? · risk: write in workspace"));

    assert!(!dir.path().join("x.txt").exists());
    assert!(last_tool_output(&runtime, &server).starts_with("denied:"));
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

/// A mock OpenAI server that always fails with 500, so the retry loop logs `warn!`s.
fn failing_server(runtime: &tokio::runtime::Runtime) -> MockServer {
    runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(
                ResponseTemplate::new(500)
                    .insert_header("retry-after-ms", "1")
                    .set_body_string("boom"),
            )
            .mount(&server)
            .await;
        server
    })
}

#[test]
fn warnings_go_to_the_log_file_not_stderr() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = failing_server(&runtime);
    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let log_file = logs.path().join("sub/rozu.log");

    let output = chat(&server, dir.path())
        .env("ROZU_LOG_FILE", &log_file)
        .arg("hi")
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("error:"), "{stderr}");
    assert!(
        !stderr.contains("WARN") && !stderr.contains("retrying request"),
        "{stderr}"
    );

    let logged = std::fs::read_to_string(&log_file).unwrap();
    assert!(
        logged.contains("WARN") && logged.contains("retrying request"),
        "{logged}"
    );
    // Plain text (no ANSI colours) and still redacted.
    assert!(!logged.contains('\x1b'), "{logged}");
    assert!(!logged.contains("0123456789abcdef"), "{logged}");
}

#[test]
fn rozu_log_stderr_brings_logs_back_to_the_terminal() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = failing_server(&runtime);
    let dir = tempfile::tempdir().unwrap();

    chat(&server, dir.path())
        .env("ROZU_LOG_STDERR", "1")
        .arg("hi")
        .assert()
        .failure()
        .stderr(contains("WARN").and(contains("retrying request")));
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
