//! The shell tool kills its whole process group on timeout.
#![cfg(unix)]

use std::time::{Duration, Instant};

use rozu_tools::{Shell, Tool, ToolCtx, Workspace};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// Whether `pid` still exists (zombies count until their parent reaps them).
fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks for existence.
    unsafe { libc::kill(pid, 0) == 0 }
}

fn wait_gone(pid: i32) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if !alive(pid) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn read_pids(path: &std::path::Path) -> Vec<i32> {
    std::fs::read_to_string(path)
        .unwrap()
        .split_whitespace()
        .map(|p| p.parse().unwrap())
        .collect()
}

#[tokio::test]
async fn timeout_kills_the_process_group() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolCtx::new(
        Workspace::new(dir.path()).unwrap(),
        CancellationToken::new(),
    );
    // Two background sleepers plus a foreground one; record the background pids.
    let command = "sleep 60 & echo $! > pids; sleep 60 & echo $! >> pids; sleep 60";

    let started = Instant::now();
    let out = Shell
        .call(json!({"command": command, "timeout_secs": 1}), &ctx)
        .await;

    assert!(out.is_error, "{out:?}");
    assert!(
        out.content.contains("timed out after 1s"),
        "{}",
        out.content
    );
    assert!(started.elapsed() < Duration::from_secs(10));
    let pids = read_pids(&dir.path().join("pids"));
    assert_eq!(pids.len(), 2);
    for pid in pids {
        assert!(wait_gone(pid), "sleep {pid} survived the timeout");
    }
}

#[tokio::test]
async fn background_children_die_when_the_command_finishes() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolCtx::new(
        Workspace::new(dir.path()).unwrap(),
        CancellationToken::new(),
    );

    let started = Instant::now();
    let out = Shell
        .call(
            json!({"command": "sleep 60 > /dev/null 2>&1 & echo $! > pids; echo done"}),
            &ctx,
        )
        .await;

    assert!(!out.is_error, "{out:?}");
    assert!(out.content.contains("done"));
    assert!(started.elapsed() < Duration::from_secs(10));
    for pid in read_pids(&dir.path().join("pids")) {
        assert!(wait_gone(pid), "background sleep {pid} outlived the call");
    }
}

#[tokio::test]
async fn dropping_the_call_kills_the_group() {
    let dir = tempfile::tempdir().unwrap();
    let ctx = ToolCtx::new(
        Workspace::new(dir.path()).unwrap(),
        CancellationToken::new(),
    );
    let call = Shell.call(
        json!({"command": "sleep 60 & echo $! > pids; sleep 60"}),
        &ctx,
    );
    // The agent drops a tool future when its own timeout fires.
    let _ = tokio::time::timeout(Duration::from_millis(500), call).await;
    for pid in read_pids(&dir.path().join("pids")) {
        assert!(wait_gone(pid), "sleep {pid} survived the dropped call");
    }
}
