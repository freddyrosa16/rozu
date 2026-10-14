//! `rozu chat --provider claude-cli` against the fake `claude` binary, and `rozu mcp-serve`
//! over real stdio.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};

use assert_cmd::Command;
use common::{FakeCli, json_lines, read_json_lines, rozu, stdout_of};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;

fn claude_fixture(name: &str) -> PathBuf {
    FakeCli::fixtures("claude").join(name)
}

/// A fake claude whose scenario files (`turns/<name>`) are copied from `from`.
fn fake(scenario: &[(&str, PathBuf)]) -> FakeCli {
    let fake = FakeCli::new("claude", &[]);
    for (name, from) in scenario {
        std::fs::copy(from, fake.path(&format!("turns/{name}"))).unwrap();
    }
    fake
}

fn chat(fake: &FakeCli) -> Command {
    fake.chat("claude-cli")
}

fn text_of(v: &Value) -> &str {
    v["content"][0]["text"].as_str().unwrap()
}

#[test]
fn chat_with_claude_cli_streams_text_tools_and_usage_without_cost() {
    let fake = fake(&[("turn1.jsonl", claude_fixture("tool_use.jsonl"))]);

    chat(&fake)
        .args(["--model", "opus", "summarize README"])
        .write_stdin("")
        .assert()
        .success()
        .stdout("Let me look.\nThe README says hello.\n")
        .stderr(contains("→ read_file {\"path\": \"README.md\"}"))
        .stderr(contains("✓ 2 lines"))
        .stderr(contains(
            "[22607 in (20500 cached, 2100 cache-written) / 62 out tokens]",
        ))
        .stderr(contains("$").not())
        // The CLI's own stderr goes to the log file only.
        .stderr(contains("stderr noise").not());

    let args = fake.read("log/args.txt");
    assert!(args.contains("--model\nopus\n"), "{args}");
    assert!(
        args.contains("--permission-prompt-tool\nmcp__rozu__approve\n"),
        "{args}"
    );
}

#[test]
fn claude_requests_go_through_rozu_mcp_and_its_approval_prompt() {
    // The fake claude runs the real `rozu mcp-serve` from the generated --mcp-config: a
    // workspace write (auto-approved), a force push (destructive, so it asks the user and is
    // denied without a terminal), and a permission prompt for Claude's own Bash (denied: it
    // would run outside rozu's sandbox).
    let fake = fake(&[
        ("turn1.jsonl", claude_fixture("text.jsonl")),
        (
            "mcp1.jsonl",
            Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/mcp_write_and_force_push.jsonl"),
        ),
    ]);

    chat(&fake)
        .arg("write a note, then force push")
        .write_stdin("")
        .assert()
        .success()
        .stdout("Hello again!\n")
        .stderr(contains("auto-approved (write in workspace, auto mode)"))
        .stderr(contains("approve shell? · risk: destructive"))
        .stderr(contains("$ git push --force origin main"))
        .stderr(contains("✗ denied: no terminal to ask on"))
        .stderr(contains(
            "✗ denied: rozu runs shell commands only in its sandbox",
        ));

    assert_eq!(fake.read("workspace/notes.md"), "from claude\n");
    let responses = read_json_lines(&fake.path("log/mcp1.out"));
    let by_id = |id: u64| responses.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(by_id(0)["result"]["serverInfo"]["name"], "rozu");
    assert_eq!(by_id(1)["result"]["isError"], false);
    let push = &by_id(2)["result"];
    assert_eq!(push["isError"], true, "{push}");
    assert!(text_of(push).contains("non-interactively"), "{push}");
    let verdict: Value = serde_json::from_str(text_of(&by_id(3)["result"])).unwrap();
    assert_eq!(verdict["behavior"], "deny", "{verdict}");
    assert!(
        verdict["message"]
            .as_str()
            .unwrap()
            .contains("mcp__rozu__shell"),
        "{verdict}"
    );
}

#[test]
fn chat_reports_claude_not_logged_in() {
    let fake = fake(&[("turn1.jsonl", claude_fixture("not_logged_in.jsonl"))]);
    chat(&fake)
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("not logged in"))
        .stderr(contains("run `claude`"))
        .stderr(contains("/login"));
}

#[test]
fn chat_reports_a_missing_claude_binary() {
    let fake = fake(&[]);
    chat(&fake)
        .env("ROZU_CLAUDE_BIN", fake.path("nope/claude"))
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("claude"))
        .stderr(contains("ROZU_CLAUDE_BIN"));
}

#[test]
fn models_lists_claude_cli_as_subscription() {
    let stdout = stdout_of(rozu().arg("models"));
    let line = stdout
        .lines()
        .find(|l| l.starts_with("claude-cli/sonnet *"))
        .unwrap_or_else(|| panic!("no claude-cli/sonnet in:\n{stdout}"));
    assert!(line.contains("subscription"), "{line}");
    assert!(stdout.contains("claude-cli/opus"), "{stdout}");
}

#[test]
fn mcp_serve_round_trips_over_stdio() {
    let workspace = tempfile::tempdir().unwrap();
    std::fs::write(workspace.path().join("README.md"), "# hi\n").unwrap();
    let input = [
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"test","version":"0"}}}"#,
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        r#"{"jsonrpc":"2.0","id":2,"method":"tools/list"}"#,
        r#"{"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":"read_file","arguments":{"path":"README.md"}}}"#,
        r#"{"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"shell","arguments":{"command":"rm -rf ."}}}"#,
    ]
    .join("\n")
        + "\n";

    let stdout = stdout_of(
        rozu()
            .env_remove("ROZU_APPROVAL_SOCKET")
            .args(["mcp-serve", "--workspace"])
            .arg(workspace.path())
            .write_stdin(input),
    );
    let responses = json_lines(&stdout);
    assert_eq!(responses.len(), 4, "{responses:?}");
    let by_id = |id: u64| responses.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18");
    let tools = by_id(2)["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "approve"));
    assert!(tools.iter().any(|t| t["name"] == "apply_patch"));
    let read = text_of(&by_id(3)["result"]);
    assert!(read.contains("# hi"), "{read}");
    // No parent to ask, so the destructive command is denied and nothing is deleted.
    let denied = &by_id(4)["result"];
    assert_eq!(denied["isError"], true);
    assert!(text_of(denied).contains("denied by default"), "{denied}");
    assert!(workspace.path().join("README.md").exists());
}
