//! `rozu eval replay`: plays a recorded session's model responses back through the real agent
//! loop and tools, in a copy of its workspace, and checks every tool result matches.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{Context, bail};
use clap::Args;
use rozu_core::{
    Agent, AgentConfig, ApprovalMode, ApprovalPolicy, ContentPart, ReplayProvider, Role, Session,
};
use rozu_store::SessionStore;
use rozu_tools::{ToolRegistry, Workspace, platform_sandbox};

use super::{EvalGate, copy_dir, quiet_turn};

#[derive(Debug, Args)]
pub struct ReplayArgs {
    /// A session file (`.jsonl`), or an id from `rozu sessions`.
    session: String,
    /// The files the session started from (copied to a temp dir first). Defaults to the
    /// session's own workspace.
    #[arg(long)]
    workspace: Option<PathBuf>,
}

pub async fn run(args: ReplayArgs) -> anyhow::Result<ExitCode> {
    let path = PathBuf::from(&args.session);
    let (meta, recorded) = if path.extension().is_some_and(|e| e == "jsonl") {
        SessionStore::load_file(&path)
    } else {
        let store = SessionStore::open_default()?;
        let id = store.resolve(Some(&args.session), std::path::Path::new(""))?;
        store.load(&id)
    }
    .with_context(|| format!("cannot load {}", args.session))?;
    if recorded.compactions > 0 {
        bail!(
            "the session was compacted, so its early tool results are gone; it can't be replayed"
        );
    }
    let source = args.workspace.unwrap_or_else(|| meta.workspace.clone());
    let dir = tempfile::tempdir()?;
    copy_dir(&source, dir.path()).with_context(|| format!("cannot copy {}", source.display()))?;
    let workspace = Workspace::new(dir.path())?;

    let provider = Arc::new(ReplayProvider::from_session(&recorded.messages));
    let policy = ApprovalPolicy::for_project(ApprovalMode::AutoWorkspace, workspace.root())?;
    let agent = Agent::new(
        provider.clone(),
        Arc::new(ToolRegistry::standard()),
        workspace.clone(),
        AgentConfig::new("replay"),
    )
    .with_approval_policy(Arc::new(policy))
    .with_approval_gate(EvalGate::new(false))
    .with_sandbox(platform_sandbox());

    let mut session = Session::new();
    session.messages.extend(
        recorded
            .messages
            .iter()
            .take_while(|m| m.role == Role::System)
            .cloned(),
    );
    for user in recorded.messages.iter().filter(|m| m.role == Role::User) {
        quiet_turn(&agent, &mut session, &user.text_content()).await?;
    }

    let normalize = |text: &str, root: &std::path::Path| {
        text.replace(&root.display().to_string(), "<workspace>")
    };
    let expected = tool_results(&recorded);
    let actual = tool_results(&session);
    let mut mismatches = 0;
    for (i, (want, got)) in expected.iter().zip(&actual).enumerate() {
        if normalize(want, &meta.workspace) != normalize(got, workspace.root()) {
            mismatches += 1;
            println!(
                "tool result {} differs:\n--- recorded\n{want}\n--- replayed\n{got}",
                i + 1
            );
        }
    }
    if expected.len() != actual.len() {
        mismatches += 1;
        println!(
            "the recording has {} tool results, the replay {}",
            expected.len(),
            actual.len()
        );
    }
    if mismatches == 0 {
        println!(
            "replayed {} model responses and {} tool calls; every result matches",
            provider.requests().len(),
            actual.len()
        );
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

fn tool_results(session: &Session) -> Vec<String> {
    session
        .messages
        .iter()
        .flat_map(|m| &m.content)
        .filter_map(|part| match part {
            ContentPart::ToolResult { output, .. } => Some(output.clone()),
            _ => None,
        })
        .collect()
}
