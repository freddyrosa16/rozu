//! Workspace write tools: `write_file` and `apply_patch`.
//!
//! Both resolve their path with [`Workspace::resolve_for_write`] and refuse anything outside the
//! workspace (or inside `.rozu/`) before approval is even asked, so these tools can never write
//! outside the workspace regardless of the approval mode.

use std::fmt::Write as _;
use std::path::PathBuf;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::patch::{SearchReplace, apply_search_replace, apply_unified_diff};
use crate::tool::{parse_args, schema_for};
use crate::truncate::truncate_head_tail;
use crate::{PathError, Risk, Tool, ToolCtx, ToolOutput, Workspace};

/// Approval previews longer than this are cut (head + tail).
const MAX_PREVIEW_BYTES: usize = 8 * 1024;

/// Resolves a write target, rejecting paths outside the workspace and in `.rozu/`.
fn write_target(workspace: &Workspace, path: &str) -> Result<PathBuf, ToolOutput> {
    let resolved = workspace
        .resolve_for_write(path)
        .map_err(|err| ToolOutput::error(err.to_string()))?;
    if workspace.is_protected(&resolved) {
        return Err(ToolOutput::error(format!(
            "{path} is in rozu's settings directory (.rozu/), which tools may not modify"
        )));
    }
    if resolved.is_dir() {
        return Err(ToolOutput::error(format!("{path} is a directory")));
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

async fn write_atomically(path: &std::path::Path, contents: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    // Write next to the target and rename, so a failure never leaves a half-written file.
    let tmp = path.with_file_name(format!(
        ".{}.rozu-tmp",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    tokio::fs::write(&tmp, contents).await?;
    if let Ok(meta) = tokio::fs::metadata(path).await {
        let _ = tokio::fs::set_permissions(&tmp, meta.permissions()).await;
    }
    if let Err(err) = tokio::fs::rename(&tmp, path).await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(err);
    }
    Ok(())
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
        let args: WriteFileArgs = parse_args(args.clone())?;
        write_target(&ctx.workspace, &args.path).map(|_| ())
    }

    fn preview(&self, args: &Value) -> String {
        let path = path_arg(args).unwrap_or("?");
        let content = args.get("content").and_then(Value::as_str).unwrap_or("");
        format!(
            "write {path} ({} bytes):\n{}",
            content.len(),
            truncate_head_tail(content, MAX_PREVIEW_BYTES)
        )
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput {
        let args: WriteFileArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let path = match write_target(&ctx.workspace, &args.path) {
            Ok(path) => path,
            Err(output) => return output,
        };
        let existed = path.exists();
        match write_atomically(&path, &args.content).await {
            Ok(()) => ToolOutput::ok(format!(
                "{} {} ({} bytes)",
                if existed { "overwrote" } else { "created" },
                ctx.workspace.display_relative(&path),
                args.content.len()
            )),
            Err(err) => ToolOutput::error(format!("could not write {}: {err}", args.path)),
        }
    }
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
        truncate_head_tail(&out, MAX_PREVIEW_BYTES).into_owned()
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
        let original = match tokio::fs::read(&path).await {
            Ok(bytes) => match String::from_utf8(bytes) {
                Ok(text) => Some(text),
                Err(_) => {
                    return ToolOutput::error(format!("{} is not UTF-8 text", args.path));
                }
            },
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => None,
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
        match write_atomically(&path, &updated).await {
            Ok(()) => ToolOutput::ok(format!(
                "patched {} ({} → {} lines)",
                ctx.workspace.display_relative(&path),
                original.as_deref().map_or(0, |t| t.lines().count()),
                updated.lines().count()
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
}
