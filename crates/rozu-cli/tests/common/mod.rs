#![allow(dead_code)]

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use assert_cmd::Command;
use serde_json::Value;
use wiremock::matchers::method;
use wiremock::{Mock, MockServer, Request, ResponseTemplate};

pub const OPENAI_KEY: &str = "sk-test-0123456789abcdef";

/// Env for every spawned `rozu`: logs and audit decisions stay out of the developer's home, and
/// their shell config (provider, model, a real Codex CLI) can't leak in.
pub fn base_env() -> Vec<(&'static str, Option<OsString>)> {
    let tmp = Path::new(env!("CARGO_TARGET_TMPDIR"));
    let set = |key, value: &dyn AsRef<std::ffi::OsStr>| (key, Some(value.as_ref().to_owned()));
    let mut env = vec![
        set("ROZU_LOG_FILE", &tmp.join("rozu-cli-tests.log")),
        set(
            "ROZU_AUDIT_LOG",
            &tmp.join("rozu-cli-tests-approvals.jsonl"),
        ),
        set("NO_COLOR", &"1"),
        set("ROZU_CODEX_BIN", &"/nonexistent/codex"),
    ];
    for key in [
        "ROZU_LOG_STDERR",
        "RUST_LOG",
        "ROZU_PROVIDER",
        "ROZU_MODEL",
        "ROZU_GROK_HEADLESS",
        "ROZU_GROK_HOOK_MAX_BYTES",
    ] {
        env.push((key, None));
    }
    env
}

pub fn rozu() -> Command {
    let mut cmd = Command::cargo_bin("rozu").expect("rozu binary is built");
    for (key, value) in base_env() {
        match value {
            Some(value) => cmd.env(key, value),
            None => cmd.env_remove(key),
        };
    }
    cmd
}

pub fn stdout_of(cmd: &mut Command) -> String {
    let output = cmd.assert().success().get_output().clone();
    String::from_utf8(output.stdout).expect("utf-8 output")
}

pub fn json_lines(text: &str) -> Vec<Value> {
    text.lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

pub fn read_json_lines(path: &Path) -> Vec<Value> {
    json_lines(&std::fs::read_to_string(path).unwrap_or_default())
}

pub fn sse(body: &str) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

/// A mock HTTP API answering POSTs with `responses` in order, the last one repeating.
pub struct Api {
    pub runtime: tokio::runtime::Runtime,
    pub server: MockServer,
}

impl Api {
    pub fn new(responses: Vec<ResponseTemplate>) -> Self {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let server = runtime.block_on(async {
            let server = MockServer::start().await;
            let last = responses.len() - 1;
            for (i, response) in responses.into_iter().enumerate() {
                let mock = Mock::given(method("POST")).respond_with(response);
                let mock = if i < last {
                    mock.up_to_n_times(1)
                } else {
                    mock
                };
                mock.mount(&server).await;
            }
            server
        });
        Self { runtime, server }
    }

    pub fn sse(body: &str) -> Self {
        Self::new(vec![sse(body)])
    }

    pub fn uri(&self) -> String {
        self.server.uri()
    }

    pub fn requests(&self) -> Vec<Request> {
        self.runtime
            .block_on(self.server.received_requests())
            .unwrap_or_default()
    }

    pub fn bodies(&self) -> Vec<Value> {
        self.requests()
            .iter()
            .map(|r| serde_json::from_slice(&r.body).unwrap())
            .collect()
    }
}

/// A fake provider CLI (`fake_<name>.sh` from rozu-cli-providers) in a temp dir with `log/`,
/// `turns/` and `workspace/`, and its script made of `scripts` concatenated.
#[cfg(unix)]
pub struct FakeCli {
    pub name: &'static str,
    dir: tempfile::TempDir,
}

#[cfg(unix)]
impl FakeCli {
    pub fn fixtures(name: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../rozu-cli-providers/tests/fixtures")
            .join(name)
    }

    pub fn new(name: &'static str, scripts: &[PathBuf]) -> Self {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        for sub in ["log", "turns", "workspace"] {
            std::fs::create_dir(dir.path().join(sub)).unwrap();
        }
        let bin = dir.path().join(name);
        std::fs::copy(Self::fixtures(name).join(format!("fake_{name}.sh")), &bin).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let script: String = scripts
            .iter()
            .map(|path| std::fs::read_to_string(path).unwrap())
            .collect();
        std::fs::write(dir.path().join("script.txt"), script).unwrap();
        Self { name, dir }
    }

    pub fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    pub fn rozu(&self) -> Command {
        let upper = self.name.to_uppercase();
        let mut cmd = rozu();
        cmd.env("ROZU_AUDIT_LOG", self.path("approvals.jsonl"))
            .env(format!("ROZU_{upper}_BIN"), self.path(self.name))
            .env(format!("FAKE_{upper}_SCRIPT"), self.path("script.txt"))
            .env(format!("FAKE_{upper}_TURNS"), self.path("turns"))
            .env(format!("FAKE_{upper}_LOG"), self.path("log"));
        cmd
    }

    pub fn chat(&self, provider: &str) -> Command {
        let mut cmd = self.rozu();
        cmd.args(["chat", "--provider", provider, "--workspace"])
            .arg(self.path("workspace"));
        cmd
    }

    /// What rozu wrote to the fake's stdin.
    pub fn sent(&self) -> Vec<Value> {
        read_json_lines(&self.path("log/stdin.jsonl"))
    }

    pub fn read(&self, rel: &str) -> String {
        std::fs::read_to_string(self.path(rel)).unwrap()
    }
}
