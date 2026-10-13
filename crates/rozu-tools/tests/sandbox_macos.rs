//! Seatbelt sandbox behavior. macOS only.
//!
//! The workspace lives under `CARGO_TARGET_TMPDIR` (inside `target/`), not `$TMPDIR`, because
//! the temp directories are writable by design.
#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::Arc;

use rozu_tools::{ExecPermissions, SANDBOX_EXEC, Seatbelt, Shell, Tool, ToolCtx, Workspace};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// `sandbox-exec` can't apply a profile when this process is already sandboxed (e.g. inside
/// another tool's sandbox). Skip there rather than fail; CI runners aren't sandboxed.
fn seatbelt_usable() -> bool {
    let ok = std::process::Command::new(SANDBOX_EXEC)
        .args(["-p", "(version 1)(allow default)", "/usr/bin/true"])
        .status()
        .is_ok_and(|s| s.success());
    if !ok {
        eprintln!("skipping: sandbox-exec is unavailable or can't nest here");
    }
    ok
}

fn setup() -> (tempfile::TempDir, ToolCtx) {
    let base = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::fs::create_dir_all(base.path().join("ws")).unwrap();
    std::fs::create_dir_all(base.path().join("outside")).unwrap();
    let ctx = ToolCtx::new(
        Workspace::new(base.path().join("ws")).unwrap(),
        CancellationToken::new(),
    )
    .with_sandbox(Arc::new(Seatbelt))
    .with_permissions(ExecPermissions {
        sandbox: true,
        network: false,
    });
    (base, ctx)
}

#[tokio::test]
async fn writes_outside_the_workspace_are_blocked() {
    if !seatbelt_usable() {
        return;
    }
    let (base, ctx) = setup();
    let outside = base.path().join("outside/pwned.txt");
    let command = format!(
        "echo inside > inside.txt; echo pwned > '{}'; mkdir -p .rozu; echo rule > .rozu/approvals.json; echo tmp > \"$TMPDIR/rozu-sandbox-test-$$\" && rm \"$TMPDIR/rozu-sandbox-test-$$\" && echo tmp-ok",
        outside.display()
    );

    let out = Shell.call(json!({"command": command}), &ctx).await;

    assert!(!outside.exists(), "sandbox let a write escape: {out:?}");
    assert!(
        out.content.contains("Operation not permitted"),
        "{}",
        out.content
    );
    assert_eq!(
        std::fs::read_to_string(ctx.workspace.root().join("inside.txt")).unwrap(),
        "inside\n"
    );
    assert!(!ctx.workspace.root().join(".rozu/approvals.json").exists());
    assert!(out.content.contains("tmp-ok"), "{}", out.content);
}

#[tokio::test]
async fn unsandboxed_permission_really_turns_the_sandbox_off() {
    if !seatbelt_usable() {
        return;
    }
    let (base, ctx) = setup();
    let ctx = ctx.with_permissions(ExecPermissions {
        sandbox: false,
        network: true,
    });
    let outside = base.path().join("outside/allowed.txt");
    let out = Shell
        .call(
            json!({"command": format!("echo ok > '{}'", outside.display())}),
            &ctx,
        )
        .await;
    assert!(!out.is_error, "{out:?}");
    assert!(outside.exists());
}

#[tokio::test]
async fn network_is_denied_without_approval() {
    if !seatbelt_usable() {
        return;
    }
    if !Path::new("/usr/bin/curl").exists() {
        eprintln!("skipping: /usr/bin/curl not found");
        return;
    }
    let (_base, ctx) = setup();
    let out = Shell
        .call(
            json!({"command": "/usr/bin/curl -sS --max-time 5 -o /dev/null http://1.1.1.1"}),
            &ctx,
        )
        .await;
    assert!(out.is_error, "curl reached the network: {out:?}");
    assert!(!out.content.starts_with("exit code: 0"), "{}", out.content);
}
