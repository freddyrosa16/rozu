//! The app's link to rozu-core, built like `rozu chat` builds it: one agent and session per
//! chat, on a shared tokio runtime. Events and approval prompts come back as [`Update`]s.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use rozu_activity::ActivityEvent;
use rozu_cli_providers::claude::install::find_claude_binary;
use rozu_cli_providers::codex::install::find_codex_binary;
use rozu_cli_providers::grok::install::find_grok_binary;
use rozu_cli_providers::{
    ClaudeCliConfig, ClaudeCliProvider, CodexAppServerProvider, CodexConfig, GrokConfig,
    GrokSession,
};
use rozu_core::{
    Agent, AgentConfig, AgentEvent, ApprovalGate, ApprovalMode, ApprovalPolicy, ApprovalRequest,
    AuditLog, Decision, Provider, Session, ToolExecutor, new_session_id,
};
use rozu_providers::registry::{self, CLAUDE_CLI_ID, CODEX_ID, GROK_ID};
use rozu_providers::{ModelChoice, PROVIDERS};
use rozu_secrets::{KeyringStore, resolve_api_key};
use rozu_tools::{SessionTemp, ToolRegistry, Workspace, platform_sandbox};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

pub enum Update {
    Event(AgentEvent),
    Ask(ApprovalRequest, oneshot::Sender<Decision>),
    Failed { message: String, offline: bool },
}

pub struct Backend {
    rt: tokio::runtime::Runtime,
    workspace: Workspace,
    policy: Arc<ApprovalPolicy>,
    tools: Arc<ToolRegistry>,
}

/// A running chat: prompts go in, [`Update`]s come out on the channel given to [`Backend::start`].
pub struct Chat {
    prompts: mpsc::UnboundedSender<String>,
    cancel: Arc<Mutex<CancellationToken>>,
}

impl Chat {
    pub fn send(&self, text: String) -> bool {
        self.prompts.send(text).is_ok()
    }

    pub fn cancel(&self) {
        self.cancel.lock().expect("cancel lock").cancel();
    }
}

/// Sends each prompt to the UI and waits for the answer.
struct Gate(mpsc::UnboundedSender<Update>);

#[async_trait]
impl ApprovalGate for Gate {
    async fn decide(&self, req: ApprovalRequest) -> Decision {
        let (tx, rx) = oneshot::channel();
        let deny = |reason: &str| Decision::Deny {
            reason: reason.into(),
        };
        if self.0.send(Update::Ask(req, tx)).is_err() {
            return deny("the app closed");
        }
        rx.await.unwrap_or_else(|_| deny("the prompt was closed"))
    }
}

impl Backend {
    pub fn new(root: &Path) -> anyhow::Result<Self> {
        let workspace = Workspace::new(root)
            .with_context(|| format!("cannot open workspace {}", root.display()))?;
        let policy = ApprovalPolicy::for_project(ApprovalMode::AutoWorkspace, workspace.root())?;
        Ok(Self {
            rt: tokio::runtime::Runtime::new()?,
            workspace,
            policy: Arc::new(policy),
            tools: Arc::new(ToolRegistry::standard()),
        })
    }

    pub fn root(&self) -> &Path {
        self.workspace.root()
    }

    pub fn start(&self, choice: &ModelChoice, updates: mpsc::UnboundedSender<Update>) -> Chat {
        let (prompts, mut rx) = mpsc::unbounded_channel::<String>();
        let cancel = Arc::new(Mutex::new(CancellationToken::new()));
        let agent = {
            let _rt = self.rt.enter();
            self.agent(choice, Arc::new(Gate(updates.clone())))
        };
        let prompt = system_prompt(self.root());
        let current = cancel.clone();
        self.rt.spawn(async move {
            let agent = match agent {
                Ok(agent) => agent,
                Err(err) => {
                    let message = format!("{err:#}");
                    let _ = updates.send(Update::Failed {
                        message,
                        offline: false,
                    });
                    return;
                }
            };
            let mut session = Session::with_system_prompt(prompt);
            let (tx, mut events) = mpsc::channel(256);
            let out = updates.clone();
            tokio::spawn(async move {
                while let Some(event) = events.recv().await {
                    if out.send(Update::Event(event)).is_err() {
                        break;
                    }
                }
            });
            while let Some(text) = rx.recv().await {
                let token = CancellationToken::new();
                *current.lock().expect("cancel lock") = token.clone();
                if let Err(err) = agent.run_turn(text, &mut session, &tx, token).await {
                    let offline = matches!(
                        ActivityEvent::from(&err),
                        ActivityEvent::TurnFailed { offline: true }
                    );
                    let _ = updates.send(Update::Failed {
                        message: err.to_string(),
                        offline,
                    });
                }
            }
        });
        Chat { prompts, cancel }
    }

    fn agent(&self, choice: &ModelChoice, gate: Arc<dyn ApprovalGate>) -> anyhow::Result<Agent> {
        let id = new_session_id();
        let audit = AuditLog::from_env(&id).map(Arc::new);
        let temp = SessionTemp::create(&id).ok().map(Arc::new);
        let exec = |provider: &str| {
            let mut exec = ToolExecutor::new(
                self.tools.clone(),
                gate.clone(),
                self.policy.clone(),
                platform_sandbox(),
            )
            .with_audit(audit.clone())
            .with_provider(Some(provider.to_owned()));
            exec.temp = temp.clone();
            exec
        };
        let mcp = |provider: &str| -> anyhow::Result<(PathBuf, Vec<String>)> {
            let root = self.root().display().to_string();
            let args = [
                "mcp-serve",
                "--workspace",
                &root,
                "--approval",
                "auto",
                "--session-id",
                &id,
            ]
            .into_iter()
            .chain(
                ["--provider", provider]
                    .into_iter()
                    .filter(|_| provider != CLAUDE_CLI_ID),
            )
            .map(str::to_owned)
            .collect();
            Ok((rozu_binary()?, args))
        };
        let missing = |cli: &str| anyhow!("the {cli} CLI isn't installed (see `rozu chat --help`)");
        let id_ = choice.provider.id;
        let provider: Arc<dyn Provider> = if id_ == CLAUDE_CLI_ID {
            let (rozu, args) = mcp(id_)?;
            let binary = find_claude_binary().ok_or_else(|| missing("claude"))?;
            let config = ClaudeCliConfig::new(binary, self.root(), rozu, args, gate.clone());
            Arc::new(ClaudeCliProvider::new(config)?)
        } else if id_ == CODEX_ID {
            let (rozu, args) = mcp(id_)?;
            let binary = find_codex_binary().ok_or_else(|| missing("codex"))?;
            let config = CodexConfig::new(binary, self.workspace.clone(), rozu, args, exec(id_));
            Arc::new(CodexAppServerProvider::new(config)?)
        } else if id_ == GROK_ID {
            let (rozu, args) = mcp(id_)?;
            let binary = find_grok_binary().map_err(|err| anyhow!(err))?;
            let config = GrokConfig::new(binary, self.workspace.clone(), &rozu, args, exec(id_));
            Arc::new(GrokSession::new(config)?)
        } else {
            let (key, _) =
                resolve_api_key(&KeyringStore::default(), id_, |v| std::env::var(v).ok())?
                    .ok_or_else(|| {
                        anyhow!(
                            "no {} API key: run `rozu auth set {id_}`",
                            choice.provider.label
                        )
                    })?;
            registry::build(choice.provider, key, None, None)?
        };
        let config = AgentConfig::new(&choice.model).with_model_info(choice.info.clone());
        Ok(
            Agent::new(provider, self.tools.clone(), self.workspace.clone(), config)
                .with_approval_policy(self.policy.clone())
                .with_approval_gate(gate)
                .with_audit(audit)
                .with_session_temp(temp),
        )
    }
}

/// The models this Mac can use: installed subscription CLIs first, then API keys that are set.
pub fn choices() -> Vec<ModelChoice> {
    let store = KeyringStore::default();
    let mut all = PROVIDERS
        .iter()
        .filter(|p| match p.id {
            CLAUDE_CLI_ID => find_claude_binary().is_some_and(|p| p.exists()),
            CODEX_ID => find_codex_binary().is_some_and(|p| p.exists()),
            GROK_ID => find_grok_binary().is_ok_and(|p| p.exists()),
            id => matches!(
                resolve_api_key(&store, id, |v| std::env::var(v).ok()),
                Ok(Some(_))
            ),
        })
        .filter_map(|p| registry::resolve(Some(p.id), None).ok())
        .collect::<Vec<_>>();
    all.sort_by_key(|c| !c.provider.is_cli_backed());
    all
}

/// How the composer names a model.
pub fn label(choice: &ModelChoice) -> String {
    let name = match choice.provider.id {
        CLAUDE_CLI_ID => "Claude",
        CODEX_ID => "ChatGPT",
        GROK_ID => "Grok",
        _ => choice.provider.label,
    };
    match choice.model.as_str() {
        "default" => name.to_owned(),
        model => format!("{name} {model}"),
    }
}

/// `rozu` (for `rozu mcp-serve`): next to this binary, else on PATH.
fn rozu_binary() -> anyhow::Result<PathBuf> {
    let sibling = std::env::current_exe()?.with_file_name("rozu");
    std::iter::once(sibling)
        .chain(
            std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
                .map(|d| d.join("rozu")),
        )
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("the rozu CLI wasn't found: run `cargo build -p rozu-cli`"))
}

fn system_prompt(root: &Path) -> String {
    format!(
        "You are rozu, a coding agent running in the rozu desktop app.\n\
         The workspace root is {}; tool paths are relative to it.\n\
         Shell commands run in a sandbox that only allows writes in the workspace and $TMPDIR and \
         blocks the network; set network: true when a command needs it. Calls that need approval \
         are shown to the user, who may deny them with a reason; respect it and adapt.\n\
         Read files before editing them, keep changes minimal, and run the relevant tests after \
         changing code. Be concise, and cite file paths when you refer to code.",
        root.display()
    )
}
