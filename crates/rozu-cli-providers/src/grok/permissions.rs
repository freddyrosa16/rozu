//! Grok's agent → client requests, answered by rozu.
//!
//! | request | answer | audit `decided_by` |
//! |---|---|---|
//! | `session/request_permission` for `use_tool` → `rozu__<tool>` (one of rozu's tools) | the `allow_once` option | `delegated-to-rozu-mcp`: `rozu mcp-serve` applies the policy, sandbox and audit log when the call runs |
//! | `session/request_permission` for anything else (a built-in, another MCP server, an unknown tool) | the `reject_once` option (else `cancelled`) | `policy` (default-deny) |
//! | any request while the turn is being cancelled | `cancelled` | `cancelled` |
//! | `fs/*`, `terminal/*` (rozu advertises neither) and unknown methods | JSON-RPC error | `policy` |
//!
//! Approvals are always one-time: rozu never picks an `allow_always` option. Every decision is
//! in the audit log, tagged with the executor's provider (`grok`).

use rozu_core::{
    ApprovalRecord, ApprovalRequest, Decision, DecisionSource, SandboxMode, ToolExecutor,
};
use rozu_mcp::SERVER_NAME;
use rozu_tools::{OutsideWrites, Risk, ToolCtx, Workspace};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::protocol::{
    METHOD_NOT_FOUND, Mapper, ROZU_TOOL_PREFIX, USE_TOOL, error_response, response,
};

/// Longest preview shown on an approval card or in the audit log.
const MAX_PREVIEW_CHARS: usize = 4000;

/// The reply to send, and the decisions made on the way.
#[derive(Debug)]
pub struct Answer {
    pub reply: Value,
    pub records: Vec<ApprovalRecord>,
}

/// Answers the agent request `method` (JSON-RPC id `id`). `mapper` knows the session's tool
/// calls (a permission request may carry only the call id); `cancelled` is set while the turn
/// is being cancelled.
pub fn answer(
    id: &Value,
    method: &str,
    params: &Value,
    exec: &ToolExecutor,
    workspace: &Workspace,
    mapper: Option<&Mapper>,
    cancelled: bool,
) -> Answer {
    if method != "session/request_permission" {
        let reason = format!(
            "rozu gives Grok no file system or terminal and can't map the request `{method}` to \
             a tool and risk class, so it is denied by default"
        );
        warn!(target: "rozu::cli", method, "refused a grok request");
        let record = record(
            exec,
            &format!("request-{}", id_text(id)),
            &format!("grok:{method}"),
            Value::Null,
            &format!("{method} {params}"),
            &Decision::Deny {
                reason: reason.clone(),
            },
            &DecisionSource::Policy,
        );
        return Answer {
            reply: error_response(id, METHOD_NOT_FOUND, &reason),
            records: vec![record],
        };
    }
    permission(id, params, exec, workspace, mapper, cancelled)
}

/// What a permission request is for.
enum Target {
    /// One of rozu's tools (registry name), with its arguments.
    Rozu(String, Value),
    /// Something rozu doesn't run: its display name and why it is refused.
    Other(String, &'static str),
}

/// The tool a permission request is for: only a `use_tool` call naming `rozu__<tool>` of rozu's
/// registry (or the MCP name itself as the call's title) counts as rozu's.
fn target(params: &Value, exec: &ToolExecutor, mapper: Option<&Mapper>) -> Target {
    let call = &params["toolCall"];
    let id = call["toolCallId"].as_str().unwrap_or_default();
    let known = mapper.and_then(|m| m.call(id));
    let name = ["toolName", "title", "kind"]
        .iter()
        .find_map(|key| call[*key].as_str().filter(|s| !s.is_empty()));
    let raw_input = if call["rawInput"].is_object() {
        call["rawInput"].clone()
    } else {
        Value::Null
    };
    let (mcp_name, args) = match name {
        // `use_tool {"tool_name": "rozu__read_file", "tool_input": {…}}`
        Some(USE_TOOL) | None => match raw_input["tool_name"].as_str() {
            Some(tool) => (
                tool.to_owned(),
                raw_input.get("tool_input").cloned().unwrap_or(json!({})),
            ),
            None => match known {
                // Announced earlier as a rozu tool (the mapper strips the prefix).
                Some((tool, args)) if !tool.starts_with("grok:") => {
                    (format!("{ROZU_TOOL_PREFIX}{tool}"), args.clone())
                }
                _ => {
                    return Target::Other(
                        format!("grok:{}", name.unwrap_or("tool")),
                        "the request doesn't say which tool use_tool would call",
                    );
                }
            },
        },
        Some(title) if title.starts_with(ROZU_TOOL_PREFIX) => (title.to_owned(), raw_input),
        Some(other) => {
            return Target::Other(
                format!("grok:{other}"),
                "it is not one of rozu's tools (Grok's built-ins are off; rozu's tools come \
                 only from its own MCP server)",
            );
        }
    };
    match mcp_name.strip_prefix(ROZU_TOOL_PREFIX) {
        Some(tool) if exec.tools.get(tool).is_some() => Target::Rozu(tool.to_owned(), args),
        Some(_) => Target::Other(format!("grok:{mcp_name}"), "rozu has no tool by that name"),
        None => Target::Other(
            format!("grok:{mcp_name}"),
            "only rozu's own MCP tools (rozu__*) may run",
        ),
    }
}

/// The `optionId` of the first option of `kind`.
fn option(params: &Value, kind: &str) -> Option<String> {
    params["options"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|o| o["kind"] == kind)
        .and_then(|o| o["optionId"].as_str())
        .map(str::to_owned)
}

fn selected(option_id: &str) -> Value {
    json!({ "outcome": { "outcome": "selected", "optionId": option_id } })
}

fn cancelled_outcome() -> Value {
    json!({ "outcome": { "outcome": "cancelled" } })
}

fn permission(
    id: &Value,
    params: &Value,
    exec: &ToolExecutor,
    workspace: &Workspace,
    mapper: Option<&Mapper>,
    cancelled: bool,
) -> Answer {
    let call_id = params["toolCall"]["toolCallId"]
        .as_str()
        .map_or_else(|| format!("permission-{}", id_text(id)), str::to_owned);
    let deny = |tool: &str, args: Value, preview: String, reason: String, source| {
        let record = record(
            exec,
            &call_id,
            tool,
            args,
            &preview,
            &Decision::Deny { reason },
            &source,
        );
        let reply = match option(params, "reject_once") {
            Some(option) if source != DecisionSource::Cancelled => selected(&option),
            _ => cancelled_outcome(),
        };
        Answer {
            reply: response(id, reply),
            records: vec![record],
        }
    };
    match target(params, exec, mapper) {
        Target::Rozu(tool_name, args) => {
            let tool_label = format!("{SERVER_NAME}:{tool_name}");
            let preview = format!(
                "Grok asks to run rozu's MCP tool `{tool_name}`; rozu mcp-serve decides the call \
                 itself\n{}",
                serde_json::to_string_pretty(&args).unwrap_or_default()
            );
            if cancelled {
                return deny(
                    &tool_label,
                    args,
                    preview,
                    "the turn was cancelled".into(),
                    DecisionSource::Cancelled,
                );
            }
            let Some(allow) = option(params, "allow_once") else {
                return deny(
                    &tool_label,
                    args,
                    preview,
                    "Grok offered no one-time approval, and rozu never approves for the session"
                        .into(),
                    DecisionSource::Policy,
                );
            };
            let tool = exec.tools.get(&tool_name).expect("checked by target()");
            let ctx = ToolCtx::new(workspace.clone(), CancellationToken::new());
            let request = ApprovalRequest {
                call_id: call_id.clone(),
                tool: tool_label,
                risk: tool.risk(&args, &ctx),
                sandbox: exec
                    .policy
                    .confinement(
                        tool.as_ref(),
                        false,
                        &OutsideWrites::default(),
                        exec.sandbox.as_ref(),
                    )
                    .mode,
                args,
                preview: clip(preview),
                network: false,
                extra_writes: Vec::new(),
                outside_unknown: false,
                blocked_retry: false,
                allow_rule: None,
            };
            let record =
                exec.record_decision(&request, &Decision::Approve, &DecisionSource::Delegated);
            Answer {
                reply: response(id, selected(&allow)),
                records: vec![record],
            }
        }
        Target::Other(tool, why) => {
            let input = &params["toolCall"]["rawInput"];
            let preview = format!("{tool} {input}");
            let (reason, source) = if cancelled {
                (
                    "the turn was cancelled".to_owned(),
                    DecisionSource::Cancelled,
                )
            } else {
                (
                    format!("rozu denies `{tool}` by default: {why}"),
                    DecisionSource::Policy,
                )
            };
            deny(&tool, input.clone(), preview, reason, source)
        }
    }
}

fn record(
    exec: &ToolExecutor,
    call_id: &str,
    tool: &str,
    args: Value,
    preview: &str,
    decision: &Decision,
    source: &DecisionSource,
) -> ApprovalRecord {
    let request = ApprovalRequest {
        call_id: call_id.to_owned(),
        tool: tool.to_owned(),
        args,
        risk: Risk::Destructive,
        preview: clip(preview.to_owned()),
        sandbox: SandboxMode::Off,
        network: false,
        extra_writes: Vec::new(),
        outside_unknown: false,
        blocked_retry: false,
        allow_rule: None,
    };
    exec.record_decision(&request, decision, source)
}

fn id_text(id: &Value) -> String {
    id.as_str().map_or_else(|| id.to_string(), str::to_owned)
}

fn clip(mut text: String) -> String {
    if text.chars().count() > MAX_PREVIEW_CHARS {
        text = text.chars().take(MAX_PREVIEW_CHARS).collect();
        text.push_str("\n…");
    }
    text
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use rozu_core::{ApprovalMode, ApprovalPolicy, ReadOnlyGate};
    use rozu_tools::{NoSandbox, ToolRegistry};

    use super::*;

    struct Setup {
        _dir: tempfile::TempDir,
        exec: ToolExecutor,
        workspace: Workspace,
    }

    impl Setup {
        fn new() -> Self {
            let dir = tempfile::tempdir().unwrap();
            let exec = ToolExecutor::new(
                Arc::new(ToolRegistry::standard()),
                Arc::new(ReadOnlyGate),
                Arc::new(ApprovalPolicy::new(ApprovalMode::AutoWorkspace)),
                Arc::new(NoSandbox),
            )
            .with_provider(Some("grok".into()));
            let workspace = Workspace::new(dir.path()).unwrap();
            Self {
                _dir: dir,
                exec,
                workspace,
            }
        }

        fn answer(&self, id: Value, method: &str, params: &Value, cancelled: bool) -> Answer {
            answer(
                &id,
                method,
                params,
                &self.exec,
                &self.workspace,
                None,
                cancelled,
            )
        }
    }

    fn options() -> Value {
        json!([
            {"optionId": "allow-always", "name": "Always", "kind": "allow_always"},
            {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
            {"optionId": "reject", "name": "Reject", "kind": "reject_once"},
        ])
    }

    fn ask(tool_call: Value) -> Answer {
        let params = json!({"sessionId": "s1", "toolCall": tool_call, "options": options()});
        Setup::new().answer(json!(7), "session/request_permission", &params, false)
    }

    #[test]
    fn approves_once_only_rozu_tools_via_use_tool() {
        let answer = ask(json!({"toolCallId": "c1", "title": "use_tool",
            "rawInput": {"tool_name": "rozu__read_file", "tool_input": {"path": "a"}}}));
        assert_eq!(answer.reply["result"]["outcome"]["optionId"], "allow");
        assert_eq!(answer.records[0].source, DecisionSource::Delegated);
        assert_eq!(answer.records[0].tool, "rozu:read_file");
        assert_eq!(answer.records[0].decision, Decision::Approve);
        // The MCP name as the title works too.
        let answer =
            ask(json!({"toolCallId": "c2", "title": "rozu__shell", "rawInput": {"command": "ls"}}));
        assert_eq!(answer.reply["result"]["outcome"]["optionId"], "allow");
    }

    #[test]
    fn denies_everything_else() {
        for call in [
            json!({"toolCallId": "c1", "title": "run_terminal_cmd", "rawInput": {"command": "rm -rf /"}}),
            // A built-in can't borrow a rozu tool's name through its input.
            json!({"toolCallId": "c2", "title": "run_terminal_cmd", "rawInput": {"tool_name": "rozu__shell"}}),
            json!({"toolCallId": "c3", "title": "use_tool", "rawInput": {"tool_name": "github__push"}}),
            json!({"toolCallId": "c4", "title": "use_tool", "rawInput": {"tool_name": "rozu__nope"}}),
            json!({"toolCallId": "c5", "title": "use_tool"}),
        ] {
            let answer = ask(call.clone());
            assert_eq!(
                answer.reply["result"]["outcome"]["optionId"], "reject",
                "{call}"
            );
            assert_eq!(answer.records[0].source, DecisionSource::Policy, "{call}");
            assert!(!answer.records[0].decision.is_approval(), "{call}");
        }
    }

    #[test]
    fn never_approves_for_the_session_and_cancels_when_cancelled() {
        let setup = Setup::new();
        let call = json!({"toolCallId": "c1", "title": "use_tool",
            "rawInput": {"tool_name": "rozu__read_file", "tool_input": {}}});
        let only_always =
            json!({"toolCall": call, "options": [{"optionId": "a", "kind": "allow_always"}]});
        let answer = setup.answer(json!(1), "session/request_permission", &only_always, false);
        assert_eq!(answer.reply["result"]["outcome"]["outcome"], "cancelled");
        assert!(!answer.records[0].decision.is_approval());

        let params = json!({"toolCall": call, "options": options()});
        let cancelled = setup.answer(json!(2), "session/request_permission", &params, true);
        assert_eq!(cancelled.reply["result"]["outcome"]["outcome"], "cancelled");
        assert_eq!(cancelled.records[0].source, DecisionSource::Cancelled);
    }

    #[test]
    fn refuses_fs_and_terminal_requests() {
        let answer = Setup::new().answer(
            json!("r1"),
            "fs/read_text_file",
            &json!({"path": "/etc/passwd"}),
            false,
        );
        assert_eq!(answer.reply["error"]["code"], METHOD_NOT_FOUND);
        assert_eq!(answer.records[0].tool, "grok:fs/read_text_file");
    }
}
