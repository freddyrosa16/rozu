//! `rozu mcp-serve` (hidden): rozu's stdio MCP server, started by a vendor CLI from the MCP
//! config rozu gives it (Claude's `--mcp-config`, Codex's `-c mcp_servers.rozu.*`). stdout carries only the protocol; logs go to the log file.
//!
//! Tool calls and permission prompts are decided by the project's approval policy; prompts go to
//! the parent `rozu chat` over the socket in `ROZU_APPROVAL_SOCKET`, and are denied when it
//! can't be reached. Every decision made here is appended to the approval audit log under the
//! parent's session id.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::Args;
use rozu_core::{ApprovalMode, ApprovalPolicy, AuditLog, ToolExecutor, new_session_id};
use rozu_mcp::{McpServer, SocketGate};
use rozu_tools::{ToolRegistry, Workspace, platform_sandbox};

use crate::approve::ApprovalArg;

#[derive(Debug, Args)]
pub struct McpServeArgs {
    /// Workspace the tools work in.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// Approval mode, as for `rozu chat`.
    #[arg(long, value_enum, default_value_t = ApprovalArg::Auto)]
    approval: ApprovalArg,
    /// The parent `rozu chat`'s session id, so both processes' audit entries match.
    #[arg(long)]
    session_id: Option<String>,
    /// The CLI-backed provider that started this server (e.g. `codex`), recorded in the audit
    /// log.
    #[arg(long)]
    provider: Option<String>,
}

pub fn run(args: McpServeArgs) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Runtime::new().context("could not start the async runtime")?;
    runtime.block_on(serve(args))
}

async fn serve(args: McpServeArgs) -> anyhow::Result<ExitCode> {
    let workspace = Workspace::new(&args.workspace)
        .with_context(|| format!("cannot open workspace {}", args.workspace.display()))?;
    let mode = ApprovalMode::from(args.approval);
    let policy = ApprovalPolicy::for_project(mode, workspace.root())
        .context("cannot load this user's project approval rules")?;
    let gate = SocketGate::from_env();
    if gate.path().is_none() {
        tracing::warn!(
            "{} is not set; every call that needs approval will be denied",
            rozu_mcp::APPROVAL_SOCKET_ENV
        );
    }
    let session_id = args.session_id.unwrap_or_else(new_session_id);
    let exec = ToolExecutor::new(
        Arc::new(ToolRegistry::standard()),
        Arc::new(gate.clone()),
        Arc::new(policy),
        platform_sandbox(),
    )
    .with_audit(AuditLog::from_env(session_id).map(Arc::new))
    .with_provider(args.provider);
    let server = Arc::new(McpServer::new(exec, workspace, gate));
    server
        .serve(tokio::io::stdin(), tokio::io::stdout())
        .await
        .context("MCP session failed")?;
    Ok(ExitCode::SUCCESS)
}
