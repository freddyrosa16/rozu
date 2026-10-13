//! Claude Code specifics: the tools rozu disables and how a permission prompt
//! (`--permission-prompt-tool mcp__rozu__approve`) maps onto rozu's risk classes.
//!
//! | Claude tool | rozu risk | decided by |
//! |---|---|---|
//! | `mcp__rozu__*` (rozu's own tools) | — | allowed here; gated when the tool runs |
//! | `Read`, `Glob`, `Grep`, `LS`, `NotebookRead`, `TodoWrite`, `ExitPlanMode` | read | policy; paths outside the workspace always ask |
//! | `Bash` | — | denied: Claude would run it outside rozu's sandbox; `mcp__rozu__shell` is the sandboxed equivalent |
//! | `Write`, `Edit`, `MultiEdit`, `NotebookEdit` | write in / outside workspace (unsandboxed) | policy; `.rozu/` is denied |
//! | `WebFetch`, `WebSearch` | network | policy |
//! | anything else | — | denied (default-deny) |
//!
//! The mutating built-ins are disallowed at launch ([`DISALLOWED_BUILTIN_TOOLS`]), so the rows
//! for them are a second line of defence.

use std::path::Path;

use async_trait::async_trait;
use rozu_core::{ApprovalRequest, SandboxMode};
use rozu_tools::{PathError, Risk, Tool, ToolCtx, ToolOutput};
use serde_json::{Value, json};

/// MCP server name rozu registers in the `--mcp-config`; Claude prefixes tool names with
/// `mcp__<server>__`.
pub const MCP_SERVER: &str = "rozu";
/// Prefix of rozu's tools as Claude names them.
pub const ROZU_TOOL_PREFIX: &str = "mcp__rozu__";

/// Claude Code built-ins that write, run commands, reach the network or spawn sub-agents. rozu
/// starts Claude with these disallowed; its own (gated, sandboxed) tools replace them.
pub const DISALLOWED_BUILTIN_TOOLS: &[&str] = &[
    "Bash",
    "BashOutput",
    "KillShell",
    "Edit",
    "MultiEdit",
    "Write",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "Task",
];

/// Longest preview shown on an approval card for a built-in tool's input.
const MAX_PREVIEW_CHARS: usize = 4000;

/// How a permission prompt is decided.
pub enum PermissionRequest {
    /// One of rozu's own tools: allowed here because the tool call itself goes through the
    /// approval policy when it runs.
    RozuTool(String),
    /// A request with a risk class, decided by the policy (and the user, if it asks).
    Policy {
        request: ApprovalRequest,
        /// Supplies "always allow" rule matching.
        rule_tool: &'static dyn Tool,
    },
    /// Reads outside the workspace: always asks.
    AskAlways(ApprovalRequest),
    /// Denied without asking.
    Deny {
        request: ApprovalRequest,
        reason: String,
    },
}

/// Maps Claude's permission prompt for `tool_name` with `input` onto rozu's policy.
pub fn map_permission_request(
    tool_name: &str,
    input: &Value,
    call_id: &str,
    ctx: &ToolCtx,
) -> PermissionRequest {
    if let Some(name) = tool_name.strip_prefix(ROZU_TOOL_PREFIX) {
        return PermissionRequest::RozuTool(name.to_owned());
    }
    let request = |risk, sandbox, network, preview: String, args: Value| ApprovalRequest {
        call_id: call_id.to_owned(),
        tool: tool_name.to_owned(),
        args,
        risk,
        preview,
        sandbox,
        network,
        extra_writes: Vec::new(),
        outside_unknown: false,
        blocked_retry: false,
        allow_rule: None,
    };
    let deny = |reason: String| PermissionRequest::Deny {
        request: request(
            Risk::Destructive,
            SandboxMode::Off,
            false,
            preview(tool_name, input),
            input.clone(),
        ),
        reason,
    };
    let str_field = |key: &str| input.get(key).and_then(Value::as_str);

    match tool_name {
        "Read" | "Glob" | "Grep" | "LS" | "NotebookRead" | "TodoWrite" | "ExitPlanMode" => {
            let path = str_field("file_path")
                .or_else(|| str_field("notebook_path"))
                .or_else(|| str_field("path"));
            let pattern_escapes = tool_name == "Glob"
                && str_field("pattern").is_some_and(|p| {
                    p.starts_with('/') || p.starts_with('~') || p.split('/').any(|s| s == "..")
                });
            let outside = pattern_escapes
                || path.is_some_and(|path| {
                    matches!(
                        ctx.workspace.resolve(path),
                        Err(PathError::Outside(_) | PathError::Io { .. })
                    )
                });
            let req = request(
                Risk::Read,
                SandboxMode::Off,
                false,
                preview(tool_name, input),
                input.clone(),
            );
            if outside {
                PermissionRequest::AskAlways(req)
            } else {
                PermissionRequest::Policy {
                    request: req,
                    rule_tool: &NoRules,
                }
            }
        }
        // Claude would run it itself, outside rozu's sandbox; shell commands only run through
        // rozu's sandboxed `shell` tool.
        "Bash" => deny(format!(
            "rozu runs shell commands only in its sandbox: use {ROZU_TOOL_PREFIX}shell instead \
             of Bash"
        )),
        "Write" | "Edit" | "MultiEdit" | "NotebookEdit" => {
            let Some(path) = str_field("file_path").or_else(|| str_field("notebook_path")) else {
                return deny(format!("{tool_name} without a file path"));
            };
            let risk = match ctx.workspace.resolve_for_write(path) {
                Ok(resolved) if ctx.workspace.is_protected(Path::new(&resolved)) => {
                    return deny(format!(
                        "{path} is inside {}, which only the user may change",
                        rozu_tools::PROTECTED_DIR
                    ));
                }
                Ok(_) => Risk::WriteWorkspace,
                Err(_) => Risk::WriteOutside,
            };
            PermissionRequest::Policy {
                request: request(
                    risk,
                    SandboxMode::Off,
                    false,
                    preview(tool_name, input),
                    input.clone(),
                ),
                rule_tool: &NoRules,
            }
        }
        "WebFetch" | "WebSearch" => PermissionRequest::Policy {
            request: request(
                Risk::Network,
                SandboxMode::Off,
                true,
                preview(tool_name, input),
                input.clone(),
            ),
            rule_tool: &NoRules,
        },
        _ => deny(format!(
            "rozu can't map the tool `{tool_name}` to a risk class, so it is denied by default"
        )),
    }
}

/// `Tool input` as the approval card shows it, cut to a sane length.
fn preview(tool_name: &str, input: &Value) -> String {
    let body = serde_json::to_string_pretty(input).unwrap_or_else(|_| input.to_string());
    let mut text = format!("{tool_name} {body}");
    if text.chars().count() > MAX_PREVIEW_CHARS {
        text = text.chars().take(MAX_PREVIEW_CHARS).collect();
        text.push_str("\n…");
    }
    text
}

/// The text Claude expects back from the permission-prompt tool.
pub fn permission_response(allow: bool, input: &Value, message: &str) -> String {
    let value = if allow {
        json!({ "behavior": "allow", "updatedInput": input })
    } else {
        json!({ "behavior": "deny", "message": message })
    };
    value.to_string()
}

/// A stand-in for tools rozu doesn't run itself: no "always allow" rules ever match.
struct NoRules;

#[async_trait]
impl Tool for NoRules {
    fn name(&self) -> &'static str {
        "claude-builtin"
    }

    fn description(&self) -> &'static str {
        "A Claude Code built-in tool (decided by rozu, run by Claude)."
    }

    fn schema(&self) -> Value {
        json!({ "type": "object" })
    }

    fn risk(&self, _args: &Value, _ctx: &ToolCtx) -> Risk {
        Risk::Destructive
    }

    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolOutput {
        ToolOutput::error("rozu does not run Claude Code's built-in tools")
    }
}

#[cfg(test)]
mod tests {
    use rozu_tools::Workspace;
    use tokio_util::sync::CancellationToken;

    use super::*;

    fn ctx() -> (tempfile::TempDir, ToolCtx) {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolCtx::new(
            Workspace::new(dir.path()).unwrap(),
            CancellationToken::new(),
        );
        (dir, ctx)
    }

    fn map(tool: &str, input: Value) -> PermissionRequest {
        let (_dir, ctx) = ctx();
        map_permission_request(tool, &input, "c1", &ctx)
    }

    fn risk_of(mapped: &PermissionRequest) -> Option<Risk> {
        match mapped {
            PermissionRequest::Policy { request, .. } => Some(request.risk),
            _ => None,
        }
    }

    #[test]
    fn maps_claude_tools_to_risks() {
        assert!(matches!(
            map("mcp__rozu__shell", json!({})),
            PermissionRequest::RozuTool(name) if name == "shell"
        ));
        assert_eq!(
            risk_of(&map("Read", json!({"file_path": "src/main.rs"}))),
            Some(Risk::Read)
        );
        assert!(matches!(
            map("Read", json!({"file_path": "/etc/passwd"})),
            PermissionRequest::AskAlways(_)
        ));
        assert!(matches!(
            map("Glob", json!({"pattern": "../**/*.rs"})),
            PermissionRequest::AskAlways(_)
        ));
        // Claude's own Bash would run unsandboxed: always denied, in favor of rozu's shell.
        for command in ["ls", "rm -rf build", "python3 -c 'import os'"] {
            assert!(matches!(
                map("Bash", json!({ "command": command })),
                PermissionRequest::Deny { reason, .. } if reason.contains("mcp__rozu__shell")
            ));
        }
        assert_eq!(
            risk_of(&map("Write", json!({"file_path": "a.txt", "content": "x"}))),
            Some(Risk::WriteWorkspace)
        );
        assert_eq!(
            risk_of(&map(
                "Edit",
                json!({"file_path": "/etc/hosts", "old_string": "a", "new_string": "b"})
            )),
            Some(Risk::WriteOutside)
        );
        assert_eq!(
            risk_of(&map("WebFetch", json!({"url": "https://example.com"}))),
            Some(Risk::Network)
        );
    }

    #[test]
    fn unmappable_and_protected_requests_are_denied() {
        for (tool, input) in [
            ("SomethingNew", json!({})),
            ("mcp__other__tool", json!({})),
            ("Bash", json!({})),
            ("Write", json!({"content": "x"})),
            (
                "Write",
                json!({"file_path": ".rozu/approvals.json", "content": "{}"}),
            ),
        ] {
            assert!(
                matches!(map(tool, input.clone()), PermissionRequest::Deny { .. }),
                "{tool} {input}"
            );
        }
    }

    #[test]
    fn permission_response_shapes() {
        let input = json!({"file_path": "a"});
        let allow: Value = serde_json::from_str(&permission_response(true, &input, "")).unwrap();
        assert_eq!(
            allow,
            json!({"behavior": "allow", "updatedInput": {"file_path": "a"}})
        );
        let deny: Value = serde_json::from_str(&permission_response(false, &input, "no")).unwrap();
        assert_eq!(deny, json!({"behavior": "deny", "message": "no"}));
    }
}
