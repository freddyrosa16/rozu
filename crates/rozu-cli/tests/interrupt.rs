//! Ctrl-C against the real `rozu` binary: a fast double press during a turn, and a press at the
//! prompt, must exit promptly with status 130 (QA: a double Ctrl-C under 300 ms used to hang).
#![cfg(unix)]

use std::io::Write;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use wiremock::matchers::method;
use wiremock::{Mock, MockServer, ResponseTemplate};

/// A mock OpenAI server whose responses take a minute, so a turn stays in flight.
fn stalled_server(runtime: &tokio::runtime::Runtime) -> MockServer {
    runtime.block_on(async {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(200).set_delay(Duration::from_secs(60)))
            .mount(&server)
            .await;
        server
    })
}

/// Interactive `rozu chat` (stdin piped and kept open) against `server`.
fn spawn_chat(server: &MockServer, workspace: &std::path::Path) -> Child {
    Command::new(assert_cmd::cargo::cargo_bin("rozu"))
        .env("OPENAI_API_KEY", "sk-test-0123456789abcdef")
        .env("OPENAI_BASE_URL", server.uri())
        .env("NO_COLOR", "1")
        .env(
            "ROZU_LOG_FILE",
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("rozu-cli-tests.log"),
        )
        .env(
            "ROZU_AUDIT_LOG",
            std::path::Path::new(env!("CARGO_TARGET_TMPDIR"))
                .join("rozu-cli-tests-approvals.jsonl"),
        )
        .env_remove("ROZU_LOG_STDERR")
        .env_remove("ROZU_PROVIDER")
        .env_remove("ROZU_MODEL")
        .arg("chat")
        .arg("--workspace")
        .arg(workspace)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn rozu")
}

fn sigint(child: &Child) {
    let pid = libc::pid_t::try_from(child.id()).unwrap();
    // SAFETY: plain kill(2) on our own child.
    assert_eq!(unsafe { libc::kill(pid, libc::SIGINT) }, 0);
}

/// Waits up to `limit` for `child` to exit; kills it and panics if it doesn't.
fn wait_exit(child: &mut Child, limit: Duration) -> std::process::ExitStatus {
    let start = Instant::now();
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if start.elapsed() > limit {
            let _ = child.kill();
            let _ = child.wait();
            panic!("rozu did not exit within {limit:?} after Ctrl-C");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Waits until `server` has received a request, i.e. a turn is in flight.
fn wait_for_request(runtime: &tokio::runtime::Runtime, server: &MockServer) {
    let start = Instant::now();
    while runtime
        .block_on(server.received_requests())
        .unwrap_or_default()
        .is_empty()
    {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "the turn never started"
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn fast_double_ctrl_c_during_a_turn_exits() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    // Repeat: the bug hung about two times out of three. No 0 ms gap: two back-to-back kill(2)s
    // can reach the process as one pending SIGINT (the kernel doesn't queue standard signals),
    // which is a single press by definition; human double presses are tens of ms apart.
    for gap_ms in [1, 5, 20, 50, 100, 250, 1, 5] {
        let server = stalled_server(&runtime);
        let dir = tempfile::tempdir().unwrap();
        let mut child = spawn_chat(&server, dir.path());
        let mut stdin = child.stdin.take().unwrap();
        writeln!(stdin, "hello").unwrap();
        wait_for_request(&runtime, &server);

        sigint(&child);
        std::thread::sleep(Duration::from_millis(gap_ms));
        sigint(&child);

        let status = wait_exit(&mut child, Duration::from_secs(2));
        assert_eq!(status.code(), Some(130), "gap {gap_ms} ms");
        drop(stdin);
    }
}

#[test]
fn single_ctrl_c_cancels_the_turn_and_keeps_the_session() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = stalled_server(&runtime);
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_chat(&server, dir.path());
    let mut stdin = child.stdin.take().unwrap();
    writeln!(stdin, "hello").unwrap();
    wait_for_request(&runtime, &server);

    sigint(&child);
    // Still running at the prompt after the cancel...
    std::thread::sleep(Duration::from_millis(500));
    assert!(child.try_wait().unwrap().is_none(), "exited on one Ctrl-C");
    // ...and Ctrl-C at the prompt exits.
    sigint(&child);
    let status = wait_exit(&mut child, Duration::from_secs(2));
    assert_eq!(status.code(), Some(130));

    let mut stderr = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut stderr).unwrap();
    assert!(stderr.contains("cancelling"), "{stderr}");
    drop(stdin);
}

/// `rozu chat` on a real terminal (a PTY), so Ctrl-C arrives as a `^C` byte, not a signal.
mod pty {
    use std::io::Read;
    use std::sync::{Arc, Mutex};

    use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};

    use super::*;

    pub struct Session {
        pub child: Box<dyn Child + Send + Sync>,
        pub writer: Box<dyn Write + Send>,
        pub output: Arc<Mutex<Vec<u8>>>,
        _master: Box<dyn MasterPty + Send>,
    }

    impl Session {
        pub fn spawn(server: &MockServer, workspace: &std::path::Path) -> Self {
            let pair = native_pty_system()
                .openpty(PtySize {
                    rows: 24,
                    cols: 120,
                    pixel_width: 0,
                    pixel_height: 0,
                })
                .unwrap();
            let mut cmd = CommandBuilder::new(assert_cmd::cargo::cargo_bin("rozu"));
            cmd.args(["chat", "--workspace"]);
            cmd.arg(workspace);
            cmd.cwd(workspace);
            cmd.env("OPENAI_API_KEY", "sk-test-0123456789abcdef");
            cmd.env("OPENAI_BASE_URL", server.uri());
            cmd.env("NO_COLOR", "1");
            let tmp = std::path::Path::new(env!("CARGO_TARGET_TMPDIR"));
            cmd.env("ROZU_LOG_FILE", tmp.join("rozu-cli-tests.log"));
            cmd.env("ROZU_AUDIT_LOG", tmp.join("rozu-cli-tests-approvals.jsonl"));
            for var in ["ROZU_LOG_STDERR", "ROZU_PROVIDER", "ROZU_MODEL", "RUST_LOG"] {
                cmd.env_remove(var);
            }
            let child = pair.slave.spawn_command(cmd).unwrap();
            drop(pair.slave);
            // Drain the terminal so rozu never blocks writing to it.
            let output = Arc::new(Mutex::new(Vec::new()));
            let mut reader = pair.master.try_clone_reader().unwrap();
            let sink = output.clone();
            std::thread::spawn(move || {
                let mut buf = [0u8; 4096];
                while let Ok(n) = reader.read(&mut buf) {
                    if n == 0 {
                        break;
                    }
                    sink.lock().unwrap().extend_from_slice(&buf[..n]);
                }
            });
            let writer = pair.master.take_writer().unwrap();
            Self {
                child,
                writer,
                output,
                _master: pair.master,
            }
        }

        pub fn write(&mut self, bytes: &[u8]) {
            // One write call: both bytes reach the terminal together.
            self.writer.write_all(bytes).unwrap();
            self.writer.flush().unwrap();
        }

        pub fn output(&self) -> String {
            String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
        }

        /// Waits up to `limit` for the exit code.
        pub fn wait_exit(&mut self, limit: Duration) -> u32 {
            let start = Instant::now();
            loop {
                if let Some(status) = self.child.try_wait().unwrap() {
                    return status.exit_code();
                }
                if start.elapsed() > limit {
                    let _ = self.child.kill();
                    panic!(
                        "rozu did not exit within {limit:?}; terminal output:\n{}",
                        self.output()
                    );
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        pub fn running(&mut self) -> bool {
            self.child.try_wait().unwrap().is_none()
        }
    }
}

/// BUG-1: two `^C` bytes in one write during a turn are two presses, so rozu exits. (They used
/// to merge into one SIGINT 19 times out of 20.)
#[test]
fn double_ctrl_c_in_one_write_on_a_terminal_exits() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    for trial in 0..3 {
        let server = stalled_server(&runtime);
        let dir = tempfile::tempdir().unwrap();
        let mut session = pty::Session::spawn(&server, dir.path());
        std::thread::sleep(Duration::from_millis(300));
        session.write(b"hello\r");
        wait_for_request(&runtime, &server);

        session.write(b"\x03\x03");

        assert_eq!(
            session.wait_exit(Duration::from_secs(3)),
            130,
            "trial {trial}: {}",
            session.output()
        );
    }
}

/// One `^C` byte cancels the turn and keeps the session; the terminal is back to normal at the
/// prompt, where Ctrl-C is a signal again and exits.
#[test]
fn single_ctrl_c_on_a_terminal_cancels_then_exits_at_the_prompt() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = stalled_server(&runtime);
    let dir = tempfile::tempdir().unwrap();
    let mut session = pty::Session::spawn(&server, dir.path());
    std::thread::sleep(Duration::from_millis(300));
    session.write(b"hello\r");
    wait_for_request(&runtime, &server);

    session.write(b"\x03");
    let start = Instant::now();
    while !session.output().contains("cancelling") {
        assert!(
            start.elapsed() < Duration::from_secs(5),
            "{}",
            session.output()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    std::thread::sleep(Duration::from_millis(300));
    assert!(
        session.running(),
        "exited on one Ctrl-C: {}",
        session.output()
    );

    session.write(b"\x03");
    assert_eq!(session.wait_exit(Duration::from_secs(3)), 130);
}

#[test]
fn ctrl_c_at_the_prompt_exits() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let server = stalled_server(&runtime);
    let dir = tempfile::tempdir().unwrap();
    let mut child = spawn_chat(&server, dir.path());
    let stdin = child.stdin.take().unwrap();
    // Give it time to install the handler and wait at the prompt.
    std::thread::sleep(Duration::from_secs(1));

    sigint(&child);
    let status = wait_exit(&mut child, Duration::from_secs(2));
    // Killed by the default SIGINT action would be `None`: the handler must be what exits.
    assert_eq!(status.code(), Some(130));
    drop(stdin);
}
