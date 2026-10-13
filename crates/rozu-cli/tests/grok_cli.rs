//! `rozu chat --provider grok`, `rozu auth login grok`, `rozu models` and `rozu grok-hook`
//! against the fake `grok` binary. No real Grok Build CLI is run.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::{Value, json};

const GROK_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../rozu-cli-providers/tests/fixtures/grok"
);

/// The fake grok with its script (fixture names, concatenated), log dir and a workspace.
struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new(scripts: &[&str]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["log", "workspace"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let bin = dir.path().join("grok");
        std::fs::copy(Path::new(GROK_FIXTURES).join("fake_grok.sh"), &bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script: String = scripts
            .iter()
            .map(|name| std::fs::read_to_string(Path::new(GROK_FIXTURES).join(name)).unwrap())
            .collect();
        std::fs::write(dir.path().join("script.txt"), script).unwrap();
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn rozu(&self) -> Command {
        let mut cmd = Command::cargo_bin("rozu").expect("rozu binary is built");
        cmd.env(
            "ROZU_LOG_FILE",
            Path::new(env!("CARGO_TARGET_TMPDIR")).join("rozu-cli-tests.log"),
        )
        .env("ROZU_AUDIT_LOG", self.path("approvals.jsonl"))
        .env_remove("ROZU_LOG_STDERR")
        .env_remove("RUST_LOG")
        .env_remove("ROZU_PROVIDER")
        .env_remove("ROZU_MODEL")
        .env_remove("ROZU_GROK_HEADLESS")
        .env_remove("ROZU_GROK_HOOK_MAX_BYTES")
        .env("NO_COLOR", "1")
        .env("ROZU_GROK_BIN", self.path("grok"))
        .env("FAKE_GROK_SCRIPT", self.path("script.txt"))
        .env("FAKE_GROK_LOG", self.path("log"));
        cmd
    }

    fn chat(&self) -> Command {
        let mut cmd = self.rozu();
        cmd.args(["chat", "--provider", "grok", "--workspace"])
            .arg(self.path("workspace"));
        cmd
    }

    fn sent(&self) -> Vec<Value> {
        std::fs::read_to_string(self.path("log/stdin.jsonl"))
            .unwrap_or_default()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect()
    }
}

#[test]
fn chat_with_grok_streams_text_and_usage_without_cost() {
    let fake = Fake::new(&["handshake.script", "text_turn.script"]);

    fake.chat()
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

    let env = std::fs::read_to_string(fake.path("log/env.txt")).unwrap();
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
    let args = std::fs::read_to_string(fake.path("log/args.txt")).unwrap();
    assert!(args.starts_with("--no-auto-update\nagent\n"), "{args}");
    assert!(args.ends_with("stdio\n--\n"), "{args}");
}

#[test]
fn grok_permission_for_a_builtin_is_denied_and_audited() {
    let fake = Fake::new(&["handshake.script", "tool_turn.script"]);
    fake.chat()
        .arg("read the readme")
        .write_stdin("")
        .assert()
        .success()
        .stdout(contains("The README says demo."))
        .stderr(contains("→ read_file"))
        .stderr(contains(
            "✗ denied: rozu denies `grok:run_terminal_cmd` by default",
        ));
    let audit: Vec<Value> = std::fs::read_to_string(fake.path("approvals.jsonl"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert!(audit.iter().all(|e| e["provider"] == "grok"), "{audit:?}");
    assert_eq!(audit[0]["tool"], "rozu:read_file");
    assert_eq!(audit[0]["decided_by"], "delegated-to-rozu-mcp");
    assert_eq!(audit[1]["tool"], "grok:run_terminal_cmd");
    assert_eq!(audit[1]["decision"], "deny");
}

#[test]
fn chat_reports_grok_not_signed_in() {
    let fake = Fake::new(&["not_logged_in.script"]);
    fake.chat()
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("not logged in"))
        .stderr(contains("rozu auth login grok"));
}

#[test]
fn chat_reports_a_missing_grok_binary() {
    let fake = Fake::new(&[]);
    fake.chat()
        .env("ROZU_GROK_BIN", fake.path("nope/grok"))
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("ROZU_GROK_BIN"));
}

#[test]
fn auth_login_grok_runs_the_official_login() {
    let fake = Fake::new(&[]);
    fake.rozu()
        .args(["auth", "login", "grok"])
        .write_stdin("")
        .assert()
        .success()
        .stdout(contains("fake grok: login finished"))
        .stderr(contains("official `grok login`"));
    assert_eq!(
        std::fs::read_to_string(fake.path("log/login_args.txt")).unwrap(),
        "login\n"
    );
}

#[test]
fn models_lists_grok_subscription_models_as_unverified() {
    let fake = Fake::new(&[]);
    let output = fake
        .rozu()
        .arg("models")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
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
    let fake = Fake::new(&[]);
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
    let output = fake
        .rozu()
        .args(["grok-hook", "--max-bytes", "200"])
        .write_stdin(event)
        .assert()
        .success()
        .get_output()
        .clone();
    let out: Value = serde_json::from_slice(&output.stdout).unwrap();
    let text = out["hookSpecificOutput"]["updatedToolOutput"]
        .as_str()
        .unwrap();
    assert!(text.starts_with("HEAD") && text.ends_with("TAIL"), "{text}");
    assert!(text.len() < long.len());
}
