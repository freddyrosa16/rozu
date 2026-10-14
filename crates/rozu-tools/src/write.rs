//! Workspace write tools: `write_file` and `apply_patch`.
//!
//! Both resolve their path with [`Workspace::resolve_for_write`] and refuse anything outside the
//! workspace (or inside `.rozu/`) before approval is even asked, so these tools can never write
//! outside the workspace regardless of the approval mode.

use std::fmt::Write as _;
use std::io;
use std::path::PathBuf;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::patch::{
    MAX_PATCH_BYTES, MAX_PATCH_EDITS, SearchReplace, apply_search_replace, apply_unified_diff,
};
use crate::tool::{parse_args, schema_for};
use crate::workspace_file::WorkspaceFile;
use crate::{PathError, Risk, Tool, ToolCtx, ToolOutput, Workspace};

/// Resolves a write target, rejecting paths outside the workspace and in `.rozu/`.
fn write_target(workspace: &Workspace, path: &str) -> Result<PathBuf, ToolOutput> {
    let resolved = workspace
        .resolve_for_write(path)
        .map_err(|err| ToolOutput::error(err.to_string()))?;
    if workspace.is_protected(std::path::Path::new(path)) || workspace.is_protected(&resolved) {
        return Err(ToolOutput::error(format!(
            "{path} is in rozu's settings directory (.rozu/), which tools may not modify"
        )));
    }
    match std::fs::symlink_metadata(&resolved) {
        Ok(meta) if !meta.is_file() => {
            return Err(ToolOutput::error(format!("{path} is not a regular file")));
        }
        Err(err) if err.kind() != io::ErrorKind::NotFound => {
            return Err(ToolOutput::error(format!("cannot access {path}: {err}")));
        }
        _ => {}
    }
    Ok(resolved)
}

fn write_risk(workspace: &Workspace, path: Option<&str>) -> Risk {
    match path.map(|p| workspace.resolve_for_write(p)) {
        Some(Err(PathError::Outside(_))) => Risk::WriteOutside,
        _ => Risk::WriteWorkspace,
    }
}

fn path_arg(args: &Value) -> Option<&str> {
    args.get("path").and_then(Value::as_str)
}

async fn write_atomically(target: WorkspaceFile, contents: String) -> io::Result<bool> {
    tokio::task::spawn_blocking(move || target.replace(&contents))
        .await
        .map_err(io::Error::other)?
}

async fn read_patch_file(target: WorkspaceFile) -> io::Result<(WorkspaceFile, Option<String>)> {
    tokio::task::spawn_blocking(move || {
        let original = target.read()?;
        Ok((target, original))
    })
    .await
    .map_err(io::Error::other)?
}

// ---------------------------------------------------------------------------------------------
// write_file

/// Creates or overwrites a file in the workspace.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteFile;

#[derive(Debug, Deserialize, JsonSchema)]
struct WriteFileArgs {
    /// File path, relative to the workspace root. Missing parent directories are created.
    path: String,
    /// The complete new contents of the file.
    content: String,
}

#[async_trait]
impl Tool for WriteFile {
    fn name(&self) -> &'static str {
        "write_file"
    }

    fn description(&self) -> &'static str {
        "Create a file, or overwrite it, with the given contents. Use for new files or full \
         rewrites; prefer apply_patch for small changes to existing files. The path must be \
         inside the workspace."
    }

    fn schema(&self) -> Value {
        schema_for::<WriteFileArgs>()
    }

    fn risk(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        write_risk(&ctx.workspace, path_arg(args))
    }

    fn validate(&self, args: &Value, ctx: &ToolCtx) -> Result<(), ToolOutput> {
        check_write_size(args)?;
        let args: WriteFileArgs = parse_args(args.clone())?;
        write_target(&ctx.workspace, &args.path).map(|_| ())
    }

    fn preview(&self, args: &Value) -> String {
        let path = path_arg(args).unwrap_or("?");
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        format!("write {path} ({} bytes):\n{}", content.len(), content)
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput {
        if let Err(output) = check_write_size(&args) {
            return output;
        }
        let args: WriteFileArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let path = match write_target(&ctx.workspace, &args.path) {
            Ok(path) => path,
            Err(output) => return output,
        };
        let target = match WorkspaceFile::open(&ctx.workspace, &path) {
            Ok(target) => target,
            Err(err) => return ToolOutput::error(format!("cannot access {}: {err}", args.path)),
        };
        let bytes = args.content.len();
        match write_atomically(target, args.content).await {
            Ok(existed) => ToolOutput::ok(format!(
                "{} {} ({} bytes)",
                if existed { "overwrote" } else { "created" },
                ctx.workspace.display_relative(&path),
                bytes
            )),
            Err(err) => ToolOutput::error(format!("could not write {}: {err}", args.path)),
        }
    }
}

fn check_write_size(args: &Value) -> Result<(), ToolOutput> {
    if args
        .get("content")
        .and_then(Value::as_str)
        .is_some_and(|text| text.len() > MAX_PATCH_BYTES)
    {
        return Err(ToolOutput::error("file contents exceed the 1 MiB limit"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------
// apply_patch

/// Edits one existing file with exact search/replace edits or a unified diff.
#[derive(Debug, Clone, Copy, Default)]
pub struct ApplyPatch;

#[derive(Debug, Deserialize, JsonSchema)]
struct ApplyPatchArgs {
    /// File to edit, relative to the workspace root.
    path: String,
    /// Search/replace edits, applied in order. Each `old` must match the file exactly
    /// (whitespace included) and exactly once, unless `replace_all` is set. Use either `edits`
    /// or `diff`, not both.
    #[serde(default)]
    edits: Vec<EditArg>,
    /// A unified diff for this one file: `@@ -start,count +start,count @@` hunks with ' '
    /// context, '-' removed and '+' added lines. `---`/`+++` headers are optional.
    #[serde(default)]
    diff: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct EditArg {
    /// Exact text to find.
    old: String,
    /// Replacement text.
    new: String,
    /// Replace every occurrence instead of requiring a unique match.
    #[serde(default)]
    replace_all: bool,
}

impl ApplyPatchArgs {
    fn check(&self) -> Result<(), ToolOutput> {
        if self.edits.len() > MAX_PATCH_EDITS {
            return Err(ToolOutput::error("too many patch edits (limit 128)"));
        }
        let bytes =
            self.edits
                .iter()
                .try_fold(self.diff.as_ref().map_or(0, String::len), |total, edit| {
                    total
                        .checked_add(edit.old.len())?
                        .checked_add(edit.new.len())
                });
        if bytes.is_none_or(|bytes| bytes > MAX_PATCH_BYTES) {
            return Err(ToolOutput::error("patch arguments exceed the 1 MiB limit"));
        }
        match (self.edits.is_empty(), &self.diff) {
            (true, None) => Err(ToolOutput::error(
                "nothing to apply: pass `edits` (search/replace) or `diff` (unified diff)",
            )),
            (false, Some(_)) => Err(ToolOutput::error("pass either `edits` or `diff`, not both")),
            _ => Ok(()),
        }
    }
}

#[async_trait]
impl Tool for ApplyPatch {
    fn name(&self) -> &'static str {
        "apply_patch"
    }

    fn description(&self) -> &'static str {
        "Edit a file in the workspace. Either pass `edits`, a list of exact search/replace pairs \
         (each `old` must appear exactly once; include enough surrounding lines to be unique), \
         or `diff`, a unified diff for this one file. All edits apply or none do; on a mismatch \
         re-read the file and retry. A diff starting at `@@ -0,0` can create a new file."
    }

    fn schema(&self) -> Value {
        schema_for::<ApplyPatchArgs>()
    }

    fn risk(&self, args: &Value, ctx: &ToolCtx) -> Risk {
        write_risk(&ctx.workspace, path_arg(args))
    }

    fn validate(&self, args: &Value, ctx: &ToolCtx) -> Result<(), ToolOutput> {
        let args: ApplyPatchArgs = parse_args(args.clone())?;
        args.check()?;
        write_target(&ctx.workspace, &args.path).map(|_| ())
    }

    fn preview(&self, args: &Value) -> String {
        let path = path_arg(args).unwrap_or("?");
        let mut out = format!("patch {path}:\n");
        if let Some(diff) = args.get("diff").and_then(Value::as_str) {
            out.push_str(diff);
        } else if let Some(edits) = args.get("edits").and_then(Value::as_array) {
            for (i, edit) in edits.iter().enumerate() {
                let field = |name| edit.get(name).and_then(Value::as_str).unwrap_or("");
                let _ = writeln!(out, "@@ edit {} @@", i + 1);
                for line in field("old").lines() {
                    let _ = writeln!(out, "-{line}");
                }
                for line in field("new").lines() {
                    let _ = writeln!(out, "+{line}");
                }
            }
        }
        out
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput {
        let args: ApplyPatchArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return output,
        };
        if let Err(output) = args.check() {
            return output;
        }
        let path = match write_target(&ctx.workspace, &args.path) {
            Ok(path) => path,
            Err(output) => return output,
        };
        let target = match WorkspaceFile::open(&ctx.workspace, &path) {
            Ok(target) => target,
            Err(err) => return ToolOutput::error(format!("cannot access {}: {err}", args.path)),
        };
        let (target, original) = match read_patch_file(target).await {
            Ok(result) => result,
            Err(err) => return ToolOutput::error(format!("cannot read {}: {err}", args.path)),
        };

        let result = match (&args.diff, &original) {
            (Some(diff), original) => apply_unified_diff(original.as_deref().unwrap_or(""), diff),
            (None, Some(original)) => {
                let edits: Vec<SearchReplace> = args
                    .edits
                    .into_iter()
                    .map(|e| SearchReplace {
                        old: e.old,
                        new: e.new,
                        replace_all: e.replace_all,
                    })
                    .collect();
                apply_search_replace(original, &edits)
            }
            (None, None) => {
                return ToolOutput::error(format!(
                    "{} does not exist; use write_file to create it",
                    args.path
                ));
            }
        };
        let updated = match result {
            Ok(text) => text,
            Err(err) => {
                return ToolOutput::error(format!("{}: {err}. Nothing was changed.", args.path));
            }
        };
        if original.is_none() && updated.is_empty() {
            return ToolOutput::error(format!(
                "{} does not exist and the diff adds nothing",
                args.path
            ));
        }
        if original.as_deref() == Some(updated.as_str()) {
            return ToolOutput::ok(format!("{}: no changes", args.path));
        }
        let updated_lines = updated.lines().count();
        match write_atomically(target, updated).await {
            Ok(_) => ToolOutput::ok(format!(
                "patched {} ({} → {} lines)",
                ctx.workspace.display_relative(&path),
                original.as_deref().map_or(0, |t| t.lines().count()),
                updated_lines
            )),
            Err(err) => ToolOutput::error(format!("could not write {}: {err}", args.path)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn setup() -> (tempfile::TempDir, ToolCtx) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(
            root.join("src/lib.rs"),
            "fn add(a: i32, b: i32) -> i32 {\n    a - b\n}\n",
        )
        .unwrap();
        let ctx = ToolCtx::new(Workspace::new(&root).unwrap(), CancellationToken::new());
        (dir, ctx)
    }

    fn read(ctx: &ToolCtx, path: &str) -> String {
        fs::read_to_string(ctx.workspace.root().join(path)).unwrap()
    }

    #[tokio::test]
    async fn write_file_creates_and_overwrites() {
        let (_dir, ctx) = setup();
        let out = WriteFile
            .call(json!({"path": "docs/new.md", "content": "hi\n"}), &ctx)
            .await;
        assert!(!out.is_error, "{out:?}");
        assert!(out.content.starts_with("created"), "{out:?}");
        assert_eq!(read(&ctx, "docs/new.md"), "hi\n");

        let out = WriteFile
            .call(json!({"path": "docs/new.md", "content": "bye\n"}), &ctx)
            .await;
        assert!(out.content.starts_with("overwrote"), "{out:?}");
        assert_eq!(read(&ctx, "docs/new.md"), "bye\n");
        // No temp files left behind.
        let entries: Vec<_> = fs::read_dir(ctx.workspace.root().join("docs"))
            .unwrap()
            .collect();
        assert_eq!(entries.len(), 1);
    }

    #[tokio::test]
    async fn write_tools_reject_outside_and_protected_paths() {
        let (dir, ctx) = setup();
        for path in [
            "../escape.txt",
            "/etc/rozu-test",
            ".rozu/approvals.json",
            ".ROZU/x",
        ] {
            let args = json!({"path": path, "content": "x"});
            assert!(WriteFile.validate(&args, &ctx).is_err(), "{path}");
            let out = WriteFile.call(args, &ctx).await;
            assert!(out.is_error, "{path}: {out:?}");
        }
        assert!(!dir.path().join("escape.txt").exists());
        assert_eq!(
            WriteFile.risk(&json!({"path": "../escape.txt"}), &ctx),
            Risk::WriteOutside
        );
        assert_eq!(
            WriteFile.risk(&json!({"path": "src/x.rs"}), &ctx),
            Risk::WriteWorkspace
        );
        let patch = json!({"path": "../x", "edits": [{"old": "a", "new": "b"}]});
        assert!(ApplyPatch.validate(&patch, &ctx).is_err());
        assert_eq!(ApplyPatch.risk(&patch, &ctx), Risk::WriteOutside);
    }

    #[tokio::test]
    async fn apply_patch_edits_succeed() {
        let (_dir, ctx) = setup();
        let out = ApplyPatch
            .call(
                json!({"path": "src/lib.rs", "edits": [{"old": "    a - b", "new": "    a + b"}]}),
                &ctx,
            )
            .await;
        assert!(!out.is_error, "{out:?}");
        assert_eq!(
            read(&ctx, "src/lib.rs"),
            "fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n"
        );
    }

    #[tokio::test]
    async fn apply_patch_diff_succeeds() {
        let (_dir, ctx) = setup();
        let diff = "--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1,3 +1,3 @@\n fn add(a: i32, b: i32) -> i32 {\n-    a - b\n+    a + b\n }\n";
        let out = ApplyPatch
            .call(json!({"path": "src/lib.rs", "diff": diff}), &ctx)
            .await;
        assert!(!out.is_error, "{out:?}");
        assert!(read(&ctx, "src/lib.rs").contains("a + b"));
    }

    #[tokio::test]
    async fn apply_patch_mismatch_changes_nothing() {
        let (_dir, ctx) = setup();
        let before = read(&ctx, "src/lib.rs");
        let out = ApplyPatch
            .call(
                json!({"path": "src/lib.rs", "edits": [
                    {"old": "    a - b", "new": "    a + b"},
                    {"old": "fn sub", "new": "fn subtract"},
                ]}),
                &ctx,
            )
            .await;
        assert!(out.is_error);
        assert!(out.content.contains("edit 2"), "{}", out.content);
        assert!(
            out.content.contains("Nothing was changed"),
            "{}",
            out.content
        );
        assert_eq!(read(&ctx, "src/lib.rs"), before);

        let diff = "@@ -1,2 +1,2 @@\n fn add(a: i32, b: i32) -> i32 {\n-    a * b\n+    a + b\n";
        let out = ApplyPatch
            .call(json!({"path": "src/lib.rs", "diff": diff}), &ctx)
            .await;
        assert!(out.is_error);
        assert!(
            out.content
                .contains("expected `    a * b`, found `    a - b`"),
            "{}",
            out.content
        );
        assert_eq!(read(&ctx, "src/lib.rs"), before);
    }

    #[tokio::test]
    async fn apply_patch_argument_errors() {
        let (_dir, ctx) = setup();
        for (args, needle) in [
            (json!({"path": "src/lib.rs"}), "nothing to apply"),
            (
                json!({"path": "src/lib.rs", "diff": "@@ -1 +1 @@\n", "edits": [{"old": "a", "new": "b"}]}),
                "not both",
            ),
            (
                json!({"path": "missing.rs", "edits": [{"old": "a", "new": "b"}]}),
                "use write_file",
            ),
        ] {
            let out = ApplyPatch.call(args.clone(), &ctx).await;
            assert!(
                out.is_error && out.content.contains(needle),
                "{args}: {out:?}"
            );
        }
    }

    #[test]
    fn previews_show_the_change() {
        let preview =
            ApplyPatch.preview(&json!({"path": "a.rs", "edits": [{"old": "x\ny", "new": "z"}]}));
        assert_eq!(preview, "patch a.rs:\n@@ edit 1 @@\n-x\n-y\n+z\n");
        let preview = WriteFile.preview(&json!({"path": "a.txt", "content": "hello"}));
        assert_eq!(preview, "write a.txt (5 bytes):\nhello");
    }

    #[test]
    fn previews_include_the_middle_of_large_allowed_changes() {
        let text = format!("{}VISIBLE MIDDLE{}", "a".repeat(8192), "z".repeat(8192));
        assert_eq!(
            WriteFile.preview(&json!({"path": "a.txt", "content": text})),
            format!("write a.txt ({} bytes):\n{text}", text.len())
        );
        assert_eq!(
            ApplyPatch.preview(&json!({"path": "a.txt", "diff": text})),
            format!("patch a.txt:\n{text}")
        );
        assert_eq!(
            ApplyPatch.preview(&json!({"path": "a.txt", "edits": [{"old": "old", "new": text}]})),
            format!("patch a.txt:\n@@ edit 1 @@\n-old\n+{text}\n")
        );
    }

    #[cfg(unix)]
    #[test]
    fn queued_write_and_patch_stay_in_the_opened_parent_after_a_swap() {
        use std::task::Poll;

        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            for patch in [false, true] {
                for protected in [false, true] {
                    let (dir, ctx) = setup();
                    let root = ctx.workspace.root();
                    fs::write(root.join("src/file.txt"), "before").unwrap();
                    let redirected = if protected { root.join(".rozu") } else { dir.path().join("outside") };
                    fs::create_dir(&redirected).unwrap();
                    fs::write(redirected.join("file.txt"), "keep private").unwrap();
                    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
                    let (release_tx, release_rx) = std::sync::mpsc::channel();
                    let blocker = tokio::task::spawn_blocking(move || {
                        started_tx.send(()).unwrap();
                        release_rx.recv().unwrap();
                    });
                    started_rx.await.unwrap();

                    let tool: &dyn Tool = if patch { &ApplyPatch } else { &WriteFile };
                    let args = if patch {
                        json!({"path": "src/file.txt", "edits": [{"old": "before", "new": "after"}]})
                    } else {
                        json!({"path": "src/file.txt", "content": "after"})
                    };
                    assert!(tool.validate(&args, &ctx).is_ok());
                    let mut call = tool.call(args, &ctx);
                    // One poll captures the directory and queues I/O behind the blocked worker.
                    std::future::poll_fn(|cx| {
                        assert!(call.as_mut().poll(cx).is_pending());
                        Poll::Ready(())
                    }).await;
                    fs::rename(root.join("src"), root.join("original-src")).unwrap();
                    std::os::unix::fs::symlink(&redirected, root.join("src")).unwrap();
                    release_tx.send(()).unwrap();
                    let result = call.await;
                    blocker.await.unwrap();
                    assert!(!result.is_error, "{result:?}");
                    assert_eq!(read(&ctx, "original-src/file.txt"), "after");
                    assert_eq!(fs::read_to_string(redirected.join("file.txt")).unwrap(), "keep private");
                    assert_eq!(fs::read_dir(root.join("original-src")).unwrap().count(), 2);
                }
            }
        });
    }

    #[cfg(unix)]
    #[test]
    fn patch_keeps_its_parent_between_read_and_replace_and_rejects_prior_swaps() {
        for protected in [false, true] {
            let (dir, ctx) = setup();
            let root = ctx.workspace.root();
            fs::write(root.join("src/file.txt"), "before").unwrap();
            let path = write_target(&ctx.workspace, "src/file.txt").unwrap();
            let target = WorkspaceFile::open(&ctx.workspace, &path).unwrap();
            assert_eq!(target.read().unwrap().as_deref(), Some("before"));
            let redirected = if protected {
                root.join(".rozu")
            } else {
                dir.path().join("outside")
            };
            fs::create_dir(&redirected).unwrap();
            fs::write(redirected.join("file.txt"), "keep private").unwrap();
            fs::rename(root.join("src"), root.join("original-src")).unwrap();
            std::os::unix::fs::symlink(&redirected, root.join("src")).unwrap();
            assert!(WorkspaceFile::open(&ctx.workspace, &path).is_err());
            target.replace("after").unwrap();
            assert_eq!(read(&ctx, "original-src/file.txt"), "after");
            assert_eq!(
                fs::read_to_string(redirected.join("file.txt")).unwrap(),
                "keep private"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn settings_alias_created_after_opening_a_target_is_rejected_at_use() {
        let (_dir, ctx) = setup();
        let path = write_target(&ctx.workspace, "src/lib.rs").unwrap();
        let target = WorkspaceFile::open(&ctx.workspace, &path).unwrap();
        let before = read(&ctx, "src/lib.rs");
        std::os::unix::fs::symlink("src", ctx.workspace.root().join(".rozu")).unwrap();
        assert!(target.read().is_err());
        assert!(target.replace("bad").is_err());
        assert_eq!(read(&ctx, "src/lib.rs"), before);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_preserves_permissions_without_requiring_read_access() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, ctx) = setup();
        let path = ctx.workspace.root().join("write-only");
        fs::write(&path, "before").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o200)).unwrap();
        let result = WriteFile
            .call(json!({"path": "write-only", "content": "after"}), &ctx)
            .await;
        assert!(!result.is_error, "{result:?}");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o200
        );
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(read(&ctx, "write-only"), "after");
    }

    #[cfg(unix)]
    #[test]
    fn workspace_root_handle_survives_root_path_replacement() {
        let (dir, ctx) = setup();
        let root = ctx.workspace.root();
        let path = write_target(&ctx.workspace, "src/lib.rs").unwrap();
        let target = WorkspaceFile::open(&ctx.workspace, &path).unwrap();
        let original = target.read().unwrap().unwrap();
        let original_root = dir.path().join("original-root");
        fs::rename(root, &original_root).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir_all(outside.join("src")).unwrap();
        fs::write(outside.join("src/lib.rs"), "keep private").unwrap();
        std::os::unix::fs::symlink(&outside, root).unwrap();
        // Even a fresh target uses the root descriptor captured when the workspace opened.
        let fresh = WorkspaceFile::open(&ctx.workspace, &path).unwrap();
        assert_eq!(fresh.read().unwrap().as_deref(), Some(original.as_str()));
        target.replace("after").unwrap();
        assert_eq!(
            fs::read_to_string(original_root.join("src/lib.rs")).unwrap(),
            "after"
        );
        assert_eq!(
            fs::read_to_string(outside.join("src/lib.rs")).unwrap(),
            "keep private"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn planted_temporary_symlinks_never_receive_write_or_patch_contents() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        for patch in [false, true] {
            for protected in [false, true] {
                let (dir, ctx) = setup();
                let root = ctx.workspace.root();
                let target = root.join("script.sh");
                fs::write(&target, "before").unwrap();
                fs::set_permissions(&target, fs::Permissions::from_mode(0o751)).unwrap();
                let canary = if protected {
                    fs::create_dir(root.join(".rozu")).unwrap();
                    root.join(".rozu/approvals.json")
                } else {
                    dir.path().join("outside.txt")
                };
                fs::write(&canary, "keep").unwrap();
                let planted = root.join(".script.sh.rozu-tmp");
                symlink(&canary, &planted).unwrap();
                let result = if patch {
                    ApplyPatch.call(json!({"path": "script.sh", "edits": [{"old": "before", "new": "after"}]}), &ctx).await
                } else {
                    WriteFile
                        .call(json!({"path": "script.sh", "content": "after"}), &ctx)
                        .await
                };
                assert!(!result.is_error, "{result:?}");
                assert_eq!(fs::read_to_string(&target).unwrap(), "after");
                assert_eq!(
                    fs::metadata(&target).unwrap().permissions().mode() & 0o777,
                    0o751
                );
                assert_eq!(fs::read_to_string(&canary).unwrap(), "keep");
                assert!(fs::symlink_metadata(&planted).unwrap().is_symlink());
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn writes_and_patches_reject_protected_directory_aliases() {
        let (_dir, ctx) = setup();
        let root = ctx.workspace.root();
        fs::create_dir(root.join("settings")).unwrap();
        fs::write(root.join("settings/pinned.md"), "keep").unwrap();
        std::os::unix::fs::symlink("settings", root.join(".rozu")).unwrap();
        std::os::unix::fs::symlink("settings", root.join("alias")).unwrap();
        for path in [
            ".rozu/pinned.md",
            "settings/pinned.md",
            "alias/pinned.md",
            "settings/new.json",
        ] {
            let write = WriteFile
                .call(json!({"path": path, "content": "bad"}), &ctx)
                .await;
            let patch = ApplyPatch
                .call(
                    json!({"path": path, "edits": [{"old": "keep", "new": "bad"}]}),
                    &ctx,
                )
                .await;
            assert!(
                write.is_error && patch.is_error,
                "{path}: {write:?} {patch:?}"
            );
        }
        assert_eq!(read(&ctx, "settings/pinned.md"), "keep");
        assert!(!root.join("settings/new.json").exists());
    }

    #[tokio::test]
    async fn oversized_patch_input_and_expansion_leave_the_file_unchanged() {
        let (_dir, ctx) = setup();
        let path = ctx.workspace.root().join("large.txt");
        let file = fs::File::create(&path).unwrap();
        file.set_len((MAX_PATCH_BYTES as u64) * 1024).unwrap();
        let result = ApplyPatch
            .call(
                json!({"path": "large.txt", "edits": [{"old": "a", "new": "b"}]}),
                &ctx,
            )
            .await;
        assert!(
            result.is_error && result.content.contains("1 MiB"),
            "{result:?}"
        );
        assert_eq!(
            file.metadata().unwrap().len(),
            (MAX_PATCH_BYTES as u64) * 1024
        );

        fs::write(&path, "a").unwrap();
        let edits = vec![json!({"old": "a", "new": "aa", "replace_all": true}); 32];
        let result = ApplyPatch
            .call(json!({"path": "large.txt", "edits": edits}), &ctx)
            .await;
        assert!(
            result.is_error && result.content.contains("resource limits"),
            "{result:?}"
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "a");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_tools_reject_nonregular_files_without_opening_them() {
        use std::ffi::CString;
        let (_dir, ctx) = setup();
        let path = ctx.workspace.root().join("pipe");
        let name = CString::new(path.as_os_str().as_encoded_bytes()).unwrap();
        // Only creates a FIFO inside the test's private temporary workspace.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        let write = WriteFile
            .call(json!({"path": "pipe", "content": "x"}), &ctx)
            .await;
        let patch = ApplyPatch
            .call(
                json!({"path": "pipe", "edits": [{"old": "a", "new": "b"}]}),
                &ctx,
            )
            .await;
        assert!(write.is_error && patch.is_error);
        assert!(!fs::metadata(path).unwrap().is_file());
    }
}
