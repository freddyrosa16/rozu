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

use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use std::sync::Mutex;

use anyhow::{Context, anyhow};
use clap::{Args, ValueEnum};
use rozu_cli_providers::claude::install::{CLAUDE_BIN_ENV, find_claude_binary};
use rozu_cli_providers::codex::install::{CODEX_BIN_ENV, find_codex_binary};
use rozu_cli_providers::grok::headless::GrokHeadless;
use rozu_cli_providers::grok::hook::{self, HookPolicy};
use rozu_cli_providers::grok::install::find_grok_binary;
use rozu_cli_providers::{
    ClaudeCliConfig, ClaudeCliProvider, CodexAppServerProvider, CodexConfig, GrokConfig,
    GrokSession,
};
use rozu_core::{
    Agent, AgentConfig, AgentError, AgentEvent, ApprovalMode, ApprovalPolicy, ApprovalRecord,
    AuditLog, DEFAULT_MAX_STEPS, Decision, DecisionSource, Provider, Session, ToolExecutor,
    TurnOutcome, TurnUsage, approvals_path, new_session_id,
};
use rozu_providers::registry::{CLAUDE_CLI_ID, CODEX_ID, GROK_ID};
use rozu_providers::{ModelChoice, PROVIDERS, registry};
use rozu_secrets::{KeyringStore, SecretStore, env_var_for, resolve_api_key};
use rozu_store::{ProjectMemory, SessionMeta, SessionStore, SessionWriter, unix_now};
use rozu_tools::{Risk, SessionTemp, ToolRegistry, Workspace, platform_sandbox};
use tokio::sync::mpsc;

use crate::approve::{self, ApprovalArg, Ask, CliGate, Input};
use crate::interrupt::{EXIT_INTERRUPTED, Interrupts};

/// Upper bound for `--max-steps`.
const MAX_STEPS_LIMIT: u64 = 200;
/// If set to a byte count, Grok gets rozu's `PostToolUse` hook (`rozu grok-hook`), which keeps
/// the head and tail of rozu tool results longer than that (unverified with Grok Build; off by
/// default, since `rozu mcp-serve` already caps tool output).
const GROK_HOOK_MAX_BYTES_ENV: &str = "ROZU_GROK_HOOK_MAX_BYTES";

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

/// Builds an [`Agent`] for a model choice; kept around so `/model` can rebuild mid-session.
struct AgentFactory {
    store: KeyringStore,
    tools: Arc<ToolRegistry>,
    workspace: Workspace,
    max_steps: usize,
    approval: ApprovalArg,
    policy: Arc<ApprovalPolicy>,
    gate: Arc<CliGate>,
    /// One claude CLI session for the whole chat, kept across `/model` switches.
    claude: Mutex<Option<Arc<ClaudeCliProvider>>>,
    /// One codex app-server (and thread) for the whole chat, kept across `/model` switches.
    codex: Mutex<Option<Arc<CodexAppServerProvider>>>,
    /// One Grok Build process (and its context epochs) for the whole chat.
    grok: Mutex<Option<Arc<dyn Provider>>>,
    grok_headless: bool,
    /// Ties this chat's audit entries (and those of its `rozu mcp-serve`) together.
    session_id: String,
    audit: Option<Arc<AuditLog>>,
    /// The session's private temp dir, shared by every agent of the chat.
    temp: Option<Arc<SessionTemp>>,
}

impl AgentFactory {
    /// Resolves the provider's key (env var first, then the keychain) — or, for claude-cli and
    /// codex, the vendor CLI — and builds the agent.
    fn build(&self, choice: &ModelChoice) -> anyhow::Result<Agent> {
        let provider: Arc<dyn Provider> = if choice.provider.id == CODEX_ID {
            self.codex_provider()?
        } else if choice.provider.id == GROK_ID {
            self.grok_provider()?
        } else if choice.provider.is_cli_backed() {
            self.claude_provider()?
        } else {
            let provider = choice.provider;
            let (key, _source) =
                resolve_api_key(&self.store, provider.id, |var| std::env::var(var).ok())?
                    .ok_or_else(|| {
                        anyhow!(
                            "no {} API key: run `rozu auth set {}` or set {} (or use a \
                             subscription: --provider claude-cli, codex or grok)",
                            provider.label,
                            provider.id,
                            env_var_for(provider.id).unwrap_or("its API key variable"),
                        )
                    })?;
            let base_url = base_url_override(provider.id);
            registry::build(provider, key, base_url.as_deref(), None)?
        };
        let mut config = AgentConfig::new(&choice.model).with_model_info(choice.info.clone());
        config.max_steps = self.max_steps;
        Ok(
            Agent::new(provider, self.tools.clone(), self.workspace.clone(), config)
                .with_approval_policy(self.policy.clone())
                .with_approval_gate(self.gate.clone())
                .with_audit(self.audit.clone())
                .with_session_temp(self.temp.clone()),
        )
    }

    /// The claude CLI provider, started once. Claude reaches rozu's tools through
    /// `rozu mcp-serve` (this binary), which asks this process's gate when approval is needed.
    fn claude_provider(&self) -> anyhow::Result<Arc<dyn Provider>> {
        let mut cached = self.claude.lock().expect("claude provider lock");
        if let Some(provider) = cached.as_ref() {
            return Ok(provider.clone());
        }
        let binary = find_claude_binary().ok_or_else(|| {
            anyhow!(
                "the claude CLI isn't installed (looked on PATH and in ~/.local/bin): install \
                 Claude Code and log in with `claude` → /login, or set {CLAUDE_BIN_ENV}"
            )
        })?;
        let rozu = std::env::current_exe().context("cannot locate the rozu binary")?;
        let config = ClaudeCliConfig::new(
            binary,
            self.workspace.root(),
            rozu,
            self.mcp_args(None),
            self.gate.clone(),
        );
        let provider = Arc::new(
            ClaudeCliProvider::new(config).context("could not prepare the claude CLI session")?,
        );
        *cached = Some(provider.clone());
        Ok(provider)
    }

    /// The codex app-server provider, started once. Codex's own approval requests are decided
    /// here (policy, then this process's gate); rozu's tools reach it through
    /// `rozu mcp-serve --provider codex`.
    fn codex_provider(&self) -> anyhow::Result<Arc<dyn Provider>> {
        let mut cached = self.codex.lock().expect("codex provider lock");
        if let Some(provider) = cached.as_ref() {
            return Ok(provider.clone());
        }
        let binary = find_codex_binary().ok_or_else(|| {
            anyhow!(
                "the Codex CLI isn't installed (looked on PATH and in /opt/homebrew/bin): install \
                 it, log in with `rozu auth login codex`, or set {CODEX_BIN_ENV}"
            )
        })?;
        let rozu = std::env::current_exe().context("cannot locate the rozu binary")?;
        let mut exec = ToolExecutor::new(
            self.tools.clone(),
            self.gate.clone(),
            self.policy.clone(),
            platform_sandbox(),
        )
        .with_audit(self.audit.clone())
        .with_provider(Some(CODEX_ID.to_owned()));
        exec.temp = self.temp.clone();
        let config = CodexConfig::new(
            binary,
            self.workspace.clone(),
            rozu,
            self.mcp_args(Some(CODEX_ID)),
            exec,
        );
        let provider = Arc::new(
            CodexAppServerProvider::new(config)
                .context("could not prepare the codex app-server session")?,
        );
        *cached = Some(provider.clone());
        Ok(provider)
    }

    /// The Grok Build provider, started once: the long-lived ACP session (default) or the
    /// headless fallback. Grok's built-ins are off; its only tools are rozu's, through
    /// `rozu mcp-serve --provider grok`, which asks this process's gate.
    fn grok_provider(&self) -> anyhow::Result<Arc<dyn Provider>> {
        let mut cached = self.grok.lock().expect("grok provider lock");
        if let Some(provider) = cached.as_ref() {
            return Ok(provider.clone());
        }
        let binary = find_grok_binary().map_err(|err| anyhow!(err))?;
        let rozu = std::env::current_exe().context("cannot locate the rozu binary")?;
        let mut exec = ToolExecutor::new(
            self.tools.clone(),
            self.gate.clone(),
            self.policy.clone(),
            platform_sandbox(),
        )
        .with_audit(self.audit.clone())
        .with_provider(Some(GROK_ID.to_owned()));
        exec.temp = self.temp.clone();
        let mut config = GrokConfig::new(
            binary,
            self.workspace.clone(),
            &rozu,
            self.mcp_args(Some(GROK_ID)),
            exec,
        );
        config.post_tool_hook = std::env::var(GROK_HOOK_MAX_BYTES_ENV)
            .ok()
            .and_then(|v| v.trim().parse::<usize>().ok())
            .map(|max_bytes| {
                hook::command(
                    &rozu.display().to_string(),
                    &HookPolicy::Truncate { max_bytes },
                )
            });
        let provider: Arc<dyn Provider> = if self.grok_headless {
            Arc::new(GrokHeadless::new(config))
        } else {
            Arc::new(GrokSession::new(config).context("could not prepare the Grok Build session")?)
        };
        *cached = Some(provider.clone());
        Ok(provider)
    }

    /// Arguments of the `rozu mcp-serve` a vendor CLI starts; `provider` tags its audit entries.
    fn mcp_args(&self, provider: Option<&str>) -> Vec<String> {
        let approval = self
            .approval
            .to_possible_value()
            .expect("approval values are named");
        let mut args = vec![
            "mcp-serve".to_owned(),
            "--workspace".to_owned(),
            self.workspace.root().display().to_string(),
            "--approval".to_owned(),
            approval.get_name().to_owned(),
            "--session-id".to_owned(),
            self.session_id.clone(),
        ];
        if let Some(provider) = provider {
            args.extend(["--provider".to_owned(), provider.to_owned()]);
        }
        args
    }
}

/// The provider to use when neither `--provider` nor `--model` is given: with no API key
/// configured anywhere, the first installed of claude-cli, codex and grok (claude-cli when
/// none is, so the error says how to install it); else the registry's default.
fn default_provider(
    provider: Option<String>,
    model: Option<&str>,
    any_api_key: impl FnOnce() -> bool,
    cli_installed: impl Fn(&str) -> bool,
) -> Option<String> {
    match (provider, model) {
        (Some(provider), _) => Some(provider),
        (None, Some(_)) => None,
        (None, None) if any_api_key() => None,
        (None, None) => Some(
            [CLAUDE_CLI_ID, CODEX_ID, GROK_ID]
                .into_iter()
                .find(|id| cli_installed(id))
                .unwrap_or(CLAUDE_CLI_ID)
                .to_owned(),
        ),
    }
}

/// Whether the vendor CLI behind a CLI-backed provider is installed.
fn cli_installed(provider: &str) -> bool {
    match provider {
        CLAUDE_CLI_ID => find_claude_binary().is_some_and(|p| p.exists()),
        CODEX_ID => find_codex_binary().is_some_and(|p| p.exists()),
        GROK_ID => find_grok_binary().is_ok_and(|p| p.exists()),
        _ => false,
    }
}

/// Whether any API-key provider has a key (env var or keychain).
fn any_api_key(store: &dyn SecretStore) -> bool {
    PROVIDERS.iter().filter(|p| p.needs_api_key()).any(|p| {
        matches!(
            resolve_api_key(store, p.id, |var| std::env::var(var).ok()),
            Ok(Some(_))
        )
    })
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
    let store = KeyringStore::default();
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
        _ => {
            let provider = default_provider(
                args.provider.clone(),
                args.model.as_deref(),
                || any_api_key(&store),
                cli_installed,
            );
            registry::resolve(provider.as_deref(), args.model.as_deref())?
        }
    };
    let mode = ApprovalMode::from(args.approval);
    let policy = ApprovalPolicy::for_project(mode, workspace.root()).with_context(|| {
        format!(
            "cannot read the approval rules in {}; fix or delete the file",
            approvals_path(workspace.root()).display()
        )
    })?;
    let interactive = approve::stdin_is_interactive();
    let (gate, mut asks) = CliGate::new(interactive);
    let session_id = resumed
        .as_ref()
        .map_or_else(new_session_id, |(_, writer)| writer.id().to_owned());
    let temp = match SessionTemp::create(&session_id) {
        Ok(temp) => Some(Arc::new(temp)),
        Err(err) => {
            tracing::warn!(error = %err, "could not create the session temp dir");
            None
        }
    };
    let factory = AgentFactory {
        store,
        tools: Arc::new(ToolRegistry::standard()),
        workspace: workspace.clone(),
        max_steps: args.max_steps,
        approval: args.approval,
        policy: Arc::new(policy),
        gate,
        claude: Mutex::default(),
        codex: Mutex::default(),
        grok: Mutex::default(),
        grok_headless: args.grok_headless,
        audit: AuditLog::from_env(&session_id).map(Arc::new),
        session_id,
        temp,
    };
    let mut agent = factory.build(&choice)?;
    let mut memory = load_memory(&workspace);
    let (mut session, mut writer) = match resumed {
        Some((session, writer)) => (session, Some(writer)),
        None => {
            let writer = sessions.as_ref().and_then(|sessions| {
                let meta = SessionMeta {
                    id: factory.session_id.clone(),
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
    set_system(&mut session, &workspace, mode, &memory);
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
            "rozu {} · {} · {}\napprovals: {} · sandbox: {}\n{}\nCtrl-C cancels a turn (twice exits) · /model <provider/model> switches · /pin <note> remembers · Ctrl-D or /exit quits",
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
        if let Some(note) = command_arg(input, "/pin") {
            if note.is_empty() {
                printer.status("usage: /pin <note> (saved to .rozu/pinned.md and kept in context)");
                continue;
            }
            match rozu_store::pin(workspace.root(), note) {
                Ok(path) => {
                    memory = load_memory(&workspace);
                    set_system(&mut session, &workspace, mode, &memory);
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

fn load_memory(workspace: &Workspace) -> ProjectMemory {
    ProjectMemory::load(workspace.root()).unwrap_or_else(|err| {
        eprintln!("warning: could not read the project memory: {err}");
        ProjectMemory::default()
    })
}

/// Replaces the leading system messages with the current system prompt and project memory, so
/// a resumed chat follows today's ROZU.md.
fn set_system(
    session: &mut Session,
    workspace: &Workspace,
    mode: ApprovalMode,
    memory: &ProjectMemory,
) {
    let old = session
        .messages
        .iter()
        .take_while(|m| m.role == rozu_core::Role::System)
        .count();
    let mut system = vec![rozu_core::Message::system(system_prompt(workspace, mode))];
    system.extend(memory.prompt().map(rozu_core::Message::system));
    session.messages.splice(..old, system);
}

fn save(writer: &mut Option<SessionWriter>, session: &Session) {
    if let Some(w) = writer.as_mut() {
        if let Err(err) = w.sync(session) {
            eprintln!("warning: could not save the chat, so it won't be resumable: {err}");
            *writer = None;
        }
    }
}

fn session_label(
    writer: Option<&SessionWriter>,
    session: &Session,
    memory: &ProjectMemory,
) -> String {
    let saved = match writer {
        None => "not saved".to_owned(),
        Some(w) => {
            let turns = session
                .messages
                .iter()
                .filter(|m| m.role == rozu_core::Role::User)
                .count();
            if turns == 0 {
                format!("session {}", w.id())
            } else {
                format!("resumed {} ({turns} earlier turns)", w.id())
            }
        }
    };
    let names: Vec<_> = memory.sources.iter().map(|(l, _)| l.as_str()).collect();
    if names.is_empty() {
        saved
    } else {
        format!("{saved} · memory: {}", names.join(", "))
    }
}

/// How shell commands are confined on this machine (the same in every approval mode).
fn sandbox_label() -> String {
    let sandbox = platform_sandbox();
    if sandbox.enforced() {
        format!(
            "{} in every mode (writes: workspace + session tmp; network only when approved)",
            sandbox.name()
        )
    } else {
        "unavailable on this OS (workspace writes by shell ask first)".to_owned()
    }
}

fn system_prompt(workspace: &Workspace, mode: ApprovalMode) -> String {
    format!(
        "You are rozu, a coding agent running in the user's terminal.\n\
         The workspace root is {}; tool paths are relative to it.\n\
         Tools: read_file, list_dir and grep to look at files; apply_patch to edit a file \
         (exact search/replace edits or a unified diff) and write_file to create one; shell to \
         run commands (non-interactive, with a timeout). Shell commands run in a sandbox that \
         only allows writes in the workspace and $TMPDIR (a private temp dir) and blocks the \
         network; set network: true when a command needs it.\n\
         Approval mode: {mode}. Calls that need approval are shown to the user, who may deny \
         them with a reason; respect it and adapt. Destructive commands (rm -rf, force push, \
         git reset --hard, ...), interpreter one-liners (python -c, eval, ...) and output \
         redirects always need approval, so prefer safer alternatives such as write_file and \
         apply_patch.\n\
         Read files before editing them, keep changes minimal, and run the relevant tests after \
         changing code. Be concise, and cite file paths when you refer to code.",
        workspace.root().display()
    )
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
    io.printer.usage_line(&session.last_turn);
    result
}

/// The argument of `/name arg`, or `""` for a bare `/name`; `None` for other input.
fn command_arg<'a>(input: &'a str, name: &str) -> Option<&'a str> {
    let rest = input.strip_prefix(name)?;
    (rest.is_empty() || rest.starts_with(char::is_whitespace)).then(|| rest.trim())
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
        match &choice.info {
            None => self.status(&format!(
                "note: {} isn't in `rozu models`, so cost won't be shown",
                choice.spec()
            )),
            Some(info) if info.caps.memory_features != rozu_core::MemoryFeatures::Full => {
                self.status(&format!(
                    "note: rozu's memory features are limited with {} (no per-turn control of \
                     the prompt prefix; cache writes aren't reported)",
                    choice.provider.id
                ));
            }
            Some(_) => {}
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
            last_denial: None,
        }
    }
}

struct TurnPrinter<'a> {
    printer: &'a Printer,
    /// Whether stdout has text not yet ended by a newline.
    mid_line: bool,
    /// The reason of the denial just printed, so the tool result repeating it isn't printed
    /// again.
    last_denial: Option<String>,
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
                self.last_denial = None;
                self.status(&format!("→ {name} {}", clip(&args_json, 120)));
            }
            AgentEvent::ToolCallFinished {
                output, is_error, ..
            } => {
                let first = output.lines().next().unwrap_or("");
                let denial = self.last_denial.take();
                if is_error && denial.is_some_and(|reason| repeats_denial(first, &reason)) {
                    return; // the "✗ denied" line already said it
                }
                let summary = if is_error {
                    format!("  ✗ {}", clip(first, 120))
                } else {
                    let lines = output.lines().count();
                    format!("  ✓ {lines} line{}", if lines == 1 { "" } else { "s" })
                };
                self.status(&summary);
            }
            AgentEvent::Approval(record) => {
                if let Some(line) = approval_line(&record) {
                    self.status(&line);
                }
                self.last_denial = match (&record.decision, &record.source, record.blocked_retry) {
                    (Decision::Deny { reason }, source, false)
                        if *source != DecisionSource::Cancelled =>
                    {
                        Some(reason.clone())
                    }
                    _ => None,
                };
            }
            // The card and the decision line already show the prompt and its answer.
            AgentEvent::ApprovalPending(_) | AgentEvent::ApprovalResolved { .. } => {}
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

/// Whether a failed tool result's first line just repeats the denial `reason`.
fn repeats_denial(first_line: &str, reason: &str) -> bool {
    first_line == reason
        || first_line.strip_prefix("denied: ") == Some(reason)
        || first_line.ends_with(reason)
}

/// One status line per notable approval decision; reads approved by the mode are silent.
fn approval_line(record: &ApprovalRecord) -> Option<String> {
    let mut confinement = match record.sandbox {
        rozu_core::SandboxMode::InProcess => String::new(),
        sandbox => format!(
            " · sandbox: {sandbox} · network: {}",
            if record.network { "on" } else { "off" }
        ),
    };
    if !record.extra_writes.is_empty() {
        confinement.push_str(&format!(
            " · also writable: {}",
            record
                .extra_writes
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if record.blocked_retry {
        return match (&record.decision, &record.source) {
            (_, DecisionSource::Cancelled) => None,
            (Decision::ApproveUnsandboxed, _) => {
                Some("  ⚠ rerunning UNSANDBOXED (this command only)".to_owned())
            }
            _ => Some("  ✗ kept the sandbox's refusal".to_owned()),
        };
    }
    match (&record.decision, &record.source) {
        (_, DecisionSource::Cancelled) => None,
        (Decision::Approve, DecisionSource::Mode(_)) if record.risk == Risk::Read => None,
        // `rozu mcp-serve` decides the call itself and its own entry follows.
        (Decision::Approve, DecisionSource::Delegated) => None,
        // Only the user can choose it (anything else runs sandboxed; see the executor).
        (Decision::ApproveUnsandboxed, _) => {
            Some("  ⚠ approved UNSANDBOXED (this command only)".to_owned())
        }
        (Decision::Approve, DecisionSource::Mode(mode)) => Some(format!(
            "  ✓ auto-approved ({}, {mode} mode){confinement}",
            record.risk
        )),
        (Decision::Approve, DecisionSource::Rule { prefix }) => Some(format!(
            "  ✓ allowed by project rule `{prefix}`{confinement}"
        )),
        (Decision::Approve, _) => Some(format!("  ✓ approved{confinement}")),
        (Decision::AlwaysAllowPrefix, _) => Some(format!(
            "  ✓ approved; always allowing this prefix in {}",
            rozu_tools::PROTECTED_DIR
        )),
        (Decision::Deny { reason }, _) => Some(format!("  ✗ denied: {}", clip(reason, 120))),
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
    fn claude_cli_then_codex_then_grok_are_the_defaults_without_api_keys() {
        let only = |ids: &'static [&'static str]| move |id: &str| ids.contains(&id);
        for (installed, expected) in [
            (only(&["codex", "grok"]), "codex"),
            (only(&["grok"]), "grok"),
            (only(&["claude-cli", "codex", "grok"]), "claude-cli"),
            (only(&["codex"]), "codex"),
            // With nothing installed, claude-cli's error says how to install it.
            (only(&[]), "claude-cli"),
        ] {
            assert_eq!(
                default_provider(None, None, || false, installed).as_deref(),
                Some(expected)
            );
        }
        let all = |_: &str| true;
        assert_eq!(default_provider(None, None, || true, all), None);
        assert_eq!(
            default_provider(Some("xai".into()), None, || false, all).as_deref(),
            Some("xai")
        );
        assert_eq!(default_provider(None, Some("gpt-5.1"), || false, all), None);
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
