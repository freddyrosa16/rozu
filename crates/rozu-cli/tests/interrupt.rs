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
