//! `rozu chat`: run the agent against OpenAI with the read-only workspace tools.
//!
//! Assistant text streams to stdout; tool calls, status and errors go to stderr (dimmed on a
//! terminal), so `rozu chat "…" > answer.md` captures just the answer.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use clap::Args;
use rozu_core::{
    Agent, AgentConfig, AgentError, AgentEvent, DEFAULT_MAX_STEPS, ModelInfo, Session, TurnOutcome,
    Usage,
};
use rozu_providers::openai::{self, OpenAiConfig, OpenAiProvider};
use rozu_secrets::{KeyringStore, resolve_api_key};
use rozu_tools::{ToolRegistry, Workspace};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

pub const DEFAULT_MODEL: &str = "gpt-5.1";

#[derive(Debug, Args)]
pub struct ChatArgs {
    /// Model id.
    #[arg(long, default_value = DEFAULT_MODEL)]
    model: String,
    /// Directory the agent's tools may read. Paths outside it are rejected.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// Maximum model requests per turn.
    #[arg(long, default_value_t = DEFAULT_MAX_STEPS)]
    max_steps: usize,
    /// Prompt to run once. Without it, rozu starts an interactive session.
    #[arg(trailing_var_arg = true)]
    prompt: Vec<String>,
}

pub fn run(args: ChatArgs) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Runtime::new().context("could not start the async runtime")?;
    runtime.block_on(chat(args))
}

async fn chat(args: ChatArgs) -> anyhow::Result<ExitCode> {
    let store = KeyringStore::default();
    let (key, _source) =
        resolve_api_key(&store, openai::PROVIDER_ID, |var| std::env::var(var).ok())?.ok_or_else(
            || anyhow!("no OpenAI API key: run `rozu auth set openai` or set OPENAI_API_KEY"),
        )?;
    let mut config = OpenAiConfig::new(key);
    if let Some(base_url) = std::env::var("OPENAI_BASE_URL")
        .ok()
        .filter(|url| !url.trim().is_empty())
    {
        config = config.with_base_url(base_url);
    }
    let provider = Arc::new(OpenAiProvider::new(config)?);

    let workspace = Workspace::new(&args.workspace)
        .with_context(|| format!("cannot open workspace {}", args.workspace.display()))?;
    let mut agent_config = AgentConfig::new(&args.model);
    agent_config.max_steps = args.max_steps;
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::read_only()),
        workspace.clone(),
        agent_config,
    );
    let mut session = Session::with_system_prompt(system_prompt(&workspace));
    let printer = Printer::new(openai::model_info(&args.model));
    let interrupts = Interrupts::install();

    let prompt = args.prompt.join(" ");
    if !prompt.trim().is_empty() {
        let outcome = turn(&agent, &mut session, prompt, &interrupts, &printer).await?;
        return Ok(match outcome {
            TurnOutcome::Cancelled => ExitCode::from(130),
            _ => ExitCode::SUCCESS,
        });
    }

    let interactive = io::stdin().is_terminal();
    if interactive {
        printer.status(&format!(
            "rozu {} · {} · {}\nCtrl-C cancels a turn (twice exits) · Ctrl-D or /exit quits",
            env!("CARGO_PKG_VERSION"),
            args.model,
            workspace.root().display()
        ));
    }
    loop {
        if interactive {
            eprint!("\n> ");
            let _ = io::stderr().flush();
        }
        let Some(line) = read_line().await? else {
            break;
        };
        let input = line.trim();
        if input.is_empty() {
            continue;
        }
        if matches!(input, "/exit" | "/quit") {
            break;
        }
        if let Err(err) = turn(
            &agent,
            &mut session,
            input.to_owned(),
            &interrupts,
            &printer,
        )
        .await
        {
            eprintln!("error: {err}");
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn system_prompt(workspace: &Workspace) -> String {
    format!(
        "You are rozu, a coding agent running in the user's terminal.\n\
         The workspace root is {}. Use the read_file, list_dir and grep tools to look at files; \
         paths are relative to the workspace root. You cannot modify files or run commands yet.\n\
         Be concise, and cite file paths when you refer to code.",
        workspace.root().display()
    )
}

/// Reads one line from stdin without blocking the runtime. `None` at end of input.
async fn read_line() -> anyhow::Result<Option<String>> {
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        let read = io::stdin().read_line(&mut line)?;
        Ok::<_, io::Error>((read > 0).then_some(line))
    })
    .await?
    .context("could not read from stdin")
}

/// Runs one turn while printing its events.
async fn turn(
    agent: &Agent,
    session: &mut Session,
    input: String,
    interrupts: &Interrupts,
    printer: &Printer,
) -> Result<TurnOutcome, AgentError> {
    let cancel = interrupts.begin_turn();
    let (tx, mut rx) = mpsc::channel(256);
    let mut state = printer.start_turn();
    let render = async move {
        while let Some(event) = rx.recv().await {
            state.handle(event);
        }
        state.finish();
    };
    let run = async {
        let result = agent.run_turn(input, session, &tx, cancel).await;
        drop(tx);
        result
    };
    let (result, ()) = tokio::join!(run, render);
    interrupts.end_turn();
    result
}

/// First Ctrl-C cancels the running turn; a second one (or one at the prompt) exits.
#[derive(Clone, Default)]
struct Interrupts {
    current: Arc<Mutex<Option<CancellationToken>>>,
}

impl Interrupts {
    fn install() -> Self {
        let this = Self::default();
        let current = this.current.clone();
        tokio::spawn(async move {
            while tokio::signal::ctrl_c().await.is_ok() {
                let active = current.lock().expect("interrupt lock").clone();
                match active {
                    Some(token) if !token.is_cancelled() => {
                        token.cancel();
                        eprintln!("\n^C cancelling; press Ctrl-C again to exit");
                    }
                    _ => {
                        eprintln!();
                        std::process::exit(130);
                    }
                }
            }
        });
        this
    }

    fn begin_turn(&self) -> CancellationToken {
        let token = CancellationToken::new();
        *self.current.lock().expect("interrupt lock") = Some(token.clone());
        token
    }

    fn end_turn(&self) {
        *self.current.lock().expect("interrupt lock") = None;
    }
}

/// Output styling shared by all turns.
struct Printer {
    model: Option<ModelInfo>,
    dim: bool,
}

impl Printer {
    fn new(model: Option<ModelInfo>) -> Self {
        let dim = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self { model, dim }
    }

    fn status(&self, text: &str) {
        if self.dim {
            eprintln!("\x1b[2m{text}\x1b[0m");
        } else {
            eprintln!("{text}");
        }
    }

    fn start_turn(&self) -> TurnPrinter<'_> {
        TurnPrinter {
            printer: self,
            mid_line: false,
            usage: Usage::default(),
        }
    }
}

struct TurnPrinter<'a> {
    printer: &'a Printer,
    /// Whether stdout has text not yet ended by a newline.
    mid_line: bool,
    usage: Usage,
}

impl TurnPrinter<'_> {
    fn handle(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta(text) => {
                let mut stdout = io::stdout().lock();
                let _ = stdout.write_all(text.as_bytes());
                let _ = stdout.flush();
                if !text.is_empty() {
                    self.mid_line = !text.ends_with('\n');
                }
            }
            AgentEvent::ToolCallStarted {
                name, args_json, ..
            } => {
                self.status(&format!("→ {name} {}", clip(&args_json, 120)));
            }
            AgentEvent::ToolCallFinished {
                output, is_error, ..
            } => {
                let summary = if is_error {
                    format!("  ✗ {}", clip(output.lines().next().unwrap_or(""), 120))
                } else {
                    let lines = output.lines().count();
                    format!("  ✓ {lines} line{}", if lines == 1 { "" } else { "s" })
                };
                self.status(&summary);
            }
            AgentEvent::Usage(usage) => self.usage += usage,
            AgentEvent::TurnFinished(outcome) => {
                match outcome {
                    TurnOutcome::Completed => {}
                    TurnOutcome::Cancelled => self.status("[cancelled]"),
                    TurnOutcome::MaxSteps => {
                        self.status("[stopped: reached the step limit for this turn]")
                    }
                }
                self.usage_line();
            }
        }
    }

    fn finish(&mut self) {
        self.end_line();
    }

    fn end_line(&mut self) {
        if self.mid_line {
            println!();
            self.mid_line = false;
        }
    }

    fn status(&mut self, text: &str) {
        self.end_line();
        self.printer.status(text);
    }

    fn usage_line(&mut self) {
        if self.usage == Usage::default() {
            return;
        }
        let mut line = format!(
            "[{} in / {} out tokens",
            self.usage.input_tokens, self.usage.output_tokens
        );
        if let Some(model) = &self.printer.model {
            line.push_str(&format!(" · ~${:.4}", model.cost_usd(&self.usage)));
        }
        line.push(']');
        self.status(&line);
    }
}

/// `text` on one line, cut to `max` characters.
fn clip(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_flattens_and_cuts() {
        assert_eq!(clip("a\nb", 10), "a b");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("ééé", 3), "ééé");
    }
}
