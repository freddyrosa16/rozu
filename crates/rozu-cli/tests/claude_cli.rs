//! `rozu chat --provider claude-cli` against the fake `claude` binary, and `rozu mcp-serve`
//! over real stdio. No real claude CLI is run.

#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use predicates::prelude::PredicateBooleanExt;
use predicates::str::contains;
use serde_json::Value;

const CLAUDE_FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../rozu-cli-providers/tests/fixtures/claude"
);
const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn rozu() -> Command {
    let mut cmd = Command::cargo_bin("rozu").expect("rozu binary is built");
    cmd.env(
        "ROZU_LOG_FILE",
        Path::new(env!("CARGO_TARGET_TMPDIR")).join("rozu-cli-tests.log"),
    )
    // Keep test decisions out of the developer's audit log.
    .env(
        "ROZU_AUDIT_LOG",
        Path::new(env!("CARGO_TARGET_TMPDIR")).join("rozu-cli-tests-approvals.jsonl"),
    )
    .env_remove("ROZU_LOG_STDERR")
    .env_remove("RUST_LOG")
    .env_remove("ROZU_PROVIDER")
    .env_remove("ROZU_MODEL")
    .env("NO_COLOR", "1");
    cmd
}

/// The fake claude, its scenario and log directories, and a workspace.
struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        for sub in ["turns", "log", "workspace"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let bin = dir.path().join("claude");
        std::fs::copy(Path::new(CLAUDE_FIXTURES).join("fake_claude.sh"), &bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn scenario(&self, name: &str, from: &Path) -> &Self {
        std::fs::copy(from, self.path(&format!("turns/{name}"))).unwrap();
        self
    }

    fn chat(&self) -> Command {
        let mut cmd = rozu();
        cmd.env("ROZU_CLAUDE_BIN", self.path("claude"))
            .env("FAKE_CLAUDE_TURNS", self.path("turns"))
            .env("FAKE_CLAUDE_LOG", self.path("log"))
            .args(["chat", "--provider", "claude-cli", "--workspace"])
            .arg(self.path("workspace"));
        cmd
    }
}

#[test]
fn chat_with_claude_cli_streams_text_tools_and_usage_without_cost() {
    let fake = Fake::new();
    fake.scenario(
        "turn1.jsonl",
        &Path::new(CLAUDE_FIXTURES).join("tool_use.jsonl"),
    );

    fake.chat()
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

    let args = std::fs::read_to_string(fake.path("log/args.txt")).unwrap();
    assert!(args.contains("--model\nopus\n"), "{args}");
    assert!(
        args.contains("--permission-prompt-tool\nmcp__rozu__approve\n"),
        "{args}"
    );
}

#[test]
fn claude_requests_go_through_rozu_mcp_and_its_approval_prompt() {
    // The fake claude runs the real `rozu mcp-serve` from the generated --mcp-config: one
    // workspace write (auto-approved), a force push through rozu's shell (destructive: always
    // asks the user, here denied because stdin isn't a terminal), and a permission prompt for
    // Claude's own Bash (denied by policy: it would run outside rozu's sandbox).
    let fake = Fake::new();
    fake.scenario(
        "turn1.jsonl",
        &Path::new(CLAUDE_FIXTURES).join("text.jsonl"),
    )
    .scenario(
        "mcp1.jsonl",
        &Path::new(FIXTURES).join("mcp_write_and_force_push.jsonl"),
    );

    fake.chat()
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

    assert_eq!(
        std::fs::read_to_string(fake.path("workspace/notes.md")).unwrap(),
        "from claude\n"
    );
    let responses: Vec<Value> = std::fs::read_to_string(fake.path("log/mcp1.out"))
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let by_id = |id: u64| responses.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(by_id(0)["result"]["serverInfo"]["name"], "rozu");
    assert_eq!(by_id(1)["result"]["isError"], false);
    let push = &by_id(2)["result"];
    assert_eq!(push["isError"], true, "{push}");
    assert!(
        push["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("non-interactively"),
        "{push}"
    );
    let verdict: Value =
        serde_json::from_str(by_id(3)["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
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
    let fake = Fake::new();
    fake.scenario(
        "turn1.jsonl",
        &Path::new(CLAUDE_FIXTURES).join("not_logged_in.jsonl"),
    );
    fake.chat()
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
    let fake = Fake::new();
    fake.chat()
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
    let output = rozu().arg("models").assert().success().get_output().clone();
    let stdout = String::from_utf8(output.stdout).unwrap();
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

    let output = rozu()
        .env_remove("ROZU_APPROVAL_SOCKET")
        .args(["mcp-serve", "--workspace"])
        .arg(workspace.path())
        .write_stdin(input)
        .assert()
        .success()
        .get_output()
        .clone();
    let responses: Vec<Value> = String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("stdout is pure JSON-RPC"))
        .collect();
    assert_eq!(responses.len(), 4, "{responses:?}");
    let by_id = |id: u64| responses.iter().find(|r| r["id"] == id).unwrap();
    assert_eq!(by_id(1)["result"]["protocolVersion"], "2025-06-18");
    let tools = by_id(2)["result"]["tools"].as_array().unwrap();
    assert!(tools.iter().any(|t| t["name"] == "approve"));
    assert!(tools.iter().any(|t| t["name"] == "apply_patch"));
    let read = by_id(3)["result"]["content"][0]["text"].as_str().unwrap();
    assert!(read.contains("# hi"), "{read}");
    // Destructive with no parent to ask: denied by default, and nothing was deleted.
    let denied = &by_id(4)["result"];
    assert_eq!(denied["isError"], true);
    assert!(
        denied["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains("denied by default"),
        "{denied}"
    );
    assert!(workspace.path().join("README.md").exists());
}
