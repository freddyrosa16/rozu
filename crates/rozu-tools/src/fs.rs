//! Read-only file tools: `read_file`, `list_dir` and `grep`.

use std::fmt::Write as _;
use std::path::Path;

use async_trait::async_trait;
use ignore::WalkBuilder;
use regex::RegexBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::tool::{parse_args, schema_for};
use crate::{Risk, Tool, ToolCtx, ToolOutput};

/// Files larger than this are refused by `read_file` and skipped by `grep`.
const MAX_FILE_BYTES: u64 = 10 * 1024 * 1024;
/// How far into a file to look for a NUL byte when deciding whether it is binary.
const BINARY_SNIFF_BYTES: usize = 8 * 1024;
const MAX_DIR_ENTRIES: usize = 1000;
const DEFAULT_GREP_RESULTS: usize = 100;
const MAX_GREP_RESULTS: usize = 1000;
const MAX_GREP_LINE_CHARS: usize = 300;

fn is_binary(bytes: &[u8]) -> bool {
    bytes[..bytes.len().min(BINARY_SNIFF_BYTES)].contains(&0)
}

macro_rules! try_output {
    ($expr:expr) => {
        match $expr {
            Ok(value) => value,
            Err(err) => return ToolOutput::error(err.to_string()),
        }
    };
}

// ---------------------------------------------------------------------------------------------
// read_file

/// Reads a UTF-8 text file in the workspace.
#[derive(Debug, Clone, Copy, Default)]
pub struct ReadFile;

#[derive(Debug, Deserialize, JsonSchema)]
struct ReadFileArgs {
    /// File path, relative to the workspace root.
    path: String,
    /// First line to return (1-based, inclusive). Defaults to the first line.
    #[serde(default)]
    start_line: Option<usize>,
    /// Last line to return (1-based, inclusive). Defaults to the last line.
    #[serde(default)]
    end_line: Option<usize>,
}

#[async_trait]
impl Tool for ReadFile {
    fn name(&self) -> &'static str {
        "read_file"
    }

    fn description(&self) -> &'static str {
        "Read a text file in the workspace. Returns the lines prefixed with their 1-based line \
         numbers. Use start_line/end_line to read part of a large file."
    }

    fn schema(&self) -> Value {
        schema_for::<ReadFileArgs>()
    }

    fn risk(&self, _args: &Value) -> Risk {
        Risk::Read
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput {
        let args: ReadFileArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let path = try_output!(ctx.workspace.resolve(&args.path));
        let meta = try_output!(tokio::fs::metadata(&path).await);
        if meta.is_dir() {
            return ToolOutput::error(format!("{} is a directory; use list_dir", args.path));
        }
        if meta.len() > MAX_FILE_BYTES {
            return ToolOutput::error(format!(
                "{} is too large ({} bytes); use grep to find the relevant part",
                args.path,
                meta.len()
            ));
        }
        let bytes = try_output!(tokio::fs::read(&path).await);
        if is_binary(&bytes) {
            return ToolOutput::error(format!("{} looks like a binary file", args.path));
        }
        let text = String::from_utf8_lossy(&bytes);
        number_lines(&text, args.start_line, args.end_line)
    }
}

fn number_lines(text: &str, start: Option<usize>, end: Option<usize>) -> ToolOutput {
    let total = text.lines().count();
    if total == 0 {
        return ToolOutput::ok("(empty file)");
    }
    let start = start.unwrap_or(1).max(1);
    let end = end.unwrap_or(total).min(total);
    if start > total {
        return ToolOutput::error(format!(
            "start_line {start} is past the end of the file ({total} lines)"
        ));
    }
    if start > end {
        return ToolOutput::error(format!("start_line {start} is after end_line {end}"));
    }
    let mut out = String::new();
    for (index, line) in text.lines().enumerate().take(end).skip(start - 1) {
        let _ = writeln!(out, "{:>6}\t{line}", index + 1);
    }
    ToolOutput::ok(out)
}

// ---------------------------------------------------------------------------------------------
// list_dir

/// Lists one directory in the workspace (not recursive).
#[derive(Debug, Clone, Copy, Default)]
pub struct ListDir;

#[derive(Debug, Deserialize, JsonSchema)]
struct ListDirArgs {
    /// Directory path, relative to the workspace root. Defaults to the root.
    #[serde(default)]
    path: Option<String>,
}

#[async_trait]
impl Tool for ListDir {
    fn name(&self) -> &'static str {
        "list_dir"
    }

    fn description(&self) -> &'static str {
        "List the entries of a directory in the workspace (not recursive). Directories end with \
         '/', symlinks with '@'."
    }

    fn schema(&self) -> Value {
        schema_for::<ListDirArgs>()
    }

    fn risk(&self, _args: &Value) -> Risk {
        Risk::Read
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput {
        let args: ListDirArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let requested = args.path.as_deref().unwrap_or(".");
        let path = try_output!(ctx.workspace.resolve(requested));
        let mut dir = match tokio::fs::read_dir(&path).await {
            Ok(dir) => dir,
            Err(err) if err.kind() == std::io::ErrorKind::NotADirectory => {
                return ToolOutput::error(format!("{requested} is not a directory"));
            }
            Err(err) => return ToolOutput::error(err.to_string()),
        };

        let mut entries = Vec::new();
        while let Some(entry) = try_output!(dir.next_entry().await) {
            let name = entry.file_name().to_string_lossy().into_owned();
            let suffix = match entry.file_type().await {
                Ok(kind) if kind.is_dir() => "/",
                Ok(kind) if kind.is_symlink() => "@",
                _ => "",
            };
            entries.push(format!("{name}{suffix}"));
        }
        if entries.is_empty() {
            return ToolOutput::ok("(empty directory)");
        }
        entries.sort();
        let total = entries.len();
        entries.truncate(MAX_DIR_ENTRIES);
        let mut out = entries.join("\n");
        if total > MAX_DIR_ENTRIES {
            let _ = write!(out, "\n[… {} more entries]", total - MAX_DIR_ENTRIES);
        }
        ToolOutput::ok(out)
    }
}

// ---------------------------------------------------------------------------------------------
// grep

/// Regex search over the workspace, honoring `.gitignore` and skipping hidden and binary files.
#[derive(Debug, Clone, Copy, Default)]
pub struct Grep;

#[derive(Debug, Deserialize, JsonSchema)]
struct GrepArgs {
    /// Regular expression to search for (Rust `regex` syntax).
    pattern: String,
    /// File or directory to search, relative to the workspace root. Defaults to the whole
    /// workspace.
    #[serde(default)]
    path: Option<String>,
    /// Match case-insensitively.
    #[serde(default)]
    case_insensitive: bool,
    /// Maximum number of matching lines to return (default 100, at most 1000).
    #[serde(default)]
    max_results: Option<usize>,
}

#[async_trait]
impl Tool for Grep {
    fn name(&self) -> &'static str {
        "grep"
    }

    fn description(&self) -> &'static str {
        "Search file contents in the workspace with a regular expression. Respects .gitignore and \
         skips hidden and binary files. Returns matching lines as `path:line: text`."
    }

    fn schema(&self) -> Value {
        schema_for::<GrepArgs>()
    }

    fn risk(&self, _args: &Value) -> Risk {
        Risk::Read
    }

    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput {
        let args: GrepArgs = match parse_args(args) {
            Ok(args) => args,
            Err(output) => return output,
        };
        let regex = match RegexBuilder::new(&args.pattern)
            .case_insensitive(args.case_insensitive)
            .build()
        {
            Ok(regex) => regex,
            Err(err) => return ToolOutput::error(format!("invalid pattern: {err}")),
        };
        let start = try_output!(ctx.workspace.resolve(args.path.as_deref().unwrap_or(".")));
        let limit = args
            .max_results
            .unwrap_or(DEFAULT_GREP_RESULTS)
            .clamp(1, MAX_GREP_RESULTS);
        let ctx = ctx.clone();
        let search = tokio::task::spawn_blocking(move || search(&regex, &start, limit, &ctx));
        match search.await {
            Ok(output) => output,
            Err(err) => ToolOutput::error(format!("grep failed: {err}")),
        }
    }
}

fn search(regex: &regex::Regex, start: &Path, limit: usize, ctx: &ToolCtx) -> ToolOutput {
    let mut matches = Vec::new();
    let walker = WalkBuilder::new(start)
        .require_git(false)
        .sort_by_file_name(|a, b| a.cmp(b))
        .build();
    'files: for entry in walker {
        if ctx.cancel.is_cancelled() {
            return ToolOutput::error("cancelled");
        }
        let Ok(entry) = entry else { continue };
        if !entry.file_type().is_some_and(|kind| kind.is_file()) {
            continue;
        }
        if entry
            .metadata()
            .is_ok_and(|meta| meta.len() > MAX_FILE_BYTES)
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if is_binary(&bytes) {
            continue;
        }
        let text = String::from_utf8_lossy(&bytes);
        let display = ctx.workspace.display_relative(entry.path());
        for (index, line) in text.lines().enumerate() {
            if regex.is_match(line) {
                let line: String = line.trim_end().chars().take(MAX_GREP_LINE_CHARS).collect();
                matches.push(format!("{display}:{}: {line}", index + 1));
                if matches.len() >= limit {
                    break 'files;
                }
            }
        }
    }
    if matches.is_empty() {
        return ToolOutput::ok("no matches");
    }
    let mut out = matches.join("\n");
    if matches.len() >= limit {
        let _ = write!(
            out,
            "\n[stopped after {limit} matches; narrow the pattern or path]"
        );
    }
    ToolOutput::ok(out)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use serde_json::json;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::Workspace;

    fn setup() -> (tempfile::TempDir, ToolCtx) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("ws");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("README.md"), "# Demo\nHello rozu\nbye\n").unwrap();
        fs::write(
            root.join("src/main.rs"),
            "fn main() {\n    println!(\"hello\");\n}\n",
        )
        .unwrap();
        fs::write(root.join("blob.bin"), b"abc\0def hello").unwrap();
        fs::write(root.join(".gitignore"), "target/\n").unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join("target/out.txt"), "hello from build output").unwrap();
        fs::write(dir.path().join("outside.txt"), "hello outside").unwrap();
        let ctx = ToolCtx::new(Workspace::new(&root).unwrap(), CancellationToken::new());
        (dir, ctx)
    }

    #[tokio::test]
    async fn read_file_numbers_lines() {
        let (_dir, ctx) = setup();
        let out = ReadFile.call(json!({"path": "README.md"}), &ctx).await;
        assert!(!out.is_error, "{out:?}");
        assert_eq!(
            out.content,
            "     1\t# Demo\n     2\tHello rozu\n     3\tbye\n"
        );
    }

    #[tokio::test]
    async fn read_file_line_range() {
        let (_dir, ctx) = setup();
        let out = ReadFile
            .call(
                json!({"path": "README.md", "start_line": 2, "end_line": 2}),
                &ctx,
            )
            .await;
        assert_eq!(out.content, "     2\tHello rozu\n");
        let out = ReadFile
            .call(json!({"path": "README.md", "start_line": 9}), &ctx)
            .await;
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn read_file_rejects_escape_binary_and_dirs() {
        let (_dir, ctx) = setup();
        for (args, needle) in [
            (json!({"path": "../outside.txt"}), "outside the workspace"),
            (json!({"path": "blob.bin"}), "binary"),
            (json!({"path": "src"}), "directory"),
            (json!({"path": "nope.txt"}), "no such file"),
            (json!({"wrong": 1}), "invalid arguments"),
        ] {
            let out = ReadFile.call(args.clone(), &ctx).await;
            assert!(out.is_error, "{args}: {out:?}");
            assert!(out.content.contains(needle), "{args}: {}", out.content);
        }
    }

    #[tokio::test]
    async fn list_dir_lists_sorted_entries() {
        let (_dir, ctx) = setup();
        let out = ListDir.call(json!({}), &ctx).await;
        assert_eq!(
            out.content, ".gitignore\nREADME.md\nblob.bin\nsrc/\ntarget/",
            "{out:?}"
        );
        let out = ListDir.call(json!({"path": "src"}), &ctx).await;
        assert_eq!(out.content, "main.rs");
        let out = ListDir.call(json!({"path": ".."}), &ctx).await;
        assert!(out.is_error && out.content.contains("outside"), "{out:?}");
    }

    #[tokio::test]
    async fn grep_finds_matches_and_honors_ignores() {
        let (_dir, ctx) = setup();
        let out = Grep
            .call(json!({"pattern": "hello", "case_insensitive": true}), &ctx)
            .await;
        assert!(!out.is_error, "{out:?}");
        let main = Path::new("src").join("main.rs").display().to_string();
        assert_eq!(
            out.content,
            format!("README.md:2: Hello rozu\n{main}:2:     println!(\"hello\");")
        );
    }

    #[tokio::test]
    async fn grep_respects_limits_and_scope() {
        let (_dir, ctx) = setup();
        let out = Grep
            .call(
                json!({"pattern": "o", "path": "README.md", "max_results": 1}),
                &ctx,
            )
            .await;
        assert!(out.content.starts_with("README.md:1: # Demo"), "{out:?}");
        assert!(out.content.contains("stopped after 1 matches"));

        let out = Grep.call(json!({"pattern": "zzz"}), &ctx).await;
        assert_eq!(out.content, "no matches");

        let out = Grep.call(json!({"pattern": "("}), &ctx).await;
        assert!(out.is_error && out.content.contains("invalid pattern"));

        let out = Grep
            .call(json!({"pattern": "hello", "path": "../"}), &ctx)
            .await;
        assert!(out.is_error && out.content.contains("outside"));
    }
}
