//! `rozu chat --provider codex`, `rozu auth login codex` and `rozu models` against the fake
//! `codex` binary.
#![cfg(unix)]

mod common;

use std::path::{Path, PathBuf};

use common::{FakeCli, read_json_lines, stdout_of};
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;

fn codex_fixture(name: &str) -> PathBuf {
    FakeCli::fixtures("codex").join(name)
}

fn fake(scripts: &[&str]) -> FakeCli {
    let scripts: Vec<_> = scripts.iter().map(|name| codex_fixture(name)).collect();
    FakeCli::new("codex", &scripts)
}

#[test]
fn chat_with_codex_streams_text_and_usage_without_cost() {
    let fake = fake(&["handshake.script", "text_turn.script"]);

    fake.chat("codex")
        .arg("hi")
        .write_stdin("")
        .assert()
        .success()
        .stdout("Hello from Codex!\n")
        .stderr(contains("[1200 in (1000 cached) / 30 out tokens]"))
        .stderr(contains("$").not())
        // The CLI's own stderr goes to the log file only.
        .stderr(contains("stderr noise").not());

    // Codex gets rozu's MCP server, tagged so its audit entries say `codex`.
    let args = fake.read("log/args.txt");
    assert!(args.contains("approval_policy=\"on-request\"\n"), "{args}");
    assert!(args.contains("\"mcp-serve\", \"--workspace\""), "{args}");
    assert!(args.contains("\"--provider\", \"codex\"]"), "{args}");
    assert!(args.ends_with("app-server\n--\n"), "{args}");
}

#[test]
fn codex_forwards_only_mcp_logging_overrides_and_strips_api_keys() {
    let fake = fake(&["handshake.script", "text_turn.script"]);
    fake.chat("codex")
        .env("OPENAI_API_KEY", "synthetic-openai-key")
        .env("CODEX_API_KEY", "synthetic-codex-key")
        .arg("hi")
        .write_stdin("")
        .assert()
        .success();

    let args = fake.read("log/args.txt");
    let forwarded: Vec<String> = serde_json::from_str(
        args.lines()
            .find_map(|line| line.strip_prefix("mcp_servers.rozu.env_vars="))
            .expect("MCP logging overrides are explicitly forwarded"),
    )
    .unwrap();
    assert_eq!(
        forwarded,
        [
            "ROZU_AUDIT_LOG",
            "ROZU_LOG_FILE",
            "ROZU_LOG_STDERR",
            "RUST_LOG"
        ]
    );
    assert_eq!(
        fake.read("log/env.txt"),
        "OPENAI_API_KEY=unset\nCODEX_API_KEY=unset\n"
    );
    assert!(!args.contains("synthetic-openai-key"));
    assert!(!args.contains("synthetic-codex-key"));
}

#[test]
fn codex_force_push_shows_rozus_approval_card() {
    let fake = FakeCli::new(
        "codex",
        &[
            codex_fixture("handshake.script"),
            Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/codex_force_push.script"),
        ],
    );

    fake.chat("codex")
        .arg("force push it")
        .write_stdin("")
        .assert()
        .success()
        .stdout("I won't force push.\n")
        .stderr(contains("approve codex:shell? · risk: destructive"))
        .stderr(contains("│ $ git push --force origin main"))
        .stderr(contains("sandbox: codex's own"))
        .stderr(contains("✗ denied: no terminal to ask on"));

    let reply = fake
        .sent()
        .into_iter()
        .find(|m| m["id"] == "srv-1")
        .expect("rozu answered the approval request");
    assert_eq!(reply["result"]["decision"], "decline");
    let entry = &read_json_lines(&fake.path("approvals.jsonl"))[0];
    assert_eq!(entry["provider"], "codex");
    assert_eq!(entry["tool"], "codex:shell");
    assert_eq!(entry["decision"], "deny");
    assert_eq!(entry["decided_by"], "user");
}

#[test]
fn chat_reports_codex_not_logged_in() {
    fake(&["not_logged_in.script"])
        .chat("codex")
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("not logged in"))
        .stderr(contains("rozu auth login codex"));
}

#[test]
fn chat_reports_a_missing_codex_binary() {
    let fake = fake(&[]);
    fake.chat("codex")
        .env("ROZU_CODEX_BIN", fake.path("nope/codex"))
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("ROZU_CODEX_BIN"));
}

#[test]
fn auth_login_codex_runs_the_official_login() {
    for code in [0, 42, 255] {
        let fake = fake(&[]);
        fake.rozu()
            .args(["auth", "login", "codex"])
            .env("FAKE_CODEX_LOGIN_EXIT", code.to_string())
            .write_stdin("")
            .assert()
            .code(code)
            .stdout(contains("fake codex: login finished"))
            .stderr(contains("official `codex login`"));
        assert_eq!(fake.read("log/login_args.txt"), "login\n");
    }
}

#[test]
fn models_lists_codex_models_from_the_app_server_or_a_static_list() {
    let fake = fake(&["models.script"]);
    let stdout = stdout_of(fake.rozu().arg("models"));
    for expected in [
        "codex/default *",
        "codex/gpt-6.1-sol (codex default)",
        "codex/gpt-6-luna",
        "comes from the installed Codex CLI",
    ] {
        assert!(
            stdout.contains(expected),
            "missing {expected} in:\n{stdout}"
        );
    }
    assert!(!stdout.contains("codex/internal"), "hidden: {stdout}");

    let stdout = stdout_of(
        fake.rozu()
            .env("ROZU_CODEX_BIN", fake.path("nope/codex"))
            .arg("models"),
    );
    let line = stdout
        .lines()
        .find(|l| l.starts_with("codex/default *"))
        .unwrap_or_else(|| panic!("{stdout}"));
    assert!(
        line.contains("subscription") && line.contains("unverified"),
        "{line}"
    );
    assert!(stdout.contains("static, unverified"), "{stdout}");
}
