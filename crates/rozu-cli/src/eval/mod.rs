//! `rozu eval`: scripted tasks, recorded-session replays and the memory eval. `--mode dry`
//! (the default, and what CI runs) scripts the model; `--mode live` uses a real provider.

mod memory;
mod replay;
mod tasks;

use std::collections::BTreeMap;
use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use anyhow::Context;
use async_trait::async_trait;
use clap::{Args, Subcommand, ValueEnum};
use rozu_core::{AgentEvent, ApprovalGate, ApprovalRequest, Decision, Session, TurnOutcome, Usage};
use rozu_tools::Workspace;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Args)]
pub struct EvalArgs {
    #[command(subcommand)]
    command: EvalCommand,
}

#[derive(Debug, Subcommand)]
enum EvalCommand {
    /// Run the scripted tasks in evals/tasks, each in a fresh temp repo, and check the outcome.
    Tasks(tasks::TasksArgs),
    /// Replay a recorded session against a copy of its workspace and compare the tool results.
    Replay(replay::ReplayArgs),
    /// Replay the scripted conversations in evals/memory under each memory condition and log
    /// tokens, cache hits, cost, latency and recall per run.
    Memory(memory::MemoryArgs),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Scripted model responses; no provider, no network.
    Dry,
    /// A real provider (--provider / --model, as for `rozu chat`).
    Live,
}

pub fn run(args: EvalArgs) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Runtime::new().context("could not start the async runtime")?;
    runtime.block_on(async {
        match args.command {
            EvalCommand::Tasks(args) => tasks::run(args).await,
            EvalCommand::Replay(args) => replay::run(args).await,
            EvalCommand::Memory(args) => memory::run(args).await,
        }
    })
}

/// Answers every approval prompt the same way, counting them.
#[derive(Debug, Default)]
pub struct EvalGate {
    deny: bool,
    asked: AtomicUsize,
}

impl EvalGate {
    pub fn new(deny: bool) -> Arc<Self> {
        Arc::new(Self {
            deny,
            asked: AtomicUsize::new(0),
        })
    }

    pub fn asked(&self) -> usize {
        self.asked.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ApprovalGate for EvalGate {
    fn asks_user(&self) -> bool {
        false
    }

    async fn decide(&self, _: ApprovalRequest) -> Decision {
        self.asked.fetch_add(1, Ordering::SeqCst);
        if self.deny {
            Decision::Deny {
                reason: "denied by the eval".into(),
            }
        } else {
            Decision::Approve
        }
    }
}

/// A temp workspace holding `files` (relative paths).
pub fn temp_repo(
    files: &BTreeMap<String, String>,
) -> anyhow::Result<(tempfile::TempDir, Workspace)> {
    let dir = tempfile::tempdir()?;
    for (path, text) in files {
        let path = dir.path().join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(path, text)?;
    }
    let workspace = Workspace::new(dir.path())?;
    Ok((dir, workspace))
}

/// Copies the files under `from` (skipping `.git` and `target`) into `to`.
pub fn copy_dir(from: &Path, to: &Path) -> std::io::Result<()> {
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let name = entry.file_name();
        if name == ".git" || name == "target" {
            continue;
        }
        let dest = to.join(&name);
        if entry.file_type()?.is_dir() {
            std::fs::create_dir_all(&dest)?;
            copy_dir(&entry.path(), &dest)?;
        } else {
            std::fs::copy(entry.path(), dest)?;
        }
    }
    Ok(())
}

/// Runs one turn with no output, returning its outcome and how many model requests it made.
pub async fn quiet_turn(
    agent: &rozu_core::Agent,
    session: &mut Session,
    input: &str,
) -> Result<(TurnOutcome, usize), rozu_core::AgentError> {
    let (tx, mut rx) = mpsc::channel(256);
    let drain = tokio::spawn(async move {
        let mut usages = 0;
        while let Some(event) = rx.recv().await {
            usages += usize::from(matches!(event, AgentEvent::Usage(_)));
        }
        usages
    });
    let result = agent
        .run_turn(input, session, &tx, CancellationToken::new())
        .await;
    drop(tx);
    let usages = drain.await.unwrap_or_default();
    result.map(|outcome| (outcome, usages))
}

/// The last assistant text of the session.
pub fn last_answer(session: &Session) -> String {
    session
        .messages
        .iter()
        .rev()
        .find(|m| m.role == rozu_core::Role::Assistant && !m.text_content().is_empty())
        .map(|m| m.text_content())
        .unwrap_or_default()
}

pub fn cache_hit_rate(usage: &Usage) -> Option<f64> {
    (usage.input_tokens > 0).then(|| usage.cache_read_tokens as f64 / usage.input_tokens as f64)
}
