//! The app's link to rozu-core, built like `rozu chat` builds it: one agent and session per
//! chat, on a shared tokio runtime. Events and approval prompts come back as [`Update`]s.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use rozu_activity::ActivityEvent;
use rozu_cli_providers::claude::install::{claude_auth_status, find_claude_binary};
use rozu_cli_providers::codex::install::{
    LoginStatus, codex_login_status, find_codex_binary, list_models,
};
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
use rozu_providers::{ModelChoice, PROVIDERS, ProviderInfo};
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
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            rt: tokio::runtime::Runtime::new()?,
            tools: Arc::new(ToolRegistry::standard()),
        })
    }

    /// The models the installed Codex CLI offers; empty when it can't say.
    pub fn codex_models(&self) -> oneshot::Receiver<Live> {
        let (tx, rx) = oneshot::channel();
        self.rt.spawn(async move {
            let models = match find_codex_binary().filter(|p| p.exists()) {
                Some(bin) => list_models(&bin).await.unwrap_or_default(),
                None => Vec::new(),
            };
            let _ = tx.send(models.into_iter().map(|m| (m.id, m.display_name)).collect());
        });
        rx
    }

    /// Whether a subscription CLI is installed and signed in.
    pub fn check(&self, id: &'static str) -> oneshot::Receiver<Access> {
        let (tx, rx) = oneshot::channel();
        self.rt.spawn(async move {
            let _ = tx.send(access(id).await);
        });
        rx
    }

    pub fn start(
        &self,
        root: &Path,
        choice: &ModelChoice,
        effort: Option<String>,
        updates: mpsc::UnboundedSender<Update>,
    ) -> Chat {
        let (prompts, mut rx) = mpsc::unbounded_channel::<String>();
        let cancel = Arc::new(Mutex::new(CancellationToken::new()));
        let agent = {
            let _rt = self.rt.enter();
            self.agent(root, choice, effort, Arc::new(Gate(updates.clone())))
        };
        let prompt = system_prompt(root);
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

    fn agent(
        &self,
        root: &Path,
        choice: &ModelChoice,
        effort: Option<String>,
        gate: Arc<dyn ApprovalGate>,
    ) -> anyhow::Result<Agent> {
        let workspace = Workspace::new(root)
            .with_context(|| format!("cannot open the folder {}", root.display()))?;
        let policy = Arc::new(ApprovalPolicy::for_project(
            ApprovalMode::AutoWorkspace,
            workspace.root(),
        )?);
        let id = new_session_id();
        let audit = AuditLog::from_env(&id).map(Arc::new);
        let temp = SessionTemp::create(&id).ok().map(Arc::new);
        let exec = |provider: &str| {
            let mut exec = ToolExecutor::new(
                self.tools.clone(),
                gate.clone(),
                policy.clone(),
                platform_sandbox(),
            )
            .with_audit(audit.clone())
            .with_provider(Some(provider.to_owned()));
            exec.temp = temp.clone();
            exec
        };
        let mcp = |provider: &str| -> anyhow::Result<(PathBuf, Vec<String>)> {
            let root = workspace.root().display().to_string();
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
            let mut config =
                ClaudeCliConfig::new(binary, workspace.root(), rozu, args, gate.clone());
            config.effort = effort;
            Arc::new(ClaudeCliProvider::new(config)?)
        } else if id_ == CODEX_ID {
            let (rozu, args) = mcp(id_)?;
            let binary = find_codex_binary().ok_or_else(|| missing("codex"))?;
            let mut config = CodexConfig::new(binary, workspace.clone(), rozu, args, exec(id_));
            config.effort = effort;
            Arc::new(CodexAppServerProvider::new(config)?)
        } else if id_ == GROK_ID {
            let (rozu, args) = mcp(id_)?;
            let binary = find_grok_binary().map_err(|err| anyhow!(err))?;
            let config = GrokConfig::new(binary, workspace.clone(), &rozu, args, exec(id_));
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
        Ok(Agent::new(provider, self.tools.clone(), workspace, config)
            .with_approval_policy(policy)
            .with_approval_gate(gate)
            .with_audit(audit)
            .with_session_temp(temp))
    }
}

/// A subscription the app can sign in with: its CLI, and how to install it and sign in.
pub struct Plan {
    pub id: &'static str,
    pub name: &'static str,
    pub plan: &'static str,
    pub install: &'static str,
    pub login: &'static str,
}

pub const PLANS: &[Plan] = &[
    Plan {
        id: CLAUDE_CLI_ID,
        name: "Claude",
        plan: "Claude Pro or Max, through Claude Code",
        install: "curl -fsSL https://claude.ai/install.sh | bash",
        login: "claude auth login",
    },
    Plan {
        id: CODEX_ID,
        name: "ChatGPT",
        plan: "ChatGPT Plus or Pro, through Codex",
        install: "npm install -g @openai/codex",
        login: "codex login",
    },
    Plan {
        id: GROK_ID,
        name: "Grok",
        plan: "SuperGrok, through Grok Build",
        install: "npm install -g @xai-official/grok",
        login: "grok login",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Checking,
    Missing,
    SignedOut,
    Ready,
    /// Installed; Grok Build can't say whether it is signed in.
    Installed,
}

async fn access(id: &str) -> Access {
    match id {
        CLAUDE_CLI_ID => match find_claude_binary().filter(|p| p.exists()) {
            None => Access::Missing,
            Some(bin) => match claude_auth_status(&bin).await.map(|s| s.logged_in) {
                Ok(Some(false)) => Access::SignedOut,
                Ok(Some(true)) => Access::Ready,
                _ => Access::Installed,
            },
        },
        CODEX_ID => match find_codex_binary().filter(|p| p.exists()) {
            None => Access::Missing,
            Some(bin) => match codex_login_status(&bin).await {
                Ok(LoginStatus::NotLoggedIn) => Access::SignedOut,
                Ok(LoginStatus::ChatGpt | LoginStatus::ApiKey) => Access::Ready,
                _ => Access::Installed,
            },
        },
        _ => match find_grok_binary() {
            Ok(p) if p.exists() => Access::Installed,
            _ => Access::Missing,
        },
    }
}

/// Runs `command` in a new Terminal window (installs and sign-ins need one).
pub fn in_terminal(command: &str) {
    let script = format!(
        "tell application \"Terminal\"\nactivate\ndo script \"{}\"\nend tell",
        command.replace('\\', "\\\\").replace('"', "\\\"")
    );
    let _ = std::process::Command::new("osascript")
        .args(["-e", &script])
        .spawn();
}

/// The providers this Mac can use: installed subscription CLIs first, then API keys that are set.
pub fn providers() -> Vec<&'static ProviderInfo> {
    let store = KeyringStore::default();
    let mut all: Vec<_> = PROVIDERS
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
        .collect();
    all.sort_by_key(|p| !p.is_cli_backed());
    all
}

/// Models a CLI reported itself (Codex's `model/list`): id and display name.
pub type Live = Vec<(String, String)>;

/// Every model `provider` offers; Codex's own list when it gave one.
pub fn models(provider: &ProviderInfo, live: &Live) -> Vec<ModelChoice> {
    let ids: Vec<String> = if provider.id == CODEX_ID && !live.is_empty() {
        std::iter::once("default".to_owned())
            .chain(live.iter().map(|(id, _)| id.clone()))
            .collect()
    } else if provider.id == CLAUDE_CLI_ID {
        claude_ids()
    } else {
        registry::models_for(provider.id)
            .into_iter()
            .map(|m| m.id)
            .collect()
    };
    ids.iter()
        .filter_map(|id| registry::resolve(Some(provider.id), Some(id)).ok())
        .collect()
}

/// Every Claude model by its full id, newest first, so the version shows.
fn claude_ids() -> Vec<String> {
    registry::models_for("anthropic")
        .into_iter()
        .map(|m| m.id)
        .collect()
}

/// Swaps a Claude CLI alias (`opus`) saved by older builds for the model it stands for.
pub fn pin(mut choice: ModelChoice) -> ModelChoice {
    if choice.provider.id == CLAUDE_CLI_ID
        && let Some(id) = claude_ids()
            .into_iter()
            .find(|id| id.starts_with(&format!("claude-{}-", choice.model)))
    {
        choice = registry::resolve(Some(CLAUDE_CLI_ID), Some(&id)).unwrap_or(choice);
    }
    choice
}

/// The effort levels a provider takes; empty when rozu can't set it.
pub fn efforts(provider: &str) -> &'static [&'static str] {
    match provider {
        CLAUDE_CLI_ID => &["low", "medium", "high", "xhigh", "max"],
        CODEX_ID => &["low", "medium", "high", "xhigh"],
        _ => &[],
    }
}

/// How a provider is named in the app.
pub fn provider_name(provider: &ProviderInfo) -> &'static str {
    PLANS
        .iter()
        .find(|p| p.id == provider.id)
        .map_or(provider.label, |p| p.name)
}

/// How a model is named in the app: the CLI's own name, else `opus` → `Opus`,
/// `gpt-6.1-sol` → `GPT-6.1 Sol`, `claude-opus-5-5` → `Opus 5.5`.
pub fn model_name(model: &str, live: &Live) -> String {
    if let Some((_, name)) = live.iter().find(|(id, _)| id == model) {
        return name.clone();
    }
    let model = model.strip_prefix("claude-").unwrap_or(model);
    let mut words: Vec<String> = Vec::new();
    for part in model.split('-') {
        let short_num = |s: &str| s.len() <= 2 && s.chars().all(|c| c.is_ascii_digit());
        match words.last_mut() {
            Some(last) if short_num(part) && short_num(last.rsplit('.').next().unwrap_or("")) => {
                last.push('.');
                last.push_str(part);
            }
            Some(last) if last == "GPT" => {
                last.push('-');
                last.push_str(part);
            }
            _ => words.push(match part {
                "gpt" => "GPT".to_owned(),
                _ => {
                    let mut c = part.chars();
                    c.next()
                        .map(|f| f.to_uppercase().chain(c).collect())
                        .unwrap_or_default()
                }
            }),
        }
    }
    words.join(" ")
}

/// How the composer names a model: `Claude Opus · high`.
pub fn label(choice: &ModelChoice, effort: Option<&str>, live: &Live) -> String {
    let name = provider_name(choice.provider);
    let model = match choice.model.as_str() {
        "default" => name.to_owned(),
        model => format!("{name} {}", model_name(model, live)),
    };
    match effort {
        Some(effort) => format!("{model} · {effort}"),
        None => model,
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_models_like_the_app() {
        let none = Live::new();
        let opus = pin(registry::resolve(Some(CLAUDE_CLI_ID), Some("opus")).unwrap());
        assert_eq!(label(&opus, Some("high"), &none), "Claude Opus 5.5 · high");
        let codex = registry::resolve(Some(CODEX_ID), None).unwrap();
        assert_eq!(label(&codex, None, &none), "ChatGPT");
        for (id, name) in [
            ("claude-opus-5-5", "Opus 5.5"),
            ("gpt-6.1-sol", "GPT-6.1 Sol"),
            ("grok-4.7-build", "Grok 4.7 Build"),
            ("default", "Default"),
        ] {
            assert_eq!(model_name(id, &none), name);
        }
        let live = vec![("gpt-7".to_owned(), "GPT-7 Max".to_owned())];
        assert_eq!(model_name("gpt-7", &live), "GPT-7 Max");
        let codex = registry::provider_info(CODEX_ID).unwrap();
        let ids: Vec<_> = models(codex, &live).into_iter().map(|m| m.model).collect();
        assert_eq!(ids, ["default", "gpt-7"]);
        let names: Vec<_> = models(registry::provider_info(CLAUDE_CLI_ID).unwrap(), &none)
            .iter()
            .map(|m| model_name(&m.model, &none))
            .collect();
        assert_eq!(
            names[..4],
            ["Fable 5.1", "Opus 5.5", "Sonnet 5.5", "Haiku 5.5"]
        );
        assert!(names.contains(&"Opus 4.6".to_owned()));
        assert!(efforts(GROK_ID).is_empty());
    }
}
