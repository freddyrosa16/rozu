//! `rozu chat --provider codex`, `rozu auth login codex` and `rozu models` against the fake
//! `codex` binary. No real Codex CLI is run.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;

const CODEX_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../rozu-cli-providers/tests/fixtures/codex"
);
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

/// The fake codex with its script (fixture paths, concatenated), log dir and a workspace.
struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new(scripts: &[PathBuf]) -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["log", "workspace"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let bin = dir.path().join("codex");
        std::fs::copy(Path::new(CODEX_FIXTURES).join("fake_codex.sh"), &bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script: String = scripts
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
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
        .env("NO_COLOR", "1")
        .env("ROZU_CODEX_BIN", self.path("codex"))
        .env("FAKE_CODEX_SCRIPT", self.path("script.txt"))
        .env("FAKE_CODEX_LOG", self.path("log"));
        cmd
    }

    fn chat(&self) -> Command {
        let mut cmd = self.rozu();
        cmd.args(["chat", "--provider", "codex", "--workspace"])
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

fn codex_fixture(name: &str) -> PathBuf {
    Path::new(CODEX_FIXTURES).join(name)
}

#[test]
fn chat_with_codex_streams_text_and_usage_without_cost() {
    let fake = Fake::new(&[
        codex_fixture("handshake.script"),
        codex_fixture("text_turn.script"),
    ]);

    fake.chat()
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
    let args = std::fs::read_to_string(fake.path("log/args.txt")).unwrap();
    assert!(args.contains("approval_policy=\"untrusted\"\n"), "{args}");
    assert!(args.contains("\"mcp-serve\", \"--workspace\""), "{args}");
    assert!(args.contains("\"--provider\", \"codex\"]"), "{args}");
    assert!(args.ends_with("app-server\n--\n"), "{args}");
}

#[test]
fn codex_force_push_shows_rozus_approval_card() {
    let fake = Fake::new(&[
        codex_fixture("handshake.script"),
        Path::new(FIXTURES).join("codex_force_push.script"),
    ]);

    fake.chat()
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
    let audit = std::fs::read_to_string(fake.path("approvals.jsonl")).unwrap();
    let entry: Value = serde_json::from_str(audit.lines().next().unwrap()).unwrap();
    assert_eq!(entry["provider"], "codex");
    assert_eq!(entry["tool"], "codex:shell");
    assert_eq!(entry["decision"], "deny");
    assert_eq!(entry["decided_by"], "user");
}

#[test]
fn chat_reports_codex_not_logged_in() {
    let fake = Fake::new(&[codex_fixture("not_logged_in.script")]);
    fake.chat()
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("not logged in"))
        .stderr(contains("rozu auth login codex"));
}

#[test]
fn chat_reports_a_missing_codex_binary() {
    let fake = Fake::new(&[]);
    fake.chat()
        .env("ROZU_CODEX_BIN", fake.path("nope/codex"))
        .arg("hi")
        .write_stdin("")
        .assert()
        .failure()
        .stderr(contains("ROZU_CODEX_BIN"));
}

#[test]
fn auth_login_codex_runs_the_official_login() {
    let fake = Fake::new(&[]);
    fake.rozu()
        .args(["auth", "login", "codex"])
        .write_stdin("")
        .assert()
        .success()
        .stdout(contains("fake codex: login finished"))
        .stderr(contains("official `codex login`"));
    assert_eq!(
        std::fs::read_to_string(fake.path("log/login_args.txt")).unwrap(),
        "login\n"
    );
}

#[test]
fn models_lists_codex_models_from_the_app_server_or_a_static_list() {
    let fake = Fake::new(&[codex_fixture("models.script")]);
    let output = fake
        .rozu()
        .arg("models")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(stdout.contains("codex/default *"), "{stdout}");
    assert!(
        stdout.contains("codex/gpt-6.1-sol (codex default)"),
        "{stdout}"
    );
    assert!(stdout.contains("codex/gpt-6-luna"), "{stdout}");
    assert!(!stdout.contains("codex/internal"), "hidden: {stdout}");
    assert!(
        stdout.contains("comes from the installed Codex CLI"),
        "{stdout}"
    );

    let output = fake
        .rozu()
        .env("ROZU_CODEX_BIN", fake.path("nope/codex"))
        .arg("models")
        .assert()
        .success()
        .get_output()
        .clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
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
