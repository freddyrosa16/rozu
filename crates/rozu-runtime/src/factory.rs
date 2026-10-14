//! Construction and lifetime of the tools and provider sessions shared by a chat.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
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
    Agent, AgentConfig, ApprovalGate, ApprovalMode, ApprovalPolicy, AuditLog, DEFAULT_MAX_STEPS,
    MemoryPolicy, Provider, ToolExecutor, approvals_path,
};
use rozu_providers::ModelChoice;
use rozu_providers::registry::{self, CODEX_ID, GROK_ID};
use rozu_secrets::{KeyringStore, env_var_for, resolve_api_key};
use rozu_tools::{SessionTemp, ToolRegistry, Workspace, platform_sandbox};

use crate::{approval_arg, base_url_override};

/// If set to a byte count, Grok gets rozu's `PostToolUse` hook (`rozu grok-hook`), which keeps
/// the head and tail of rozu tool results longer than that (unverified with Grok Build; off by
/// default, since `rozu mcp-serve` already caps tool output).
pub const GROK_HOOK_MAX_BYTES_ENV: &str = "ROZU_GROK_HOOK_MAX_BYTES";

/// Builds an [`Agent`] for a model choice; kept around so a model switch mid-chat can rebuild
/// while the vendor CLI sessions (one per chat) stay up.
pub struct AgentFactory {
    store: KeyringStore,
    tools: Arc<ToolRegistry>,
    workspace: Workspace,
    pub max_steps: usize,
    pub memory: MemoryPolicy,
    /// Reasoning effort for the vendor CLIs that take one (claude-cli, codex).
    pub effort: Option<String>,
    /// For grok: run each turn as a headless `grok -p` instead of the long-lived ACP session.
    pub grok_headless: bool,
    mode: ApprovalMode,
    policy: Arc<ApprovalPolicy>,
    gate: Arc<dyn ApprovalGate>,
    /// The `rozu` binary vendor CLIs start as `rozu mcp-serve`.
    rozu: Option<PathBuf>,
    /// One claude CLI session for the whole chat, kept across model switches.
    claude: Mutex<Option<Arc<ClaudeCliProvider>>>,
    /// One codex app-server (and thread) for the whole chat, kept across model switches.
    codex: Mutex<Option<Arc<CodexAppServerProvider>>>,
    /// One Grok Build process (and its context epochs) for the whole chat.
    grok: Mutex<Option<Arc<dyn Provider>>>,
    /// Ties this chat's audit entries (and those of its `rozu mcp-serve`) together.
    session_id: String,
    audit: Option<Arc<AuditLog>>,
    /// The session's private temp dir, shared by every agent of the chat.
    temp: Option<Arc<SessionTemp>>,
}

impl AgentFactory {
    /// Reads the project's approval rules and creates the session temp dir. Vendor CLIs get
    /// this process as the `rozu` binary unless [`AgentFactory::with_rozu_binary`] says
    /// otherwise.
    pub fn new(
        workspace: &Workspace,
        mode: ApprovalMode,
        gate: Arc<dyn ApprovalGate>,
        session_id: String,
    ) -> anyhow::Result<Self> {
        let policy = ApprovalPolicy::for_project(mode, workspace.root()).with_context(|| {
            format!(
                "cannot read the approval rules in {}; fix or delete the file",
                approvals_path(workspace.root()).display()
            )
        })?;
        let temp = match SessionTemp::create(&session_id) {
            Ok(temp) => Some(Arc::new(temp)),
            Err(err) => {
                tracing::warn!(error = %err, "could not create the session temp dir");
                None
            }
        };
        Ok(Self {
            store: KeyringStore::default(),
            tools: Arc::new(ToolRegistry::standard()),
            workspace: workspace.clone(),
            max_steps: DEFAULT_MAX_STEPS,
            memory: MemoryPolicy::DEFAULT,
            effort: None,
            grok_headless: false,
            mode,
            policy: Arc::new(policy),
            gate,
            rozu: None,
            claude: Mutex::default(),
            codex: Mutex::default(),
            grok: Mutex::default(),
            audit: AuditLog::from_env(&session_id).map(Arc::new),
            session_id,
            temp,
        })
    }

    /// The `rozu` binary vendor CLIs start for rozu's tools (the app isn't one itself).
    pub fn with_rozu_binary(mut self, path: PathBuf) -> Self {
        self.set_rozu_binary(path);
        self
    }

    /// Sets the tool-server binary before starting a vendor provider in an existing chat.
    pub fn set_rozu_binary(&mut self, path: PathBuf) {
        self.rozu = Some(path);
    }

    /// Updates reasoning effort while retaining this chat's tools, policy and private temp dir.
    /// Providers whose startup configuration includes effort are recreated on their next use.
    pub fn set_effort(&mut self, effort: Option<String>) {
        if self.effort != effort {
            self.effort = effort;
            *self.claude.get_mut().expect("claude provider lock") = None;
            *self.codex.get_mut().expect("codex provider lock") = None;
        }
    }

    pub fn mode(&self) -> ApprovalMode {
        self.mode
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }

    /// Resolves the provider's key (env var first, then the keychain), or for claude-cli, codex
    /// and grok the vendor CLI, and builds the agent.
    pub fn build(&self, choice: &ModelChoice) -> anyhow::Result<Agent> {
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
        config.memory = self.memory;
        Ok(
            Agent::new(provider, self.tools.clone(), self.workspace.clone(), config)
                .with_approval_policy(self.policy.clone())
                .with_approval_gate(self.gate.clone())
                .with_audit(self.audit.clone())
                .with_session_temp(self.temp.clone()),
        )
    }

    fn rozu_binary(&self) -> anyhow::Result<PathBuf> {
        match &self.rozu {
            Some(path) => Ok(path.clone()),
            None => std::env::current_exe().context("cannot locate the rozu binary"),
        }
    }

    /// A tool executor for a vendor CLI's own approval requests (Codex, Grok).
    fn executor(&self, provider: &str) -> ToolExecutor {
        let mut exec = ToolExecutor::new(
            self.tools.clone(),
            self.gate.clone(),
            self.policy.clone(),
            platform_sandbox(),
        )
        .with_audit(self.audit.clone())
        .with_provider(Some(provider.to_owned()));
        exec.temp = self.temp.clone();
        exec
    }

    /// The claude CLI provider, started once. Claude reaches rozu's tools through
    /// `rozu mcp-serve`, which asks this factory's gate when approval is needed.
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
        let mut config = ClaudeCliConfig::new(
            binary,
            self.workspace.root(),
            self.rozu_binary()?,
            self.mcp_args(None),
            self.gate.clone(),
        );
        config.effort = self.effort.clone();
        let provider = Arc::new(
            ClaudeCliProvider::new(config).context("could not prepare the claude CLI session")?,
        );
        *cached = Some(provider.clone());
        Ok(provider)
    }

    /// The codex app-server provider, started once. Codex's own approval requests are decided
    /// here (policy, then the gate); rozu's tools reach it through `rozu mcp-serve --provider
    /// codex`.
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
        let mut config = CodexConfig::new(
            binary,
            self.workspace.clone(),
            self.rozu_binary()?,
            self.mcp_args(Some(CODEX_ID)),
            self.executor(CODEX_ID),
        );
        config.effort = self.effort.clone();
        let provider = Arc::new(
            CodexAppServerProvider::new(config)
                .context("could not prepare the codex app-server session")?,
        );
        *cached = Some(provider.clone());
        Ok(provider)
    }

    /// The Grok Build provider, started once: the long-lived ACP session (default) or the
    /// headless fallback. Grok's built-ins are off; its only tools are rozu's, through
    /// `rozu mcp-serve --provider grok`, which asks the gate.
    fn grok_provider(&self) -> anyhow::Result<Arc<dyn Provider>> {
        let mut cached = self.grok.lock().expect("grok provider lock");
        if let Some(provider) = cached.as_ref() {
            return Ok(provider.clone());
        }
        let binary = find_grok_binary().map_err(|err| anyhow!(err))?;
        let rozu = self.rozu_binary()?;
        let mut config = GrokConfig::new(
            binary,
            self.workspace.clone(),
            &rozu,
            self.mcp_args(Some(GROK_ID)),
            self.executor(GROK_ID),
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
        let mut args = vec![
            "mcp-serve".to_owned(),
            "--workspace".to_owned(),
            self.workspace.root().display().to_string(),
            "--approval".to_owned(),
            approval_arg(self.mode).to_owned(),
            "--session-id".to_owned(),
            self.session_id.clone(),
        ];
        if let Some(provider) = provider {
            args.extend(["--provider".to_owned(), provider.to_owned()]);
        }
        args
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rozu_core::ReadOnlyGate;

    #[test]
    fn changing_effort_and_cli_binary_keeps_the_chat_scratch_files() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut factory = AgentFactory::new(
            &workspace,
            ApprovalMode::ReadOnly,
            Arc::new(ReadOnlyGate),
            "same-chat".into(),
        )
        .unwrap();
        let scratch = factory.temp.as_ref().unwrap().path().join("scratch.txt");
        std::fs::write(&scratch, "needed next turn").unwrap();

        factory.set_effort(Some("high".into()));
        factory.set_rozu_binary(PathBuf::from("/tools/rozu"));
        factory.set_effort(None);

        assert_eq!(
            std::fs::read_to_string(&scratch).unwrap(),
            "needed next turn"
        );
        assert_eq!(factory.rozu_binary().unwrap(), PathBuf::from("/tools/rozu"));
        assert_eq!(factory.session_id(), "same-chat");
        drop(factory);
        assert!(!scratch.exists());
    }
}
