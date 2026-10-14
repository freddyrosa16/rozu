mod common;

use assert_cmd::Command;
use common::{Api, OPENAI_KEY, read_json_lines, rozu, sse, stdout_of};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::{Value, json};
use wiremock::ResponseTemplate;

const TEXT_FIXTURE: &str = include_str!("../../rozu-providers/tests/fixtures/openai/text.sse");
const ANTHROPIC_TEXT: &str = include_str!("../../rozu-providers/tests/fixtures/anthropic/text.sse");

fn chat(api: &Api, workspace: &std::path::Path) -> Command {
    let mut cmd = rozu();
    cmd.env("OPENAI_API_KEY", OPENAI_KEY)
        .env("OPENAI_BASE_URL", api.uri())
        .env_remove("ANTHROPIC_BASE_URL")
        .env_remove("XAI_BASE_URL")
        .arg("chat")
        .arg("--workspace")
        .arg(workspace);
    cmd
}

/// An OpenAI mock whose first response calls `tool` with `args`, then answers "ok".
fn tool_then_answer(tool: &str, args: Value) -> Api {
    let added = json!({"type": "response.output_item.added", "item": {
        "type": "function_call", "id": "fc_1", "call_id": "call_1", "name": tool, "arguments": ""}});
    let done = json!({"type": "response.function_call_arguments.done",
        "item_id": "fc_1", "arguments": args.to_string()});
    let tool_call = format!(
        "data: {added}\n\ndata: {done}\n\ndata: {{\"type\":\"response.completed\",\"response\":{{}}}}\n\n"
    );
    let answer = "data: {\"type\":\"response.output_text.delta\",\"delta\":\"ok\"}\n\n\
                  data: {\"type\":\"response.completed\",\"response\":{}}\n\n";
    Api::new(vec![sse(&tool_call), sse(answer)])
}

fn curl_then_answer() -> Api {
    tool_then_answer("shell", json!({"command": "curl https://example.com"}))
}

fn last_tool_output(api: &Api) -> String {
    let bodies = api.bodies();
    assert_eq!(bodies.len(), 2);
    let last = bodies[1]["input"]
        .as_array()
        .unwrap()
        .last()
        .unwrap()
        .clone();
    assert_eq!(last["type"], "function_call_output");
    last["output"].as_str().unwrap().to_owned()
}

/// Always fails with 500, so the retry loop logs `warn!`s.
fn failing_api() -> Api {
    Api::new(vec![
        ResponseTemplate::new(500)
            .insert_header("retry-after-ms", "1")
            .set_body_string("boom"),
    ])
}

#[test]
fn chat_with_anthropic_provider() {
    let openai = Api::sse(TEXT_FIXTURE);
    let anthropic = Api::sse(ANTHROPIC_TEXT);
    let dir = tempfile::tempdir().unwrap();

    chat(&openai, dir.path())
        .env("ANTHROPIC_API_KEY", "sk-ant-test-0123456789")
        .env("ANTHROPIC_BASE_URL", anthropic.uri())
        .args(["--provider", "anthropic", "say hello"])
        .assert()
        .success()
        .stdout("Hello, world!\n")
        .stderr(contains("[12 in / 4 out tokens · ~$"));

    let requests = anthropic.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url.path(), "/v1/messages");
    let body = &anthropic.bodies()[0];
    assert_eq!(body["model"], "claude-sonnet-5-5");
    assert!(body["system"][0]["text"].as_str().unwrap().contains("rozu"));
    assert!(openai.requests().is_empty());
}

#[test]
fn repl_model_switch_applies_to_the_next_turn() {
    let openai = Api::sse(TEXT_FIXTURE);
    let anthropic = Api::sse(ANTHROPIC_TEXT);
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

    assert_eq!(openai.requests().len(), 1);
    let bodies = anthropic.bodies();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["model"], "claude-haiku-4-5");
    let texts: Vec<_> = bodies[0]["messages"]
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
        .args(["chat", "--provider", "gemini", "hi"])
        .assert()
        .failure()
        .stderr(contains("unknown provider `gemini`"));
}

#[test]
fn models_lists_every_provider_with_prices() {
    let stdout = stdout_of(rozu().arg("models"));
    for line in [
        "openai/gpt-6.1-sol *",
        "anthropic/claude-sonnet-5-5 *",
        "xai/grok-4.7 *",
        "CONTEXT",
        "400K",
        ">272K",
        "verify them",
    ] {
        assert!(stdout.contains(line), "missing {line} in:\n{stdout}");
    }
    assert!(!stdout.contains("claude-opus-4-1"), "{stdout}");
}

#[test]
fn chat_streams_answer_to_stdout() {
    let api = Api::sse(TEXT_FIXTURE);
    let dir = tempfile::tempdir().unwrap();

    chat(&api, dir.path())
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
    let api = Api::new(vec![sse(tool_call), sse(answer)]);
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("README.md"), "# hi").unwrap();

    chat(&api, dir.path())
        .args(["list", "the", "files"])
        .assert()
        .success()
        .stdout("There is one file.\n")
        .stderr(contains("→ list_dir {}"))
        .stderr(contains("✓ 1 line"));

    let bodies = api.bodies();
    assert_eq!(bodies.len(), 2);
    let input = bodies[1]["input"].as_array().unwrap();
    assert_eq!(input[0]["role"], "system");
    assert_eq!(input[1]["content"], "list the files");
    assert_eq!(input.last().unwrap()["type"], "function_call_output");
    assert_eq!(input.last().unwrap()["output"], "README.md");
}

#[test]
fn network_command_prompts_and_is_denied_without_a_tty() {
    let api = curl_then_answer();
    let dir = tempfile::tempdir().unwrap();

    chat(&api, dir.path())
        .arg("fetch it")
        .write_stdin("")
        .assert()
        .success()
        .stdout("ok\n")
        .stderr(contains("approve shell? · risk: network"))
        .stderr(contains("│ $ curl https://example.com"))
        .stderr(contains("network: on"))
        .stderr(contains("✗ denied"));

    let output = last_tool_output(&api);
    assert!(
        output.starts_with("denied:") && output.contains("non-interactively"),
        "{output}"
    );
}

/// BUG-8: one denial, one line (not the card's line plus the decision plus the tool result).
#[test]
fn denial_is_printed_once() {
    let api = curl_then_answer();
    let dir = tempfile::tempdir().unwrap();

    let output = chat(&api, dir.path())
        .arg("fetch it")
        .write_stdin("")
        .output()
        .unwrap();

    let stderr = String::from_utf8_lossy(&output.stderr);
    for needle in ["✗", "denied", "no terminal to ask on"] {
        assert_eq!(stderr.matches(needle).count(), 1, "{needle}: {stderr}");
    }
}

/// BUG-5: full mode still asks before interpreter code, so the canary outside the workspace
/// survives when that is denied.
#[test]
fn full_mode_prompts_before_interpreter_code() {
    let outside = tempfile::tempdir().unwrap();
    let canary = outside.path().join("c1.txt");
    std::fs::write(&canary, "canary").unwrap();
    let command = format!(
        "python3 -c \"import os; os.remove('{}')\"",
        canary.display()
    );
    let api = tool_then_answer("shell", json!({ "command": command }));
    let dir = tempfile::tempdir().unwrap();

    chat(&api, dir.path())
        .args(["--approval", "full", "clean up"])
        .write_stdin("")
        .assert()
        .success()
        .stderr(contains(
            "approve shell? · risk: opaque (interpreter code or output redirect)",
        ))
        .stderr(contains("auto-approved").not());

    assert_eq!(std::fs::read_to_string(&canary).unwrap(), "canary");
    assert!(last_tool_output(&api).starts_with("denied:"));
}

/// BUG-7 (REG-6)
#[test]
fn chat_decisions_go_to_the_audit_log() {
    let api = curl_then_answer();
    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let audit = logs.path().join("audit/approvals.jsonl");

    chat(&api, dir.path())
        .env("ROZU_AUDIT_LOG", &audit)
        .arg("fetch it")
        .write_stdin("")
        .assert()
        .success();

    let entries = read_json_lines(&audit);
    assert_eq!(entries.len(), 1, "{entries:?}");
    let entry = &entries[0];
    for (key, value) in [
        ("tool", "shell"),
        ("decision", "deny"),
        ("decided_by", "user"),
        ("mode", "auto"),
        ("risk", "network"),
        ("preview", "$ curl https://example.com"),
    ] {
        assert_eq!(entry[key], value, "{key}");
    }
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
    let api = tool_then_answer(
        "write_file",
        json!({"path": "notes/todo.md", "content": "- ship M3\n"}),
    );
    let dir = tempfile::tempdir().unwrap();

    chat(&api, dir.path())
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
    assert!(last_tool_output(&api).starts_with("created"));
}

#[test]
fn read_only_mode_denies_writes_without_a_tty() {
    let api = tool_then_answer("write_file", json!({"path": "x.txt", "content": "x"}));
    let dir = tempfile::tempdir().unwrap();

    chat(&api, dir.path())
        .args(["--approval", "read-only", "write x"])
        .write_stdin("")
        .assert()
        .success()
        .stderr(contains("approve write_file? · risk: write in workspace"));

    assert!(!dir.path().join("x.txt").exists());
    assert!(last_tool_output(&api).starts_with("denied:"));
}

#[test]
fn chat_reports_provider_errors() {
    let api =
        Api::new(vec![ResponseTemplate::new(401).set_body_string(
            r#"{"error":{"message":"Incorrect API key provided"}}"#,
        )]);
    let dir = tempfile::tempdir().unwrap();

    chat(&api, dir.path())
        .arg("hi")
        .assert()
        .failure()
        .stderr(contains("authentication failed (HTTP 401)"))
        .stderr(contains(OPENAI_KEY).not());
}

#[test]
fn warnings_go_to_the_log_file_not_stderr() {
    let api = failing_api();
    let dir = tempfile::tempdir().unwrap();
    let logs = tempfile::tempdir().unwrap();
    let log_file = logs.path().join("sub/rozu.log");

    let output = chat(&api, dir.path())
        .env("ROZU_LOG_FILE", &log_file)
        .arg("hi")
        .assert()
        .failure()
        .get_output()
        .clone();
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("error:"), "{stderr}");
    // The Linux "NO OS SANDBOX" banner says "WARNING", so match the level token with its spaces.
    assert!(
        !stderr.contains(" WARN ") && !stderr.contains("retrying request"),
        "{stderr}"
    );

    let logged = std::fs::read_to_string(&log_file).unwrap();
    assert!(
        logged.contains("WARN") && logged.contains("retrying request"),
        "{logged}"
    );
    assert!(!logged.contains('\x1b'), "{logged}");
    assert!(!logged.contains("0123456789abcdef"), "{logged}");
}

#[test]
fn rozu_log_stderr_brings_logs_back_to_the_terminal() {
    let api = failing_api();
    let dir = tempfile::tempdir().unwrap();

    chat(&api, dir.path())
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
    let stdout = stdout_of(rozu().arg("doctor"));
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

#[test]
fn a_chat_is_saved_and_resumed_with_project_memory() {
    let api = Api::sse(TEXT_FIXTURE);
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("ROZU.md"), "Always answer in haiku.").unwrap();
    let run = |args: &[&str]| {
        let mut cmd = chat(&api, workspace.path());
        cmd.env("ROZU_SESSIONS_DIR", sessions.path()).args(args);
        stdout_of(&mut cmd)
    };
    run(&["first question"]);
    run(&["--resume", "--", "second question"]);

    let bodies = api.bodies();
    assert_eq!(bodies.len(), 2);
    let second = bodies[1].to_string();
    for expected in [
        "first question",
        "second question",
        "Always answer in haiku.",
    ] {
        assert!(second.contains(expected), "{expected}: {second}");
    }

    let mut list = rozu();
    list.env("ROZU_SESSIONS_DIR", sessions.path())
        .arg("sessions")
        .arg("--workspace")
        .arg(workspace.path());
    let listed = stdout_of(&mut list);
    assert_eq!(listed.lines().count(), 1, "{listed}");
    assert!(
        listed.contains("openai/") && listed.contains("4 msgs  first question"),
        "{listed}"
    );
}

#[test]
fn resume_without_a_saved_chat_fails() {
    let api = Api::sse(TEXT_FIXTURE);
    let workspace = tempfile::tempdir().unwrap();
    let sessions = tempfile::tempdir().unwrap();
    chat(&api, workspace.path())
        .env("ROZU_SESSIONS_DIR", sessions.path())
        .args(["--resume", "nope", "hi"])
        .assert()
        .failure()
        .stderr(contains("no saved session matches `nope`"));
}
