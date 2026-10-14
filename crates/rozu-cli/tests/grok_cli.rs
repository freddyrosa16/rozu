//! `rozu chat --provider grok`, `rozu auth login grok`, `rozu models` and `rozu grok-hook`
//! against the fake `grok` binary.
#![cfg(unix)]

mod common;

use common::{FakeCli, read_json_lines, stdout_of};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::{Value, json};

fn fake(scripts: &[&str]) -> FakeCli {
    let scripts: Vec<_> = scripts
        .iter()
        .map(|name| FakeCli::fixtures("grok").join(name))
        .collect();
    FakeCli::new("grok", &scripts)
}

#[test]
fn chat_with_grok_streams_text_and_usage_without_cost() {
    let fake = fake(&["handshake.script", "text_turn.script"]);

    fake.chat("grok")
        .arg("hi")
        .env("XAI_API_KEY", "xai-must-not-reach-grok")
        .write_stdin("")
        .assert()
        .success()
        .stdout("Hello from Grok!\n")
        .stderr(contains("[4591 in (1152 cached) / 111 out tokens]"))
        .stderr(contains("memory features are limited with grok"))
        .stderr(contains("$").not())
        // The CLI's own stderr goes to the log file only.
        .stderr(contains("stderr noise").not());

    let env = fake.read("log/env.txt");
    assert!(env.contains("XAI_API_KEY=unset"), "{env}");
    // Grok gets rozu's MCP server, tagged so its audit entries say `grok`.
    let new = fake
        .sent()
        .into_iter()
        .find(|m| m["method"] == "session/new")
        .unwrap();
    let args: Vec<&str> = new["params"]["mcpServers"][0]["args"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(args[..2], ["mcp-serve", "--workspace"]);
    assert!(args.ends_with(&["--provider", "grok"]), "{args:?}");
    let args = fake.read("log/args.txt");
    assert!(args.starts_with("--no-auto-update\nagent\n"), "{args}");
    assert!(args.ends_with("stdio\n--\n"), "{args}");
}

#[test]
fn grok_permission_for_a_builtin_is_denied_and_audited() {
    let fake = fake(&["handshake.script", "tool_turn.script"]);
    fake.chat("grok")
        .arg("read the readme")
        .write_stdin("")
        .assert()
        .success()
        .stdout(contains("The README says demo."))
        .stderr(contains("→ read_file"))
        .stderr(contains(
            "✗ denied: rozu denies `grok:run_terminal_cmd` by default",
        ));
    let audit = read_json_lines(&fake.path("approvals.jsonl"));
    assert!(audit.iter().all(|e| e["provider"] == "grok"), "{audit:?}");
    assert_eq!(audit[0]["tool"], "rozu:read_file");
    assert_eq!(audit[0]["decided_by"], "delegated-to-rozu-mcp");
    assert_eq!(audit[1]["tool"], "grok:run_terminal_cmd");
    assert_eq!(audit[1]["decision"], "deny");
}

#[test]
fn chat_reports_grok_not_signed_in() {
    fake(&["not_logged_in.script"])
        .chat("grok")
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("not logged in"))
        .stderr(contains("rozu auth login grok"));
}

#[test]
fn chat_reports_a_missing_grok_binary() {
    let fake = fake(&[]);
    fake.chat("grok")
        .env("ROZU_GROK_BIN", fake.path("nope/grok"))
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("ROZU_GROK_BIN"));
}

#[test]
fn auth_login_grok_runs_the_official_login() {
    let fake = fake(&[]);
    fake.rozu()
        .args(["auth", "login", "grok"])
        .write_stdin("")
        .assert()
        .success()
        .stdout(contains("fake grok: login finished"))
        .stderr(contains("official `grok login`"));
    assert_eq!(fake.read("log/login_args.txt"), "login\n");
}

#[test]
fn models_lists_grok_subscription_models_as_unverified() {
    let stdout = stdout_of(fake(&[]).rozu().arg("models"));
    let line = stdout
        .lines()
        .find(|l| l.starts_with("grok/default *"))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(
        line.contains("subscription") && line.contains("unverified"),
        "{line}"
    );
    assert!(stdout.contains("grok/grok-4.7 "), "{stdout}");
    // The API-key model of the same name is still listed, priced.
    assert!(stdout.contains("xai/grok-4.7"), "{stdout}");
}

#[test]
fn grok_hook_is_a_passthrough_by_default_and_truncates_on_request() {
    let fake = fake(&[]);
    let long = format!("HEAD{}TAIL", "x".repeat(10_000));
    let event = json!({"hookEventName": "PostToolUse", "toolName": "rozu__shell",
        "toolResult": long, "toolResultTruncated": false})
    .to_string();
    fake.rozu()
        .arg("grok-hook")
        .write_stdin(event.clone())
        .assert()
        .success()
        .stdout("");
    let stdout = stdout_of(
        fake.rozu()
            .args(["grok-hook", "--max-bytes", "200"])
            .write_stdin(event),
    );
    let out: Value = serde_json::from_str(&stdout).unwrap();
    let text = out["hookSpecificOutput"]["updatedToolOutput"]
        .as_str()
        .unwrap();
    assert!(text.starts_with("HEAD") && text.ends_with("TAIL"), "{text}");
    assert!(text.len() < long.len());
}
