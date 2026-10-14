//! `rozu eval tasks`: each `evals/tasks/*.json` runs in a fresh temp repo and is checked by its
//! outcome (files, a command that must pass, the answer).

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Instant;

use anyhow::bail;
use clap::{Args, ValueEnum};
use rozu_core::{
    Agent, AgentConfig, ApprovalMode, ApprovalPolicy, ContentPart, Message, ReplayProvider, Role,
    Session, TurnOutcome, new_session_id,
};
use rozu_store::{SessionMeta, SessionStore, unix_now};
use rozu_tools::{ToolRegistry, platform_sandbox};
use serde::Deserialize;
use serde_json::Value;

use super::{EvalGate, Mode, last_answer, load_fixtures, quiet_turn, temp_repo};
use crate::approve::{ApprovalArg, CliGate};
use rozu_runtime::{AgentFactory, TERMINAL_HOST, resolve_choice, system_prompt};

#[derive(Debug, Args)]
pub struct TasksArgs {
    #[arg(long, value_enum, default_value_t = Mode::Dry)]
    mode: Mode,
    #[arg(long, default_value = "evals/tasks")]
    dir: PathBuf,
    /// For --mode live.
    #[arg(long, env = "ROZU_PROVIDER")]
    provider: Option<String>,
    /// For --mode live.
    #[arg(long, env = "ROZU_MODEL")]
    model: Option<String>,
    /// Also save each run as a session in this directory (for `rozu eval replay`).
    #[arg(long)]
    save_sessions: Option<PathBuf>,
    /// Only these tasks.
    names: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Task {
    pub name: String,
    pub prompt: String,
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    #[serde(default = "auto")]
    pub approval: String,
    /// Deny every approval prompt instead of approving it.
    #[serde(default)]
    pub deny: bool,
    /// The model's responses in dry mode.
    #[serde(default)]
    pub script: Vec<Step>,
    pub expect: Expect,
}

fn auto() -> String {
    "auto".into()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Step {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub calls: Vec<(String, Value)>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    /// Path → text it must contain.
    #[serde(default)]
    pub files: BTreeMap<String, String>,
    /// Paths that must still hold their original text.
    #[serde(default)]
    pub unchanged: Vec<String>,
    /// A shell command that must exit 0 in the repo afterwards.
    #[serde(default)]
    pub command: Option<String>,
    /// Text the final answer must contain (case-insensitive).
    #[serde(default)]
    pub answer: Vec<String>,
    /// At least this many approval prompts (dry mode).
    #[serde(default)]
    pub asked: Option<usize>,
}

impl Task {
    /// The script as assistant messages.
    pub fn steps(&self) -> Vec<Message> {
        let mut n = 0;
        self.script
            .iter()
            .map(|step| {
                let mut content = Vec::new();
                if !step.text.is_empty() {
                    content.push(ContentPart::Text {
                        text: step.text.clone(),
                    });
                }
                for (name, args) in &step.calls {
                    n += 1;
                    content.push(ContentPart::ToolCall {
                        id: format!("call_{n}"),
                        name: name.clone(),
                        args_json: args.to_string(),
                    });
                }
                Message::new(Role::Assistant, content)
            })
            .collect()
    }

    /// What's wrong with the repo / answer after a run.
    pub fn check(&self, root: &Path, session: &Session) -> Vec<String> {
        let mut failures = Vec::new();
        for (path, needle) in &self.expect.files {
            match std::fs::read_to_string(root.join(path)) {
                Ok(text) if text.contains(needle.as_str()) => {}
                Ok(_) => failures.push(format!("{path} doesn't contain {needle:?}")),
                Err(err) => failures.push(format!("{path}: {err}")),
            }
        }
        for path in &self.expect.unchanged {
            let now = std::fs::read_to_string(root.join(path)).ok();
            if now.as_ref() != self.files.get(path) {
                failures.push(format!("{path} changed"));
            }
        }
        if let Some(command) = &self.expect.command {
            match std::process::Command::new("/bin/sh")
                .args(["-c", command])
                .current_dir(root)
                .output()
            {
                Ok(out) if out.status.success() => {}
                Ok(out) => failures.push(format!(
                    "`{command}` failed: {}",
                    String::from_utf8_lossy(&out.stdout).trim()
                )),
                Err(err) => failures.push(format!("`{command}`: {err}")),
            }
        }
        let answer = last_answer(session).to_lowercase();
        for needle in &self.expect.answer {
            if !answer.contains(&needle.to_lowercase()) {
                failures.push(format!("the answer doesn't mention {needle:?}"));
            }
        }
        failures
    }
}

pub async fn run(args: TasksArgs) -> anyhow::Result<ExitCode> {
    let mut tasks: Vec<Task> = load_fixtures(&args.dir, "task")?;
    if !args.names.is_empty() {
        tasks.retain(|t| args.names.contains(&t.name));
    }
    if tasks.is_empty() {
        bail!("no tasks to run in {}", args.dir.display());
    }
    let choice = match args.mode {
        Mode::Dry => None,
        Mode::Live => Some(resolve_choice(
            args.provider.clone(),
            args.model.as_deref(),
        )?),
    };
    let sessions = args
        .save_sessions
        .as_ref()
        .map(SessionStore::open)
        .transpose()?;
    let mut failed = 0;
    for task in &tasks {
        let started = Instant::now();
        let result = run_task(task, choice.as_ref(), sessions.as_ref()).await;
        let ms = started.elapsed().as_millis();
        match result {
            Ok(failures) if failures.is_empty() => println!("PASS {} ({ms} ms)", task.name),
            Ok(failures) => {
                failed += 1;
                println!("FAIL {} ({ms} ms)", task.name);
                for failure in failures {
                    println!("  - {failure}");
                }
            }
            Err(err) => {
                failed += 1;
                println!("FAIL {}: {err:#}", task.name);
            }
        }
    }
    println!("{} of {} tasks passed", tasks.len() - failed, tasks.len());
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

async fn run_task(
    task: &Task,
    choice: Option<&rozu_providers::ModelChoice>,
    sessions: Option<&SessionStore>,
) -> anyhow::Result<Vec<String>> {
    let (dir, workspace) = temp_repo(&task.files)?;
    let approval = ApprovalArg::from_str(&task.approval, true)
        .map_err(|_| anyhow::anyhow!("unknown approval {:?}", task.approval))?;
    let gate = EvalGate::new(task.deny);
    let replay = Arc::new(ReplayProvider::new(task.steps()));
    let agent = match choice {
        None => {
            let policy =
                ApprovalPolicy::for_project(ApprovalMode::from(approval), workspace.root())?;
            Agent::new(
                replay.clone(),
                Arc::new(ToolRegistry::standard()),
                workspace.clone(),
                AgentConfig::new("replay"),
            )
            .with_approval_policy(Arc::new(policy))
            .with_approval_gate(gate.clone())
            .with_sandbox(platform_sandbox())
        }
        Some(choice) => {
            // Prompts are denied without asking; full-access live runs rarely need one.
            let (cli_gate, _asks) = CliGate::new(false);
            AgentFactory::new(&workspace, approval.into(), cli_gate, new_session_id())?
                .build(choice)?
        }
    };
    let mut session = Session::with_system_prompt(system_prompt(
        workspace.root(),
        approval.into(),
        TERMINAL_HOST,
    ));
    let (outcome, _) = quiet_turn(&agent, &mut session, &task.prompt).await?;
    if let Some(store) = sessions {
        let meta = SessionMeta {
            id: format!("{}-{}", task.name, new_session_id()),
            workspace: workspace.root().to_owned(),
            provider: choice.map_or("replay", |c| c.provider.id).to_owned(),
            model: choice.map_or("replay".into(), |c| c.model.clone()),
            created: unix_now(),
        };
        store.create(meta)?.sync(&session)?;
    }
    let mut failures = task.check(dir.path(), &session);
    if outcome != TurnOutcome::Completed {
        failures.push(format!("the turn ended with {outcome:?}"));
    }
    if let Some(want) = task.expect.asked.filter(|_| choice.is_none()) {
        if gate.asked() < want {
            failures.push(format!(
                "{} approval prompts, expected at least {want}",
                gate.asked()
            ));
        }
    }
    if choice.is_none() && replay.remaining() > 0 {
        failures.push(format!(
            "{} scripted steps were never used",
            replay.remaining()
        ));
    }
    Ok(failures)
}
