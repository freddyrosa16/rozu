//! `rozu chat`: run the agent against OpenAI, Anthropic or xAI (API keys), or Claude, Codex or
//! Grok through their official CLIs (subscriptions), with the workspace tools (read,
//! write/patch, shell) under an approval mode.
//!
//! Assistant text streams to stdout; tool calls, approval prompts, status and errors go to stderr
//! (dimmed on a terminal), so `rozu chat "…" > answer.md` captures just the answer. In the
//! interactive session, `/model <provider/model>` switches the model for the next turn while
//! keeping the conversation, and `/pin <note>` adds a note to the project memory.
//!
//! Every chat is saved (see `rozu sessions`) and `--resume [ID]` continues one.

mod render;

use std::io::{self, Write};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, anyhow};
use clap::Args;
use rozu_core::{
    Agent, AgentError, ApprovalMode, DEFAULT_MAX_STEPS, MemoryPolicy, Session, TurnOutcome,
    new_session_id,
};
use rozu_providers::{ModelChoice, registry};
use rozu_runtime::{AgentFactory, TERMINAL_HOST, load_memory, resolve_choice, set_system};
use rozu_store::{SessionMeta, SessionStore, SessionWriter, unix_now};
use rozu_tools::Workspace;
use tokio::sync::mpsc;

use crate::approve::{self, ApprovalArg, Ask, CliGate, Input};
use crate::interrupt::{EXIT_INTERRUPTED, Interrupts};

use render::{Printer, compacted_line, sandbox_label, session_label};

/// Upper bound for `--max-steps`.
const MAX_STEPS_LIMIT: u64 = 200;

#[derive(Debug, Args)]
pub struct ChatArgs {
    /// Provider: openai, anthropic, xai (API keys), claude-cli (your Claude subscription via
    /// the official claude CLI), codex (your ChatGPT subscription via the official Codex CLI)
    /// or grok (your SuperGrok subscription via xAI's Grok Build CLI). Defaults to the model's
    /// provider, else openai, or (when no API key is configured) the first installed of
    /// claude-cli, codex and grok.
    #[arg(long, env = "ROZU_PROVIDER")]
    provider: Option<String>,
    /// Model id or provider/model (see `rozu models`); for claude-cli also sonnet, opus, haiku
    /// or any id the claude CLI accepts; for codex any model the Codex CLI accepts (`default`:
    /// Codex's configured model); for grok e.g. grok-4.7 (`default`: Grok Build's own).
    /// Defaults to the provider's default model.
    #[arg(long, env = "ROZU_MODEL")]
    model: Option<String>,
    /// For --provider grok: run each turn as a headless `grok -p` instead of the default
    /// long-lived ACP session (fallback mode; rozu's tools are then available only if you
    /// registered `rozu mcp-serve` with Grok yourself).
    #[arg(long, env = "ROZU_GROK_HEADLESS")]
    grok_headless: bool,
    /// Directory the agent works in. File tools reject paths outside it, and sandboxed shell
    /// commands can only write here and to temp directories.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// What runs without asking: read-only (only reads), auto (reads + sandboxed workspace
    /// writes), full (everything except destructive commands, interpreter code and redirects).
    /// Shell commands are sandboxed in every mode.
    #[arg(long, value_enum, default_value_t = ApprovalArg::Auto)]
    approval: ApprovalArg,
    /// Maximum model requests per turn (1-200).
    #[arg(
        long,
        default_value_t = DEFAULT_MAX_STEPS,
        value_parser = clap::builder::RangedU64ValueParser::<usize>::new().range(1..=MAX_STEPS_LIMIT),
    )]
    max_steps: usize,
    /// Continue a saved chat: an id or id prefix from `rozu sessions`, or nothing for this
    /// workspace's latest. Uses the chat's last model unless --provider or --model is given.
    #[arg(long, num_args = 0..=1, default_missing_value = "last", value_name = "ID")]
    resume: Option<String>,
    /// How older turns are shrunk near the context window: default (hide old tool outputs at
    /// 60%, summarize at 80%), mask, summarize or off. `/compact` does both now.
    #[arg(long, default_value = "default", value_parser = memory_preset)]
    memory: MemoryPolicy,
    /// Don't save this chat.
    #[arg(long)]
    no_save: bool,
    /// Prompt to run once. Without it, rozu starts an interactive session.
    #[arg(trailing_var_arg = true)]
    prompt: Vec<String>,
}

pub fn run(args: ChatArgs) -> anyhow::Result<ExitCode> {
    let runtime = tokio::runtime::Runtime::new().context("could not start the async runtime")?;
    runtime.block_on(chat(args))
}

fn memory_preset(name: &str) -> Result<MemoryPolicy, String> {
    MemoryPolicy::preset(name).ok_or_else(|| {
        let names: Vec<_> = MemoryPolicy::PRESETS.iter().map(|(n, _)| *n).collect();
        format!("expected one of {}", names.join(", "))
    })
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
    let workspace = Workspace::new(&args.workspace)
        .with_context(|| format!("cannot open workspace {}", args.workspace.display()))?;
    let sessions = if args.no_save {
        None
    } else {
        match SessionStore::open_default() {
            Ok(sessions) => Some(sessions),
            Err(err) if args.resume.is_some() => {
                return Err(anyhow!(err).context("cannot open the saved sessions"));
            }
            Err(err) => {
                eprintln!("warning: this chat won't be saved: {err}");
                None
            }
        }
    };
    let resumed = match (&sessions, args.resume.as_deref()) {
        (Some(sessions), Some(query)) => {
            let id = sessions.resolve(Some(query), workspace.root())?;
            Some(sessions.resume(&id)?)
        }
        (None, Some(_)) => return Err(anyhow!("--resume can't be used with --no-save")),
        _ => None,
    };
    let mut choice = match &resumed {
        Some((_, writer)) if args.provider.is_none() && args.model.is_none() => {
            let meta = writer.meta();
            registry::resolve(Some(&meta.provider), Some(&meta.model))?
        }
        _ => resolve_choice(args.provider.clone(), args.model.as_deref())?,
    };
    let mode = ApprovalMode::from(args.approval);
    let interactive = approve::stdin_is_interactive();
    let (gate, mut asks) = CliGate::new(interactive);
    let session_id = resumed
        .as_ref()
        .map_or_else(new_session_id, |(_, writer)| writer.id().to_owned());
    let mut factory = AgentFactory::new(&workspace, mode, gate, session_id)?;
    factory.max_steps = args.max_steps;
    factory.memory = args.memory;
    factory.grok_headless = args.grok_headless;
    let mut agent = factory.build(&choice)?;
    let mut memory = load_memory(workspace.root());
    let (mut session, mut writer) = match resumed {
        Some((session, writer)) => (session, Some(writer)),
        None => {
            let writer = sessions.as_ref().and_then(|sessions| {
                let meta = SessionMeta {
                    id: factory.session_id().to_owned(),
                    workspace: workspace.root().to_owned(),
                    provider: choice.provider.id.to_owned(),
                    model: choice.model.clone(),
                    created: unix_now(),
                };
                sessions
                    .create(meta)
                    .inspect_err(|err| eprintln!("warning: this chat won't be saved: {err}"))
                    .ok()
            });
            (Session::new(), writer)
        }
    };
    set_system(&mut session, workspace.root(), mode, TERMINAL_HOST, &memory);
    let printer = Printer::new();
    let interrupts = Interrupts::install();
    let input = {
        let interrupts = interrupts.clone();
        Input::spawn(Some(Box::new(move || interrupts.tty_press())))
    };
    let io = TurnIo {
        input: &input,
        interactive,
        interrupts: &interrupts,
        printer: &printer,
    };
    crate::sandbox_warning::warn_if_unsandboxed();
    printer.unpriced_note(&choice);

    let prompt = args.prompt.join(" ");
    if !prompt.trim().is_empty() {
        let outcome = turn(&agent, &mut session, prompt, &mut asks, &io).await;
        save(&mut writer, &session);
        let outcome = outcome?;
        return Ok(match outcome {
            TurnOutcome::Cancelled => ExitCode::from(EXIT_INTERRUPTED as u8),
            _ => ExitCode::SUCCESS,
        });
    }

    let interactive = io.interactive;
    if interactive {
        printer.status(&format!(
            "rozu {} · {} · {}\napprovals: {} · sandbox: {}\n{}\nCtrl-C cancels a turn (twice exits) · /model <provider/model> switches · /pin <note> remembers · /compact shrinks · Ctrl-D or /exit quits",
            env!("CARGO_PKG_VERSION"),
            choice.spec(),
            workspace.root().display(),
            mode,
            sandbox_label(),
            session_label(writer.as_ref(), &session, &memory),
        ));
    }
    loop {
        if interactive {
            eprint!("\n> ");
            let _ = io::stderr().flush();
        }
        let Some(line) = input.line().await else {
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
                    if let Some(writer) = writer.as_mut() {
                        if let Err(err) = writer.set_model(choice.provider.id, &choice.model) {
                            tracing::warn!(error = %err, "could not save the model switch");
                        }
                    }
                    printer.status(&format!("switched to {}", choice.spec()));
                    printer.unpriced_note(&choice);
                }
                Err(err) => eprintln!("error: {err:#}"),
            }
            continue;
        }
        if command_arg(input, "/compact").is_some() {
            if agent.manages_own_context() {
                printer.status(&format!(
                    "{} keeps and compacts its own context",
                    choice.provider.id
                ));
                continue;
            }
            let done = agent.compact(&mut session, interrupts.begin_turn()).await;
            interrupts.end_turn();
            if done.masked + done.summarized == 0 {
                printer.status("nothing to compact yet");
            } else {
                printer.status(&compacted_line(&done));
                save(&mut writer, &session);
            }
            continue;
        }
        if let Some(note) = command_arg(input, "/pin") {
            if note.is_empty() {
                printer.status("usage: /pin <note> (saved to .rozu/pinned.md and kept in context)");
                continue;
            }
            match rozu_store::pin(workspace.root(), note) {
                Ok(path) => {
                    memory = load_memory(workspace.root());
                    set_system(&mut session, workspace.root(), mode, TERMINAL_HOST, &memory);
                    printer.status(&format!("pinned to {}", path.display()));
                }
                Err(err) => eprintln!("error: could not pin the note: {err}"),
            }
            continue;
        }
        let result = turn(&agent, &mut session, input.to_owned(), &mut asks, &io).await;
        save(&mut writer, &session);
        if let Err(err) = result {
            eprintln!("error: {err}");
        }
    }
    if let Some(writer) = writer.as_ref().filter(|_| interactive) {
        if session
            .messages
            .iter()
            .any(|m| m.role == rozu_core::Role::User)
        {
            printer.status(&format!("resume with: rozu chat --resume {}", writer.id()));
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn save(writer: &mut Option<SessionWriter>, session: &Session) {
    if let Some(w) = writer.as_mut() {
        if let Err(err) = w.sync(session) {
            eprintln!("warning: could not save the chat, so it won't be resumable: {err}");
            *writer = None;
        }
    }
}

/// Everything a turn needs to talk to the user.
struct TurnIo<'a> {
    input: &'a Input,
    /// Whether approval prompts can be answered (stdin is a terminal).
    interactive: bool,
    interrupts: &'a Interrupts,
    printer: &'a Printer,
}

/// Runs one turn while printing its events and answering approval prompts, in order.
async fn turn(
    agent: &Agent,
    session: &mut Session,
    input: String,
    asks: &mut mpsc::Receiver<Ask>,
    io: &TurnIo<'_>,
) -> Result<TurnOutcome, AgentError> {
    let cancel = io.interrupts.begin_turn();
    let (tx, mut rx) = mpsc::channel(256);
    let mut state = io.printer.start_turn();
    let render = async {
        loop {
            tokio::select! {
                // Events first, so the tool call line prints before its approval card.
                biased;
                event = rx.recv() => match event {
                    Some(event) => state.handle(event),
                    None => break,
                },
                Some(ask) = asks.recv() => {
                    state.end_line();
                    approve::answer(ask, io.input, io.interactive, io.printer.dim).await;
                }
            }
        }
        state.finish();
    };
    let run = async {
        let result = agent.run_turn(input, session, &tx, cancel).await;
        drop(tx);
        result
    };
    let (result, ()) = tokio::join!(run, render);
    io.interrupts.end_turn();
    if let Some(done) = &session.last_compaction {
        io.printer.status(&compacted_line(done));
    }
    io.printer.usage_line(&session.last_turn);
    result
}

/// The argument of `/name arg`, or `""` for a bare `/name`; `None` for other input.
fn command_arg<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let rest = input.strip_prefix(name)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(spec("anthropic"), "anthropic/claude-sonnet-5-5");
        assert_eq!(spec("xai/grok-4.7"), "xai/grok-4.7");
        assert_eq!(spec("gpt-5-mini"), "openai/gpt-5-mini");
        assert!(parse_model_switch("mystery").is_err());
    }
}
