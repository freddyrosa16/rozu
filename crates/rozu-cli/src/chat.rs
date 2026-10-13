//! `rozu chat`: run the agent against OpenAI, Anthropic or xAI with the read-only workspace tools.
//!
//! Assistant text streams to stdout; tool calls, status and errors go to stderr (dimmed on a
//! terminal), so `rozu chat "…" > answer.md` captures just the answer. In the interactive session,
//! `/model <provider/model>` switches the model for the next turn while keeping the conversation.

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use clap::Args;
use rozu_core::{
    Agent, AgentConfig, AgentError, AgentEvent, DEFAULT_MAX_STEPS, Session, TurnOutcome, TurnUsage,
};
use rozu_providers::{ModelChoice, registry};
use rozu_secrets::{KeyringStore, env_var_for, resolve_api_key};
use rozu_tools::{ToolRegistry, Workspace};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Args)]
pub struct ChatArgs {
    /// Provider: openai, anthropic or xai. Defaults to the model's provider, else openai.
    #[arg(long, env = "ROZU_PROVIDER")]
    provider: Option<String>,
    /// Model id or provider/model (see `rozu models`). Defaults to the provider's default model.
    #[arg(long, env = "ROZU_MODEL")]
    model: Option<String>,
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

/// Builds an [`Agent`] for a model choice; kept around so `/model` can rebuild mid-session.
struct AgentFactory {
    store: KeyringStore,
    tools: Arc<ToolRegistry>,
    workspace: Workspace,
    max_steps: usize,
}

impl AgentFactory {
    /// Resolves the provider's key (env var first, then the keychain) and builds the agent.
    fn build(&self, choice: &ModelChoice) -> anyhow::Result<Agent> {
        let provider = choice.provider;
        let (key, _source) =
            resolve_api_key(&self.store, provider.id, |var| std::env::var(var).ok())?.ok_or_else(
                || {
                    anyhow!(
                        "no {} API key: run `rozu auth set {}` or set {}",
                        provider.label,
                        provider.id,
                        env_var_for(provider.id).unwrap_or("its API key variable"),
                    )
                },
            )?;
        let base_url = base_url_override(provider.id);
        let provider = registry::build(provider, key, base_url.as_deref(), None)?;
        let mut config = AgentConfig::new(&choice.model).with_model_info(choice.info.clone());
        config.max_steps = self.max_steps;
        Ok(Agent::new(
            provider,
            self.tools.clone(),
            self.workspace.clone(),
            config,
        ))
    }
}

/// `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL` or `XAI_BASE_URL`, if set: for proxies and tests.
fn base_url_override(provider: &str) -> Option<String> {
    std::env::var(format!("{}_BASE_URL", provider.to_ascii_uppercase()))
        .ok()
        .filter(|url| !url.trim().is_empty())
}

/// Parses a `/model` argument: a provider name (its default model), `provider/model` or a model id.
fn parse_model_switch(arg: &str) -> Result<ModelChoice, registry::RegistryError> {
    if registry::provider_info(arg).is_some() {
        registry::resolve(Some(arg), None)
    } else {
        registry::resolve(None, Some(arg))
    }
}

async fn chat(args: ChatArgs) -> anyhow::Result<ExitCode> {
    let mut choice = registry::resolve(args.provider.as_deref(), args.model.as_deref())?;
    let workspace = Workspace::new(&args.workspace)
        .with_context(|| format!("cannot open workspace {}", args.workspace.display()))?;
    let factory = AgentFactory {
        store: KeyringStore::default(),
        tools: Arc::new(ToolRegistry::read_only()),
        workspace: workspace.clone(),
        max_steps: args.max_steps,
    };
    let mut agent = factory.build(&choice)?;
    let mut session = Session::with_system_prompt(system_prompt(&workspace));
    let printer = Printer::new();
    let interrupts = Interrupts::install();
    printer.unpriced_note(&choice);

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
            "rozu {} · {} · {}\nCtrl-C cancels a turn (twice exits) · /model <provider/model> switches · Ctrl-D or /exit quits",
            env!("CARGO_PKG_VERSION"),
            choice.spec(),
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
        if let Some(arg) = command_arg(input, "/model") {
            if arg.is_empty() {
                printer.status(&format!(
                    "model: {} · usage: /model <provider/model> (see `rozu models`)",
                    choice.spec()
                ));
                continue;
            }
            match parse_model_switch(arg)
                .map_err(anyhow::Error::from)
                .and_then(|next| Ok((factory.build(&next)?, next)))
            {
                Ok((next_agent, next)) => {
                    agent = next_agent;
                    choice = next;
                    printer.status(&format!("switched to {}", choice.spec()));
                    printer.unpriced_note(&choice);
                }
                Err(err) => eprintln!("error: {err:#}"),
            }
            continue;
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
    printer.usage_line(&session.last_turn);
    result
}

/// The argument of `/name arg`, or `""` for a bare `/name`; `None` for other input.
fn command_arg<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let rest = input.strip_prefix(name)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
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
    dim: bool,
}

impl Printer {
    fn new() -> Self {
        let dim = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self { dim }
    }

    fn unpriced_note(&self, choice: &ModelChoice) {
        if choice.info.is_none() {
            self.status(&format!(
                "note: {} isn't in `rozu models`, so cost won't be shown",
                choice.spec()
            ));
        }
    }

    /// `[12 in (8 cached, 2 cache-written) / 4 out tokens · ~$0.0001]` after a turn that used
    /// tokens.
    fn usage_line(&self, turn: &TurnUsage) {
        let usage = &turn.usage;
        if usage.input_tokens == 0 && usage.output_tokens == 0 {
            return;
        }
        let mut line = format!("[{} in", usage.input_tokens);
        let mut cache = Vec::new();
        if usage.cache_read_tokens > 0 {
            cache.push(format!("{} cached", usage.cache_read_tokens));
        }
        if usage.cache_write_tokens > 0 {
            cache.push(format!("{} cache-written", usage.cache_write_tokens));
        }
        if !cache.is_empty() {
            line.push_str(&format!(" ({})", cache.join(", ")));
        }
        line.push_str(&format!(" / {} out tokens", usage.output_tokens));
        if let Some(cost) = turn.cost_usd {
            line.push_str(&format!(" · ~${cost:.4}"));
        }
        line.push(']');
        self.status(&line);
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
        }
    }
}

struct TurnPrinter<'a> {
    printer: &'a Printer,
    /// Whether stdout has text not yet ended by a newline.
    mid_line: bool,
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
            // Totals come from the session once the turn ends (see `Printer::usage_line`).
            AgentEvent::Usage(_) => {}
            AgentEvent::TurnFinished(outcome) => match outcome {
                TurnOutcome::Completed => {}
                TurnOutcome::Cancelled => self.status("[cancelled]"),
                TurnOutcome::MaxSteps => {
                    self.status("[stopped: reached the step limit for this turn]")
                }
            },
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

    #[test]
    fn parses_slash_commands() {
        assert_eq!(command_arg("/model", "/model"), Some(""));
        assert_eq!(
            command_arg("/model  xai/grok-4 ", "/model"),
            Some("xai/grok-4")
        );
        assert_eq!(command_arg("/models", "/model"), None);
        assert_eq!(command_arg("hello", "/model"), None);
    }

    #[test]
    fn model_switch_accepts_provider_spec_or_id() {
        let spec = |arg| parse_model_switch(arg).unwrap().spec();
        assert_eq!(spec("anthropic"), "anthropic/claude-sonnet-4-5");
        assert_eq!(spec("xai/grok-4"), "xai/grok-4");
        assert_eq!(spec("gpt-5-mini"), "openai/gpt-5-mini");
        assert!(parse_model_switch("mystery").is_err());
    }
}
