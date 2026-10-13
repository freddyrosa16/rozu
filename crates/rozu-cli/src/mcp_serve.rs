//! `rozu mcp-serve` (hidden): rozu's stdio MCP server, started by a vendor CLI from the
//! `--mcp-config` rozu writes for it. stdout carries only the protocol; logs go to the log file.
//!
//! Tool calls and permission prompts are decided by the project's approval policy; prompts go to
//! the parent `rozu chat` over the socket in `ROZU_APPROVAL_SOCKET`, and are denied when it
//! can't be reached.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context;
use clap::Args;
use rozu_core::{ApprovalMode, ApprovalPolicy, ToolExecutor, approvals_path};
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
}

pub fn run(args: McpServeArgs) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Runtime::new().context("could not start the async runtime")?;
    runtime.block_on(serve(args))
}

async fn serve(args: McpServeArgs) -> anyhow::Result<ExitCode> {
    let workspace = Workspace::new(&args.workspace)
        .with_context(|| format!("cannot open workspace {}", args.workspace.display()))?;
    let mode = ApprovalMode::from(args.approval);
    let policy = ApprovalPolicy::for_project(mode, workspace.root()).with_context(|| {
        format!(
            "cannot read the approval rules in {}",
            approvals_path(workspace.root()).display()
        )
    })?;
    let gate = SocketGate::from_env();
    if gate.path().is_none() {
        tracing::warn!(
            "{} is not set; every call that needs approval will be denied",
            rozu_mcp::APPROVAL_SOCKET_ENV
        );
    }
    let exec = ToolExecutor::new(
        Arc::new(ToolRegistry::standard()),
        Arc::new(gate.clone()),
        Arc::new(policy),
        platform_sandbox(),
    );
    let server = Arc::new(McpServer::new(exec, workspace, gate));
    server
        .serve(tokio::io::stdin(), tokio::io::stdout())
        .await
        .context("MCP session failed")?;
    Ok(ExitCode::SUCCESS)
}
