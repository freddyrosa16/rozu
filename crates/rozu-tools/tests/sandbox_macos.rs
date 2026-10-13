//! Seatbelt sandbox behavior. macOS only.
//!
//! The workspace and the "outside" canaries live under `CARGO_TARGET_TMPDIR` (inside `target/`),
//! away from both `$TMPDIR` and `/tmp`.
#![cfg(target_os = "macos")]

use std::path::Path;
use std::sync::Arc;

use rozu_tools::{
    ExecPermissions, SANDBOX_BLOCKED, SANDBOX_EXEC, Seatbelt, SessionTemp, Shell, Tool, ToolCtx,
    Workspace, resolve_lenient,
};
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

struct Setup {
    base: tempfile::TempDir,
    session: SessionTemp,
    ctx: ToolCtx,
}

fn setup() -> Setup {
    let base = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    std::fs::create_dir_all(base.path().join("ws")).unwrap();
    std::fs::create_dir_all(base.path().join("outside")).unwrap();
    let session = SessionTemp::create("sandbox-test").unwrap();
    let ctx = ToolCtx::new(
        Workspace::new(base.path().join("ws")).unwrap(),
        CancellationToken::new(),
    )
    .with_sandbox(Arc::new(Seatbelt))
    .with_temp_dir(session.path())
    .with_permissions(ExecPermissions::default());
    Setup { base, session, ctx }
}

#[tokio::test]
async fn writes_outside_the_workspace_are_blocked() {
    if !seatbelt_usable() {
        return;
    }
    let Setup { base, session, ctx } = setup();
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
    assert!(out.content.contains(SANDBOX_BLOCKED), "{}", out.content);
    assert_eq!(
        std::fs::read_to_string(ctx.workspace.root().join("inside.txt")).unwrap(),
        "inside\n"
    );
    assert!(!ctx.workspace.root().join(".rozu/approvals.json").exists());
    // $TMPDIR is the session temp dir, which is writable.
    assert!(out.content.contains("tmp-ok"), "{}", out.content);
    drop(session);
}

#[tokio::test]
async fn tmp_and_the_user_temp_dir_are_not_writable() {
    if !seatbelt_usable() {
        return;
    }
    let Setup {
        base: _base,
        session,
        ctx,
    } = setup();
    let user_tmp = std::env::temp_dir().join(format!("rozu-sbx-user-{}", std::process::id()));
    let slash_tmp = format!("/tmp/rozu-sbx-tmp-{}", std::process::id());
    let command = format!(
        "touch '{}'; touch '{slash_tmp}'; touch \"$TMPDIR/ok\" && echo session-ok",
        user_tmp.display()
    );

    let out = Shell.call(json!({ "command": command }), &ctx).await;

    assert!(!user_tmp.exists(), "{}", out.content);
    assert!(!Path::new(&slash_tmp).exists(), "{}", out.content);
    assert!(out.content.contains("session-ok"), "{}", out.content);
    assert!(session.path().join("ok").exists());
}

#[tokio::test]
async fn approved_outside_paths_are_writable_for_that_call_only() {
    if !seatbelt_usable() {
        return;
    }
    let Setup {
        base,
        session: _session,
        ctx,
    } = setup();
    let approved = base.path().join("outside/approved_canary.txt");
    let other = base.path().join("outside/other.txt");
    let command = format!(
        "echo approved > '{}'; echo nope > '{}'",
        approved.display(),
        other.display()
    );
    let scoped = ctx.clone().with_permissions(ExecPermissions {
        extra_writable: vec![resolve_lenient(&approved)],
        ..ExecPermissions::default()
    });

    let out = Shell.call(json!({ "command": command }), &scoped).await;

    assert_eq!(std::fs::read_to_string(&approved).unwrap(), "approved\n");
    assert!(!other.exists(), "{}", out.content);
    // The next call (default permissions) can't touch it.
    let out = Shell
        .call(
            json!({ "command": format!("rm '{}'", approved.display()) }),
            &ctx,
        )
        .await;
    assert!(out.is_error && approved.exists(), "{}", out.content);
}

#[tokio::test]
async fn unsandboxed_permission_really_turns_the_sandbox_off() {
    if !seatbelt_usable() {
        return;
    }
    let Setup {
        base,
        session: _session,
        ctx,
    } = setup();
    let ctx = ctx.with_permissions(ExecPermissions::unsandboxed());
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
    let Setup {
        base: _base,
        session: _session,
        ctx,
    } = setup();
    let out = Shell
        .call(
            json!({"command": "/usr/bin/curl -sS --max-time 5 -o /dev/null http://1.1.1.1"}),
            &ctx,
        )
        .await;
    assert!(out.is_error, "curl reached the network: {out:?}");
    assert!(!out.content.starts_with("exit code: 0"), "{}", out.content);
}
