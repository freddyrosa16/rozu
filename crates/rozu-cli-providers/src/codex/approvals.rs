//! Codex's server → client requests, answered by rozu's approval policy.
//!
//! | request | rozu risk | decided by | answer |
//! |---|---|---|---|
//! | `item/commandExecution/requestApproval`, `execCommandApproval` (legacy) | the risk classifier on the exact command (Codex's `sh -lc '…'` wrapper unwrapped) | policy, or the user; escalations (a `reason`, a network host, a cwd outside the workspace) always ask | `accept` / `approved`, `decline` / `denied`, `cancel` / `abort` when the turn was cancelled |
//! | `item/fileChange/requestApproval`, `applyPatchApproval` (legacy) | write in / outside workspace per path, delete = destructive, `.rozu/` = denied; a `grantRoot` always asks | policy, or the user | same |
//! | `item/permissions/requestApproval` | network / write outside / read | always the user | the requested permissions for this turn only, or none |
//! | `mcpServer/elicitation/request`: Codex's approval prompt for a tool of the `rozu` MCP server | the tool's own risk class | delegated: `rozu mcp-serve` applies the policy, sandbox and audit log when the call runs | `accept` |
//! | `mcpServer/elicitation/request`: anything else (other servers, free-form forms, URLs, malformed) | — | denied (default-deny) | `decline` |
//! | `item/tool/call` | — | denied (rozu registers no dynamic tools) | `success: false` |
//! | `account/chatgptAuthTokens/refresh` | — | refused: rozu never handles ChatGPT tokens | JSON-RPC error |
//! | anything else (`item/tool/requestUserInput`, `attestation/generate`, new methods) | — | denied | JSON-RPC error |
//!
//! Approvals are always one-time: rozu never answers `acceptForSession`, `approved_for_session`
//! or an exec-policy / network-policy amendment. Its own "always allow" rules (for
//! [`SHELL_TOOL`]) live in the project's `.rozu/approvals.json` like every other rule. Every
//! decision, including the refusals, goes to the audit log.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use rozu_core::{
    AgentEvent, ApprovalRecord, ApprovalRequest, CODEX_SANDBOX, Decision, DecisionSource,
    SandboxMode, ToolExecutor,
};
use rozu_mcp::SERVER_NAME;
use rozu_tools::{OutsideWrites, Risk, Shell, Tool, ToolCtx, ToolOutput, Workspace};
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::protocol::{METHOD_NOT_FOUND, Mapper, PATCH_TOOL, SHELL_TOOL, error_response, response};

/// Longest preview shown on an approval card.
const MAX_PREVIEW_CHARS: usize = 6000;

/// What a request is answered with, and the decisions made on the way.
#[derive(Debug)]
pub struct Answer {
    /// The JSON-RPC response to send.
    pub reply: Value,
    pub records: Vec<ApprovalRecord>,
}

/// Everything a decision needs.
pub struct ApprovalCtx<'a> {
    pub exec: &'a ToolExecutor,
    pub workspace: &'a Workspace,
    pub cancel: &'a CancellationToken,
    /// The thread's items, for file-change approvals (whose params carry only the item id).
    pub mapper: Option<&'a Mapper>,
}

/// Answers the server request `method` with `params` (JSON-RPC id `id`).
pub async fn answer(id: &Value, method: &str, params: &Value, ctx: &ApprovalCtx<'_>) -> Answer {
    match method {
        "item/commandExecution/requestApproval" => {
            let command = params["command"].as_str().map(str::to_owned).or_else(|| {
                item(ctx, params)
                    .and_then(|item| item["command"].as_str())
                    .map(str::to_owned)
            });
            let ask = command.map(|command| CommandAsk {
                call_id: call_id(params),
                command,
                cwd: params["cwd"].as_str().map(str::to_owned),
                reason: non_empty(&params["reason"]),
                network_host: non_empty(&params["networkApprovalContext"]["host"]),
            });
            command_answer(id, ask, Protocol::V2, method, params, ctx).await
        }
        "execCommandApproval" => {
            let ask = params["command"].as_array().map(|argv| CommandAsk {
                call_id: params["callId"].as_str().unwrap_or("codex").to_owned(),
                command: join_argv(argv),
                cwd: params["cwd"].as_str().map(str::to_owned),
                reason: non_empty(&params["reason"]),
                network_host: None,
            });
            command_answer(id, ask, Protocol::Legacy, method, params, ctx).await
        }
        "item/fileChange/requestApproval" => {
            let changes = item(ctx, params).map(changes_from_item);
            let ask = changes.map(|changes| PatchAsk {
                call_id: call_id(params),
                changes,
                grant_root: non_empty(&params["grantRoot"]),
                reason: non_empty(&params["reason"]),
            });
            patch_answer(id, ask, Protocol::V2, method, ctx).await
        }
        "applyPatchApproval" => {
            let ask = params["fileChanges"].as_object().map(|files| PatchAsk {
                call_id: params["callId"].as_str().unwrap_or("codex").to_owned(),
                changes: changes_from_legacy(files),
                grant_root: non_empty(&params["grantRoot"]),
                reason: non_empty(&params["reason"]),
            });
            patch_answer(id, ask, Protocol::Legacy, method, ctx).await
        }
        "item/permissions/requestApproval" => permissions_answer(id, params, ctx).await,
        "mcpServer/elicitation/request" => elicitation_answer(id, params, ctx),
        "item/tool/call" => {
            let tool = params["tool"].as_str().unwrap_or("?");
            let reason = "rozu registers no dynamic tools with Codex, so the call is denied";
            let record = deny_by_policy(
                ctx.exec,
                params["callId"].as_str().unwrap_or("codex"),
                &format!("codex:{tool}"),
                &format!("dynamic tool `{tool}` {}", params["arguments"]),
                reason,
            );
            Answer {
                reply: response(
                    id,
                    json!({
                        "contentItems": [{ "type": "inputText", "text": format!("denied by rozu: {reason}") }],
                        "success": false,
                    }),
                ),
                records: vec![record],
            }
        }
        "account/chatgptAuthTokens/refresh" => {
            // rozu never handles ChatGPT tokens; Codex's own login (`codex login`) owns them.
            let reason = "rozu does not handle ChatGPT tokens; log in with the official Codex \
                          CLI (`rozu auth login codex` runs `codex login`)";
            warn!(target: "rozu::cli", "codex asked rozu to refresh ChatGPT tokens; refused");
            let record = deny_by_policy(
                ctx.exec,
                &format!("auth-{}", id_text(id)),
                "codex:chatgpt_auth_tokens_refresh",
                method,
                reason,
            );
            Answer {
                reply: error_response(id, METHOD_NOT_FOUND, reason),
                records: vec![record],
            }
        }
        other => {
            let reason = format!(
                "rozu can't map the Codex request `{other}` to a tool and risk class, so it is \
                 denied by default"
            );
            let record = deny_by_policy(
                ctx.exec,
                &format!("request-{}", id_text(id)),
                &format!("codex:{other}"),
                other,
                &reason,
            );
            Answer {
                reply: error_response(id, METHOD_NOT_FOUND, &reason),
                records: vec![record],
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Protocol {
    /// `item/*/requestApproval`: `accept` / `decline` / `cancel`.
    V2,
    /// `execCommandApproval` / `applyPatchApproval`: `approved` / `denied` / `abort`.
    Legacy,
}

fn decision_reply(protocol: Protocol, decision: &Decision, source: &DecisionSource) -> Value {
    let cancelled = *source == DecisionSource::Cancelled;
    match (protocol, decision) {
        // `u` and "always allow" are one-time approvals here too (rozu keeps its own rules).
        (Protocol::V2, d) if d.is_approval() => json!({ "decision": "accept" }),
        (Protocol::V2, _) if cancelled => json!({ "decision": "cancel" }),
        (Protocol::V2, _) => json!({ "decision": "decline" }),
        (Protocol::Legacy, d) if d.is_approval() => json!({ "decision": "approved" }),
        (Protocol::Legacy, _) if cancelled => json!({ "decision": "abort" }),
        (Protocol::Legacy, Decision::Deny { reason }) => {
            json!({ "decision": { "denied": { "rejection": format!("denied by rozu: {reason}") } } })
        }
        (Protocol::Legacy, _) => json!({ "decision": "abort" }),
    }
}

struct CommandAsk {
    call_id: String,
    /// Exactly as Codex sent it.
    command: String,
    cwd: Option<String>,
    reason: Option<String>,
    network_host: Option<String>,
}

async fn command_answer(
    id: &Value,
    ask: Option<CommandAsk>,
    protocol: Protocol,
    method: &str,
    params: &Value,
    ctx: &ApprovalCtx<'_>,
) -> Answer {
    let Some(ask) = ask else {
        let record = deny_by_policy(
            ctx.exec,
            &call_id(params),
            SHELL_TOOL,
            method,
            "the approval request names no command, so rozu denies it by default",
        );
        let reply = decision_reply(
            protocol,
            &Decision::Deny {
                reason: "no command".into(),
            },
            &DecisionSource::Policy,
        );
        return Answer {
            reply: response(id, reply),
            records: vec![record],
        };
    };
    let (request, ask_user) = command_request(&ask, ctx);
    let (decision, source, records) = decide(ctx, &request, &Shell, ask_user).await;
    Answer {
        reply: response(id, decision_reply(protocol, &decision, &source)),
        records,
    }
}

/// The approval request for a command Codex wants to run, and whether it must go to the user
/// whatever the mode says (an escalation).
fn command_request(ask: &CommandAsk, ctx: &ApprovalCtx<'_>) -> (ApprovalRequest, bool) {
    let script = unwrap_shell(&ask.command);
    let mut args = json!({ "command": script });
    let mut outside_cwd = false;
    if let Some(cwd) = ask.cwd.as_deref() {
        match ctx.workspace.resolve(cwd) {
            Ok(path) if path != ctx.workspace.root() => {
                let rel = ctx.workspace.display_relative(&path);
                args["cwd"] = Value::String(rel);
            }
            Ok(_) => {}
            Err(_) => outside_cwd = true,
        }
    }
    // No rozu sandbox or temp dir here: Codex runs the command, under its own sandbox.
    let tool_ctx = ToolCtx::new(ctx.workspace.clone(), ctx.cancel.clone());
    let mut risk = Shell.risk(&args, &tool_ctx);
    let outside = Shell.outside_writes(&args, &tool_ctx);
    let network = ask.network_host.is_some();
    if network {
        risk = risk.max(Risk::Network);
    }
    if outside_cwd {
        risk = risk.max(Risk::WriteOutside);
    }
    let escalation = network || outside_cwd || ask.reason.is_some();

    let mut preview = format!("$ {script}");
    if script != ask.command {
        preview.push_str(&format!("\n(codex runs: {})", ask.command));
    }
    if let Some(cwd) = &ask.cwd {
        preview.push_str(&format!("\n(in {cwd})"));
    }
    if let Some(host) = &ask.network_host {
        preview.push_str(&format!("\nCodex asks for network access to {host}"));
    }
    if let Some(reason) = &ask.reason {
        preview.push_str(&format!("\nCodex says: {reason}"));
    }
    let allow_rule = if escalation {
        None
    } else {
        ctx.exec.policy.allow_rule(&Shell, &args, risk)
    };
    let request = ApprovalRequest {
        call_id: ask.call_id.clone(),
        tool: SHELL_TOOL.to_owned(),
        args,
        risk,
        preview: clip(preview),
        sandbox: SandboxMode::Enforced(CODEX_SANDBOX),
        network,
        extra_writes: Vec::new(),
        // Codex's sandbox will block writes outside the workspace; it may then ask again.
        outside_unknown: outside.unknown || !outside.paths.is_empty(),
        blocked_retry: false,
        allow_rule,
    };
    (request, escalation)
}

/// One file in a Codex patch.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Change {
    path: String,
    /// `add`, `delete` or `update`.
    kind: String,
    move_path: Option<String>,
    /// The diff (or, for legacy adds and deletes, the content).
    diff: String,
}

struct PatchAsk {
    call_id: String,
    changes: Vec<Change>,
    grant_root: Option<String>,
    reason: Option<String>,
}

fn changes_from_item(item: &Value) -> Vec<Change> {
    item["changes"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|c| Change {
            path: c["path"].as_str().unwrap_or_default().to_owned(),
            kind: c["kind"]["type"].as_str().unwrap_or("update").to_owned(),
            move_path: non_empty(&c["kind"]["move_path"]),
            diff: c["diff"].as_str().unwrap_or_default().to_owned(),
        })
        .collect()
}

fn changes_from_legacy(files: &serde_json::Map<String, Value>) -> Vec<Change> {
    files
        .iter()
        .map(|(path, change)| Change {
            path: path.clone(),
            kind: change["type"].as_str().unwrap_or("update").to_owned(),
            move_path: non_empty(&change["move_path"]),
            diff: change["unified_diff"]
                .as_str()
                .or_else(|| change["content"].as_str())
                .unwrap_or_default()
                .to_owned(),
        })
        .collect()
}

async fn patch_answer(
    id: &Value,
    ask: Option<PatchAsk>,
    protocol: Protocol,
    method: &str,
    ctx: &ApprovalCtx<'_>,
) -> Answer {
    let denied = |call_id: &str, preview: &str, reason: String| {
        let record = deny_by_policy(ctx.exec, call_id, PATCH_TOOL, preview, &reason);
        Answer {
            reply: response(
                id,
                decision_reply(
                    protocol,
                    &Decision::Deny { reason },
                    &DecisionSource::Policy,
                ),
            ),
            records: vec![record],
        }
    };
    let Some(ask) = ask.filter(|ask| !ask.changes.is_empty()) else {
        return denied(
            "codex",
            method,
            "rozu doesn't know which files this change touches, so it is denied by default".into(),
        );
    };
    let preview = patch_preview(&ask);
    let mut risk = Risk::WriteWorkspace;
    for change in &ask.changes {
        let paths = std::iter::once(change.path.as_str()).chain(change.move_path.as_deref());
        for path in paths {
            match ctx.workspace.resolve_for_write(path) {
                Ok(resolved) if ctx.workspace.is_protected(Path::new(&resolved)) => {
                    return denied(
                        &ask.call_id,
                        &preview,
                        format!(
                            "{path} is inside {}, which only the user may change",
                            rozu_tools::PROTECTED_DIR
                        ),
                    );
                }
                Ok(_) => {}
                Err(_) => {
                    risk = risk.max(Risk::WriteOutside);
                }
            }
        }
        if change.kind == "delete" {
            risk = risk.max(Risk::Destructive);
        }
    }
    let ask_user = ask.grant_root.is_some();
    if ask_user {
        risk = risk.max(Risk::WriteOutside);
    }
    let request = ApprovalRequest {
        call_id: ask.call_id.clone(),
        tool: PATCH_TOOL.to_owned(),
        args: json!({ "files": ask.changes.iter().map(|c| &c.path).collect::<Vec<_>>() }),
        risk,
        preview,
        // Codex writes the files itself; rozu has checked every path above.
        sandbox: SandboxMode::InProcess,
        network: false,
        extra_writes: Vec::new(),
        outside_unknown: false,
        blocked_retry: false,
        allow_rule: None,
    };
    let (decision, source, records) = decide(ctx, &request, &NoRules, ask_user).await;
    Answer {
        reply: response(id, decision_reply(protocol, &decision, &source)),
        records,
    }
}

fn patch_preview(ask: &PatchAsk) -> String {
    let mut text = String::new();
    for change in &ask.changes {
        let target = match &change.move_path {
            Some(to) => format!("{} → {to}", change.path),
            None => change.path.clone(),
        };
        text.push_str(&format!("*** {} {target}\n", change.kind));
        text.push_str(&change.diff);
        if !change.diff.ends_with('\n') {
            text.push('\n');
        }
    }
    if let Some(root) = &ask.grant_root {
        text.push_str(&format!(
            "Codex also asks to write anywhere under {root} for the rest of the session; rozu \
             approves this change only\n"
        ));
    }
    if let Some(reason) = &ask.reason {
        text.push_str(&format!("Codex says: {reason}\n"));
    }
    clip(text.trim_end().to_owned())
}

async fn permissions_answer(id: &Value, params: &Value, ctx: &ApprovalCtx<'_>) -> Answer {
    let permissions = &params["permissions"];
    let network = permissions["network"]["enabled"].as_bool() == Some(true);
    let fs = &permissions["fileSystem"];
    let writes = fs["write"].as_array().is_some_and(|w| !w.is_empty())
        || fs["entries"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|e| e["access"] == "write");
    let risk = if network {
        Risk::Network
    } else if writes {
        Risk::WriteOutside
    } else {
        Risk::Read
    };
    let mut preview = format!(
        "Codex asks for more permissions for this turn:\n{}",
        serde_json::to_string_pretty(permissions).unwrap_or_default()
    );
    if let Some(reason) = non_empty(&params["reason"]) {
        preview.push_str(&format!("\nCodex says: {reason}"));
    }
    let request = ApprovalRequest {
        call_id: call_id(params),
        tool: "codex:permissions".to_owned(),
        args: permissions.clone(),
        risk,
        preview: clip(preview),
        sandbox: SandboxMode::Enforced(CODEX_SANDBOX),
        network,
        extra_writes: Vec::new(),
        outside_unknown: false,
        blocked_retry: false,
        allow_rule: None,
    };
    // Escalations are never granted automatically.
    let (decision, _, records) = decide(ctx, &request, &NoRules, true).await;
    let granted = if decision.is_approval() {
        permissions.clone()
    } else {
        json!({})
    };
    Answer {
        reply: response(id, json!({ "permissions": granted, "scope": "turn" })),
        records,
    }
}

/// `_meta` kind Codex puts on its MCP tool-call approval prompts.
const MCP_TOOL_CALL_KIND: &str = "mcp_tool_call";

/// Accepts Codex's approval prompt for one of rozu's own MCP tools (`rozu mcp-serve` decides
/// and logs the call itself when it runs, so this is no bypass); declines every other
/// elicitation.
fn elicitation_answer(id: &Value, params: &Value, ctx: &ApprovalCtx<'_>) -> Answer {
    let call_id = format!("elicitation-{}", id_text(id));
    match rozu_tool_approval(params, ctx) {
        Ok((tool, args)) => {
            let tool_ctx = ToolCtx::new(ctx.workspace.clone(), ctx.cancel.clone());
            let risk = tool.risk(&args, &tool_ctx);
            let sandbox = ctx
                .exec
                .policy
                .confinement(
                    tool.as_ref(),
                    false,
                    &OutsideWrites::default(),
                    ctx.exec.sandbox.as_ref(),
                )
                .mode;
            let request = ApprovalRequest {
                call_id,
                tool: format!("{SERVER_NAME}:{}", tool.name()),
                args,
                risk,
                preview: clip(format!(
                    "Codex asks to run rozu's MCP tool `{}`; rozu mcp-serve decides the call \
                     itself",
                    tool.name()
                )),
                sandbox,
                network: false,
                extra_writes: Vec::new(),
                outside_unknown: false,
                blocked_retry: false,
                allow_rule: None,
            };
            let record =
                ctx.exec
                    .record_decision(&request, &Decision::Approve, &DecisionSource::Delegated);
            Answer {
                reply: response(id, json!({ "action": "accept", "content": {} })),
                records: vec![record],
            }
        }
        Err(why) => {
            let server = params["serverName"].as_str().unwrap_or("?");
            let message = params["message"].as_str().unwrap_or("");
            let record = deny_by_policy(
                ctx.exec,
                &call_id,
                "codex:mcp_elicitation",
                &format!("MCP server `{server}` asks: {message}"),
                &format!("rozu answers no other MCP elicitations (default-deny): {why}"),
            );
            Answer {
                reply: response(id, json!({ "action": "decline" })),
                records: vec![record],
            }
        }
    }
}

/// The rozu tool (and its arguments, when Codex sends them) if `params` is exactly Codex's
/// tool-call approval prompt for the `rozu` MCP server rozu configured; otherwise why not.
fn rozu_tool_approval(
    params: &Value,
    ctx: &ApprovalCtx<'_>,
) -> Result<(Arc<dyn Tool>, Value), &'static str> {
    if params["serverName"].as_str() != Some(SERVER_NAME) {
        return Err("not rozu's MCP server");
    }
    if params["mode"].as_str() != Some("form") {
        return Err("not a form elicitation");
    }
    // Accepting answers with empty content, so the form must ask for nothing.
    let schema = &params["requestedSchema"];
    let empty_form = schema["type"] == "object"
        && schema["properties"]
            .as_object()
            .is_some_and(|p| p.is_empty())
        && match &schema["required"] {
            Value::Null => true,
            Value::Array(required) => required.is_empty(),
            _ => false,
        };
    if !empty_form {
        return Err("the form asks for input");
    }
    let meta = &params["_meta"];
    match &meta["codex_approval_kind"] {
        Value::Null => {}
        Value::String(kind) if kind == MCP_TOOL_CALL_KIND => {}
        _ => return Err("not a tool-call approval"),
    }
    let message = params["message"].as_str().ok_or("no message")?;
    let name = message
        .strip_prefix(&format!(
            "Allow the {SERVER_NAME} MCP server to run tool \""
        ))
        .and_then(|rest| rest.strip_suffix("\"?"))
        .filter(|name| !name.contains('"'))
        .ok_or("not a tool-call approval")?;
    let tool = ctx.exec.tools.get(name).ok_or("not one of rozu's tools")?;
    let args = match &meta["tool_params"] {
        args @ Value::Object(_) => args.clone(),
        _ => Value::Null,
    };
    Ok((tool.clone(), args))
}

/// Runs the decision (policy then user, or the user only) and collects its log entries.
async fn decide(
    ctx: &ApprovalCtx<'_>,
    request: &ApprovalRequest,
    rule_tool: &dyn Tool,
    ask_user: bool,
) -> (Decision, DecisionSource, Vec<ApprovalRecord>) {
    let (tx, mut rx) = mpsc::channel(4);
    let (decision, source) = if ask_user {
        ctx.exec.ask_user(request, ctx.cancel, &tx).await
    } else {
        ctx.exec.decide(request, rule_tool, ctx.cancel, &tx).await
    };
    drop(tx);
    let mut records = Vec::new();
    while let Some(event) = rx.recv().await {
        if let AgentEvent::Approval(record) = event {
            records.push(record);
        }
    }
    (decision, source, records)
}

/// Logs a request denied without asking (default-deny) and returns its log entry.
fn deny_by_policy(
    exec: &ToolExecutor,
    call_id: &str,
    tool: &str,
    preview: &str,
    reason: &str,
) -> ApprovalRecord {
    let request = ApprovalRequest {
        call_id: call_id.to_owned(),
        tool: tool.to_owned(),
        args: Value::Null,
        risk: Risk::Destructive,
        preview: clip(preview.to_owned()),
        sandbox: SandboxMode::Off,
        network: false,
        extra_writes: Vec::new(),
        outside_unknown: false,
        blocked_retry: false,
        allow_rule: None,
    };
    exec.record_decision(
        &request,
        &Decision::Deny {
            reason: reason.to_owned(),
        },
        &DecisionSource::Policy,
    )
}

fn item<'a>(ctx: &ApprovalCtx<'a>, params: &Value) -> Option<&'a Value> {
    ctx.mapper?.item(params["itemId"].as_str()?)
}

/// The v2 approval's id: the item, plus the callback id when one item has several.
fn call_id(params: &Value) -> String {
    let item = params["itemId"].as_str().unwrap_or("codex");
    match params["approvalId"].as_str() {
        Some(approval) => format!("{item}/{approval}"),
        None => item.to_owned(),
    }
}

fn id_text(id: &Value) -> String {
    id.as_str().map_or_else(|| id.to_string(), str::to_owned)
}

fn non_empty(value: &Value) -> Option<String> {
    value
        .as_str()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
}

fn clip(mut text: String) -> String {
    if text.chars().count() > MAX_PREVIEW_CHARS {
        text = text.chars().take(MAX_PREVIEW_CHARS).collect();
        text.push_str("\n…");
    }
    text
}

/// Shell names Codex wraps commands in.
const SHELLS: &[&str] = &["sh", "bash", "zsh"];

/// The script inside Codex's shell wrapper (`/bin/zsh -lc 'cargo test'` → `cargo test`), so
/// the classifier sees what will actually run; anything else is returned unchanged.
pub fn unwrap_shell(command: &str) -> String {
    let trimmed = command.trim();
    let Some((program, rest)) = trimmed.split_once(' ') else {
        return trimmed.to_owned();
    };
    let name = program.rsplit('/').next().unwrap_or(program);
    if !SHELLS.contains(&name) {
        return trimmed.to_owned();
    }
    let rest = rest.trim_start();
    let Some(script) = ["-lc ", "-c "]
        .iter()
        .find_map(|flag| rest.strip_prefix(flag))
    else {
        return trimmed.to_owned();
    };
    unquote(script.trim()).unwrap_or_else(|| trimmed.to_owned())
}

/// One shell word that is a single- or double-quoted string (possibly built from several
/// quoted pieces, like `'it'\''s'`), unquoted; `None` if it is anything else.
fn unquote(word: &str) -> Option<String> {
    let mut out = String::new();
    let mut chars = word.chars().peekable();
    let mut quoted = false;
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                quoted = true;
                loop {
                    match chars.next()? {
                        '\'' => break,
                        c => out.push(c),
                    }
                }
            }
            '"' => {
                quoted = true;
                loop {
                    match chars.next()? {
                        '"' => break,
                        '\\' => match chars.next()? {
                            c @ ('"' | '\\' | '$' | '`') => out.push(c),
                            '\n' => {}
                            c => {
                                out.push('\\');
                                out.push(c);
                            }
                        },
                        c => out.push(c),
                    }
                }
            }
            '\\' => out.push(chars.next()?),
            c if c.is_whitespace() => return None,
            c => out.push(c),
        }
    }
    quoted.then_some(out)
}

/// A legacy argv as one command line: the script of `[shell, -c|-lc, script]`, else the words
/// shell-quoted where needed.
fn join_argv(argv: &[Value]) -> String {
    let words: Vec<&str> = argv.iter().filter_map(Value::as_str).collect();
    if let [program, flag, script] = words.as_slice() {
        let name = program.rsplit('/').next().unwrap_or(program);
        if SHELLS.contains(&name) && matches!(*flag, "-c" | "-lc") {
            return format!("{program} {flag} {}", quote(script));
        }
    }
    words.iter().map(|w| quote(w)).collect::<Vec<_>>().join(" ")
}

fn quote(word: &str) -> String {
    if !word.is_empty()
        && word
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./=:,+@%".contains(c))
    {
        word.to_owned()
    } else {
        format!("'{}'", word.replace('\'', r"'\''"))
    }
}

/// A stand-in for Codex's own tools in rule matching: no "always allow" rules ever match.
struct NoRules;

#[async_trait]
impl Tool for NoRules {
    fn name(&self) -> &'static str {
        "codex-builtin"
    }

    fn description(&self) -> &'static str {
        "A Codex built-in tool (decided by rozu, run by Codex)."
    }

    fn schema(&self) -> Value {
        json!({ "type": "object" })
    }

    fn risk(&self, _args: &Value, _ctx: &ToolCtx) -> Risk {
        Risk::Destructive
    }

    async fn call(&self, _args: Value, _ctx: &ToolCtx) -> ToolOutput {
        ToolOutput::error("rozu does not run Codex's built-in tools")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unwraps_codex_shell_wrappers() {
        assert_eq!(unwrap_shell("/bin/zsh -lc 'cargo test'"), "cargo test");
        assert_eq!(
            unwrap_shell("bash -lc \"git push --force\""),
            "git push --force"
        );
        assert_eq!(unwrap_shell("sh -c 'echo '\\''hi'\\'''"), "echo 'hi'");
        assert_eq!(unwrap_shell("ls -la"), "ls -la");
        // Not a single quoted word: left alone, so the classifier sees `sh -c`.
        assert_eq!(unwrap_shell("bash -c foo bar"), "bash -c foo bar");
        assert_eq!(unwrap_shell("python3 -c 'x'"), "python3 -c 'x'");
    }

    #[test]
    fn joins_legacy_argv() {
        let argv = |words: &[&str]| words.iter().map(|w| json!(w)).collect::<Vec<_>>();
        assert_eq!(
            join_argv(&argv(&["bash", "-lc", "rm -rf build"])),
            "bash -lc 'rm -rf build'"
        );
        assert_eq!(
            unwrap_shell(&join_argv(&argv(&["bash", "-lc", "it's"]))),
            "it's"
        );
        assert_eq!(join_argv(&argv(&["ls", "a b"])), "ls 'a b'");
    }

    #[test]
    fn decisions_are_one_time_answers() {
        let approve = Decision::Approve;
        let always = Decision::AlwaysAllowPrefix;
        let unsandboxed = Decision::ApproveUnsandboxed;
        for d in [&approve, &always, &unsandboxed] {
            assert_eq!(
                decision_reply(Protocol::V2, d, &DecisionSource::Gate),
                json!({"decision": "accept"})
            );
            assert_eq!(
                decision_reply(Protocol::Legacy, d, &DecisionSource::Gate),
                json!({"decision": "approved"})
            );
        }
        let deny = Decision::Deny {
            reason: "no".into(),
        };
        assert_eq!(
            decision_reply(Protocol::V2, &deny, &DecisionSource::Gate),
            json!({"decision": "decline"})
        );
        assert_eq!(
            decision_reply(Protocol::V2, &deny, &DecisionSource::Cancelled),
            json!({"decision": "cancel"})
        );
        assert_eq!(
            decision_reply(Protocol::Legacy, &deny, &DecisionSource::Policy),
            json!({"decision": {"denied": {"rejection": "denied by rozu: no"}}})
        );
        assert_eq!(
            decision_reply(Protocol::Legacy, &deny, &DecisionSource::Cancelled),
            json!({"decision": "abort"})
        );
    }
}
