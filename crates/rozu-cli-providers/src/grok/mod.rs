//! Grok through xAI's official **Grok Build** CLI on the user's SuperGrok subscription (the CLI
//! does its own `grok login`; rozu never touches its credentials).
//!
//! rozu spawns the native binary (never the Node launcher, see [`install`]) as one long-lived
//! `grok --no-auto-update agent [-m <model>] --agent-profile <profile.md> stdio` and speaks ACP
//! (JSON-RPC over stdio, see [`protocol`]) with it:
//!
//! - The agent profile rozu writes (frozen for the provider's lifetime) allows only `use_tool`,
//!   so Grok's built-in tools are off. rozu's MCP server (`rozu mcp-serve --provider grok`) is
//!   the only toolset, passed in `session/new`'s `mcpServers`; it applies rozu's policy,
//!   sandbox and audit log to every call. The rozu system profile, which lists the `rozu__<tool>`
//!   names and schemas so the model calls `use_tool` directly, goes in
//!   `_meta.systemPromptOverride`.
//! - `initialize` advertises no file system and no terminal; the only auth method rozu picks is
//!   `cached_token` (the login `grok login` cached), never an API key.
//! - Grok's `session/request_permission` requests are approved (once) only for `use_tool` →
//!   `rozu__*`; everything else is denied (see [`permissions`]).
//! - **Context epochs:** Grok's prompt cache is per session (82% hits for one append-only
//!   session vs 23% for a new session per turn, measured with Grok Build 1.0.52), so turns are
//!   appended to one ACP session. A new session (in the same process) starts only when rozu
//!   rewrites the history it sent — masking, compaction, a profile refresh: call
//!   [`GrokSession::rotate_epoch`], and the next turn opens a session with the rewritten history.
//! - Cancel sends `session/cancel`; the process and session survive if the turn ends in time.
//!
//! Usage is counted from the `session/prompt` result's `_meta` (normalized so `input_tokens` is
//! the whole prompt), never priced. A headless fallback lives in [`headless`].

pub mod headless;
pub mod hook;
pub mod install;
pub mod permissions;
pub mod protocol;

use std::ffi::OsString;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rozu_core::{
    ChatRequest, EventStream, Provider, ProviderError, ProviderKind, ProviderResult, StreamEvent,
    ToolExecutor,
};
use rozu_mcp::{APPROVAL_SOCKET_ENV, ApprovalBridge, SERVER_NAME};
use rozu_tools::{ToolRegistry, Workspace};
use serde_json::{Value, json};
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc};
use tracing::{debug, info, warn};

use crate::process::{CliCommand, CliProcess, DEFAULT_MAX_LINE_BYTES, ReadError};
use crate::turn::{system_prompt, turn_text_with};
use protocol::{Incoming, LOGIN_HINT, Mapper};

/// Must match the id in `rozu_providers::registry`, which lists this provider's models.
pub const PROVIDER_ID: &str = "grok";
/// The model id meaning "Grok's own default": no `-m` is passed.
pub const DEFAULT_MODEL: &str = "default";
/// Introduces the history replayed into a new session.
const HISTORY_HEADER: &str = "Earlier in this conversation:";

/// The agent profile rozu starts Grok with: only the `use_tool` meta-tool (Grok can't remove it
/// or `search_tool`), so none of Grok's built-ins (shell, file edits, web) are available.
pub fn agent_profile(post_tool_hook: Option<&str>) -> String {
    let mut profile = String::from(
        "---\nname: rozu-backend\ndescription: Thin model backend for rozu; only rozu MCP tools.\n\
         tools: use_tool\n",
    );
    if let Some(command) = post_tool_hook {
        // JSON is valid YAML; Grok's hook object shape (unverified inside agent frontmatter).
        profile.push_str(&format!("hooks: {}\n", hook::config(command)));
    }
    profile.push_str("---\nYou are rozu's model backend.\n");
    profile
}

/// The part of the system profile that tells Grok how to reach rozu's tools: their MCP names
/// and input schemas, so the model calls `use_tool` directly instead of `search_tool` first.
pub fn tool_note(tools: &ToolRegistry) -> String {
    let mut note = String::from(
        "Your only tools are rozu's, served by the `rozu` MCP server. Call them directly with \
         use_tool, without search_tool first: use_tool {\"tool_name\": \"rozu__<name>\", \
         \"tool_input\": {…}}. Paths are relative to the workspace root. rozu runs every call \
         under its own sandbox and approval policy; the user may deny a call with a reason, so \
         respect it and adapt.\nTools:",
    );
    for tool in tools.iter() {
        note.push_str(&format!(
            "\n- rozu__{}: {} Input schema: {}",
            tool.name(),
            tool.description(),
            tool.schema()
        ));
    }
    note
}

/// Why rozu starts a new Grok session (a new context epoch).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EpochReason {
    /// Old tool outputs were masked (batched, so the cache survives in between).
    Masking,
    /// Older turns were compacted into a summary.
    Compaction,
    /// The frozen system profile changed.
    ProfileRefresh,
    Other(String),
}

impl fmt::Display for EpochReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EpochReason::Masking => f.write_str("masking"),
            EpochReason::Compaction => f.write_str("compaction"),
            EpochReason::ProfileRefresh => f.write_str("profile refresh"),
            EpochReason::Other(reason) => f.write_str(reason),
        }
    }
}

pub struct GrokConfig {
    /// The native Grok binary (see [`install::find_grok_binary`]).
    pub binary: PathBuf,
    /// The workspace: Grok's working directory and the root rozu checks paths against.
    pub workspace: Workspace,
    /// Command line of rozu's MCP server, e.g. `/path/to/rozu mcp-serve … --provider grok`.
    pub mcp_command: PathBuf,
    pub mcp_args: Vec<String>,
    /// Logs the decisions on Grok's permission requests (tag it with `provider: grok`); its
    /// registry is what the system profile lists.
    pub exec: ToolExecutor,
    /// Extra environment for the CLI (tests).
    pub env: Vec<(String, String)>,
    pub max_line_bytes: usize,
    /// How long starting Grok (initialize, authenticate) or a session may take.
    pub handshake_timeout: Duration,
    /// How long to wait for a cancelled turn to end before killing Grok.
    pub cancel_grace: Duration,
    /// A `PostToolUse` hook command (see [`hook`]) put in the agent profile; `None` (the
    /// default) leaves rozu's MCP output untouched (it is already truncated by `rozu mcp-serve`).
    pub post_tool_hook: Option<String>,
}

impl GrokConfig {
    pub fn new(
        binary: impl Into<PathBuf>,
        workspace: Workspace,
        mcp_command: impl Into<PathBuf>,
        mcp_args: Vec<String>,
        exec: ToolExecutor,
    ) -> Self {
        Self {
            binary: binary.into(),
            workspace,
            mcp_command: mcp_command.into(),
            mcp_args,
            exec,
            env: Vec::new(),
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            handshake_timeout: Duration::from_secs(60),
            cancel_grace: Duration::from_secs(10),
            post_tool_hook: None,
        }
    }

    /// The command for a CLI-backed Grok run: the native binary with Grok's API-key and proxy
    /// variables removed.
    pub(crate) fn command(&self, args: Vec<OsString>) -> CliCommand {
        let mut cmd = CliCommand::new("grok", &self.binary);
        cmd.args = args;
        cmd.cwd = Some(self.workspace.root().to_path_buf());
        cmd.env_remove = install::ENV_REMOVE.iter().map(OsString::from).collect();
        // Not even explicit extra environment may pass an API key or a proxy.
        cmd.env = self
            .env
            .iter()
            .filter(|(k, _)| !install::ENV_REMOVE.contains(&k.as_str()))
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        cmd.max_line_bytes = self.max_line_bytes;
        cmd
    }

    pub(crate) fn spawn_error(&self, err: &io::Error) -> ProviderError {
        ProviderError::CliNotFound {
            cli: "grok".into(),
            message: format!(
                "{} can't be run ({err}); install Grok Build or set {}",
                self.binary.display(),
                install::GROK_BIN_ENV
            ),
        }
    }

    /// The session's system profile: the system messages plus [`tool_note`].
    pub fn system_profile(&self, req: &ChatRequest) -> String {
        system_prompt(req, &tool_note(&self.exec.tools))
    }
}

struct State {
    process: Option<CliProcess>,
    /// The model the running process was started with.
    model: Option<String>,
    next_id: u64,
    /// The current session (context epoch); `None` until `session/new` succeeded.
    mapper: Option<Mapper>,
    /// Sessions opened so far.
    epoch: u64,
    /// Set by [`GrokSession::rotate_epoch`]: the next turn opens a new session.
    rotate: Option<EpochReason>,
    /// Decision-log entries from `rozu mcp-serve` and the prompts the bridge opened.
    records: mpsc::UnboundedReceiver<StreamEvent>,
}

/// The Grok Build ACP provider: one process, one session per context epoch.
pub struct GrokSession {
    config: Arc<GrokConfig>,
    /// Holds the agent profile and the approval socket; removed on drop.
    runtime_dir: tempfile::TempDir,
    profile: PathBuf,
    bridge: Option<ApprovalBridge>,
    state: Arc<Mutex<State>>,
}

impl GrokSession {
    /// Writes the agent profile and starts the approval bridge for `rozu mcp-serve`. Needs a
    /// tokio runtime. Grok itself starts with the first turn.
    pub fn new(config: GrokConfig) -> io::Result<Self> {
        // Short base path: Unix socket paths are limited to ~100 bytes.
        let base = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let runtime_dir = tempfile::Builder::new()
            .prefix("rozu-grok-")
            .tempdir_in(base)?;
        let (records_tx, records) = mpsc::unbounded_channel();
        let socket = runtime_dir.path().join("approval.sock");
        let bridge = match ApprovalBridge::bind(
            &socket,
            PROVIDER_ID,
            config.exec.gate.clone(),
            records_tx,
        ) {
            Ok(bridge) => Some(bridge),
            Err(err) => {
                warn!(error = %err, "no approval bridge; prompts from rozu's MCP tools will be denied");
                None
            }
        };
        let profile = runtime_dir.path().join("rozu-agent.md");
        std::fs::write(&profile, agent_profile(config.post_tool_hook.as_deref()))?;
        Ok(Self {
            config: Arc::new(config),
            runtime_dir,
            profile,
            bridge,
            state: Arc::new(Mutex::new(State {
                process: None,
                model: None,
                next_id: 1,
                mapper: None,
                epoch: 0,
                rotate: None,
                records,
            })),
        })
    }

    /// The directory holding the agent profile and the approval socket.
    pub fn runtime_dir(&self) -> &Path {
        self.runtime_dir.path()
    }

    /// The agent profile file Grok is started with.
    pub fn profile_path(&self) -> &Path {
        &self.profile
    }

    /// The arguments Grok is started with for `model`.
    pub fn args(&self, model: &str) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec!["--no-auto-update".into(), "agent".into()];
        if model != DEFAULT_MODEL {
            args.push("-m".into());
            args.push(model.into());
        }
        args.push("--agent-profile".into());
        args.push(self.profile.clone().into());
        args.push("stdio".into());
        args
    }

    /// `session/new`'s `mcpServers`: rozu's server only, with the approval socket.
    pub fn mcp_servers(&self) -> Vec<Value> {
        let env: Vec<(String, String)> = self
            .bridge
            .iter()
            .map(|b| {
                (
                    APPROVAL_SOCKET_ENV.to_owned(),
                    b.path().display().to_string(),
                )
            })
            .collect();
        vec![protocol::mcp_server(
            SERVER_NAME,
            &self.config.mcp_command.display().to_string(),
            &self.config.mcp_args,
            &env,
        )]
    }

    /// Starts a new context epoch with the next turn: a new ACP session in the same process,
    /// sent the system profile and the history of that turn's request (which rozu has
    /// rewritten). Waits for a running turn to finish. Without a session yet, nothing changes.
    pub async fn rotate_epoch(&self, reason: EpochReason) {
        let mut state = self.state.lock().await;
        if state.mapper.is_some() {
            info!(target: "rozu::cli", epoch = state.epoch, %reason, "grok: new context epoch requested");
            state.rotate = Some(reason);
        }
    }

    /// Sessions opened so far (the current epoch's number).
    pub async fn epoch(&self) -> u64 {
        self.state.lock().await.epoch
    }

    /// The current ACP session id, if any.
    pub async fn session_id(&self) -> Option<String> {
        self.state
            .lock()
            .await
            .mapper
            .as_ref()
            .map(|m| m.session_id().to_owned())
    }

    /// Starts Grok: `initialize`, then `authenticate` with `cached_token` if offered.
    async fn start(&self, state: &mut State, model: &str) -> ProviderResult<()> {
        if let Some(mut old) = state.process.take() {
            old.kill();
        }
        state.mapper = None;
        state.model = None;
        let cmd = self.config.command(self.args(model));
        info!(target: "rozu::cli", model, "starting grok agent stdio");
        let process = CliProcess::spawn(&cmd).map_err(|err| self.config.spawn_error(&err))?;
        state.process = Some(process);

        let handshake = async {
            let init = self
                .call(state, "initialize", protocol::initialize_params())
                .await?
                .map_err(|message| protocol::classify_error(&message))?;
            info!(target: "rozu::cli", protocol = %init["protocolVersion"],
                agent = %init["agentInfo"], "grok initialized");
            match protocol::auth_method(&init) {
                Some(method) => {
                    let result = self
                        .call(state, "authenticate", json!({ "methodId": method }))
                        .await?;
                    if let Err(message) = result {
                        info!(target: "rozu::cli", %message, "grok cached login unavailable");
                        return Err(ProviderError::NotLoggedIn {
                            cli: "grok".into(),
                            hint: LOGIN_HINT.into(),
                        });
                    }
                }
                // Signed-in CLIs may need no `authenticate`; `session/new` says if they do.
                None => debug!(target: "rozu::cli", "grok offers no cached_token auth method"),
            }
            Ok(())
        };
        let result = self.timed(handshake, "grok didn't start in time").await;
        match result {
            Ok(()) => state.model = Some(model.to_owned()),
            Err(_) => {
                if let Some(mut process) = state.process.take() {
                    process.kill();
                }
            }
        }
        result
    }

    /// Opens the next context epoch's session.
    async fn open_session(&self, state: &mut State, req: &ChatRequest) -> ProviderResult<()> {
        let reason = state.rotate.take();
        let params = protocol::session_new_params(
            &self.config.workspace.root().display().to_string(),
            self.mcp_servers(),
            &self.config.system_profile(req),
        );
        let opened = async {
            let result = self
                .call(state, "session/new", params)
                .await?
                .map_err(|message| protocol::classify_error(&message))?;
            result["sessionId"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| ProviderError::Decode("session/new returned no sessionId".into()))
        };
        let session = self
            .timed(opened, "grok didn't open a session in time")
            .await;
        match session {
            Ok(session) => {
                state.epoch += 1;
                info!(target: "rozu::cli", epoch = state.epoch,
                    reason = reason.as_ref().map(ToString::to_string).unwrap_or_else(|| "start".into()),
                    "grok session opened");
                state.mapper = Some(Mapper::new(session));
                Ok(())
            }
            Err(err) => {
                state.mapper = None;
                if !matches!(err, ProviderError::NotLoggedIn { .. }) {
                    state.rotate = reason;
                }
                if let Some(mut process) = state.process.take() {
                    process.kill();
                }
                Err(err)
            }
        }
    }

    async fn timed<T>(
        &self,
        work: impl Future<Output = ProviderResult<T>>,
        message: &str,
    ) -> ProviderResult<T> {
        tokio::time::timeout(self.config.handshake_timeout, work)
            .await
            .unwrap_or_else(|_| {
                Err(ProviderError::Api {
                    code: Some("timeout".into()),
                    message: message.into(),
                })
            })
    }

    /// Sends a request and waits for its response (`Err(message)` for a JSON-RPC error). Agent
    /// requests that arrive meanwhile are answered (default-deny); notifications are dropped.
    async fn call(
        &self,
        state: &mut State,
        method: &str,
        params: Value,
    ) -> ProviderResult<Result<Value, String>> {
        let id = state.next_id;
        state.next_id += 1;
        send(state, &protocol::request(id, method, params)).await?;
        loop {
            let message = next_message(state).await?;
            match Incoming::parse(message) {
                Some(Incoming::Response { id: got, result }) if got == json!(id) => {
                    return Ok(result);
                }
                Some(Incoming::Response { id, .. }) => {
                    debug!(target: "rozu::cli", %id, "late grok response");
                }
                Some(Incoming::Request { id, method, params }) => {
                    let answer = permissions::answer(
                        &id,
                        &method,
                        &params,
                        &self.config.exec,
                        &self.config.workspace,
                        state.mapper.as_ref(),
                        false,
                    );
                    send(state, &answer.reply).await?;
                }
                Some(Incoming::Notification { method, .. }) => {
                    debug!(target: "rozu::cli", %method, "grok notification outside a turn");
                }
                None => debug!(target: "rozu::cli", "ignored a non-JSON-RPC line from grok"),
            }
        }
    }
}

async fn send(state: &mut State, message: &Value) -> ProviderResult<()> {
    let process = state.process.as_mut().ok_or(ProviderError::UnexpectedEof)?;
    process
        .send_json(message)
        .await
        .map_err(|err| ProviderError::Api {
            code: Some("cli".into()),
            message: format!("could not write to grok: {err}"),
        })
}

/// The next message from Grok; its exit is a typed error.
async fn next_message(state: &mut State) -> ProviderResult<Value> {
    let process = state.process.as_mut().ok_or(ProviderError::UnexpectedEof)?;
    match process.next_json().await {
        Ok(Some(message)) => Ok(message),
        Ok(None) => Err(exited(state).await),
        Err(err) => Err(ProviderError::Decode(format!("grok output: {err}"))),
    }
}

/// Why Grok stopped (its stderr goes to the log file; here it's only classified).
async fn exited(state: &mut State) -> ProviderError {
    state.mapper = None;
    let Some(mut process) = state.process.take() else {
        return ProviderError::UnexpectedEof;
    };
    let status = process.exit_status(Duration::from_secs(2)).await;
    let stderr = process.stderr_tail().join("\n").to_lowercase();
    if protocol::looks_logged_out(&stderr) {
        return ProviderError::NotLoggedIn {
            cli: "grok".into(),
            hint: LOGIN_HINT.into(),
        };
    }
    let status = status.map_or("still running".to_owned(), |s| s.to_string());
    ProviderError::Api {
        code: Some("exit".into()),
        message: format!(
            "grok stopped ({status}) before finishing; its stderr is in rozu's log file"
        ),
    }
}

/// Holds the session lock for one turn. If the turn doesn't end cleanly (dropped, failed
/// mid-stream, cancel not acknowledged), Grok is killed; the next turn starts it again with a
/// new session and the conversation so far.
struct TurnGuard {
    state: OwnedMutexGuard<State>,
    finished: bool,
}

impl TurnGuard {
    fn keep_process(&mut self, keep: bool) {
        self.finished = keep;
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if !self.finished {
            if let Some(mut process) = self.state.process.take() {
                process.kill();
            }
            self.state.mapper = None;
        }
    }
}

enum Next {
    Cancelled,
    Record(StreamEvent),
    Line(Result<Option<Value>, ReadError>),
}

/// What handling one message meant for the turn.
enum Flow {
    Events(Vec<StreamEvent>),
    End {
        events: Vec<StreamEvent>,
        error: Option<ProviderError>,
        keep: bool,
    },
}

impl GrokSession {
    async fn handle(config: &GrokConfig, state: &mut State, message: Value, prompt: u64) -> Flow {
        match Incoming::parse(message) {
            Some(Incoming::Response { id, result }) if id == json!(prompt) => prompt_ended(result),
            Some(Incoming::Response { id, .. }) => {
                debug!(target: "rozu::cli", %id, "late grok response");
                Flow::Events(Vec::new())
            }
            Some(Incoming::Request { id, method, params }) => {
                let answer = permissions::answer(
                    &id,
                    &method,
                    &params,
                    &config.exec,
                    &config.workspace,
                    state.mapper.as_ref(),
                    false,
                );
                let events = answer
                    .records
                    .into_iter()
                    .map(StreamEvent::Approval)
                    .collect();
                match send(state, &answer.reply).await {
                    Ok(()) => Flow::Events(events),
                    Err(err) => Flow::End {
                        events,
                        error: Some(err),
                        keep: false,
                    },
                }
            }
            Some(Incoming::Notification { method, params }) if method == "session/update" => {
                Flow::Events(
                    state
                        .mapper
                        .as_mut()
                        .map(|m| m.update(&params))
                        .unwrap_or_default(),
                )
            }
            Some(Incoming::Notification { method, .. }) => {
                debug!(target: "rozu::cli", %method, "ignored grok notification");
                Flow::Events(Vec::new())
            }
            None => Flow::Events(Vec::new()),
        }
    }

    /// Sends `session/cancel` and waits (up to the grace period) for the prompt to end,
    /// answering permission requests meanwhile as cancelled. Returns whether it ended, i.e.
    /// the process and session can take the next turn.
    async fn cancel_turn(config: &GrokConfig, state: &mut State, prompt: u64) -> bool {
        let Some(session) = state.mapper.as_ref().map(|m| m.session_id().to_owned()) else {
            return false;
        };
        let cancel = protocol::notification("session/cancel", json!({ "sessionId": session }));
        if send(state, &cancel).await.is_err() {
            return false;
        }
        let drain = async {
            loop {
                let Ok(message) = next_message(state).await else {
                    return false;
                };
                match Incoming::parse(message) {
                    Some(Incoming::Response { id, .. }) if id == json!(prompt) => return true,
                    Some(Incoming::Request { id, method, params }) => {
                        let answer = permissions::answer(
                            &id,
                            &method,
                            &params,
                            &config.exec,
                            &config.workspace,
                            state.mapper.as_ref(),
                            true,
                        );
                        if send(state, &answer.reply).await.is_err() {
                            return false;
                        }
                    }
                    _ => {}
                }
            }
        };
        tokio::time::timeout(config.cancel_grace, drain)
            .await
            .unwrap_or(false)
    }
}

/// The `session/prompt` response: usage, then done, cancelled or a typed error.
fn prompt_ended(result: Result<Value, String>) -> Flow {
    match result {
        Ok(result) => {
            let mut events: Vec<StreamEvent> = protocol::usage_from_acp_meta(&result["_meta"])
                .map(StreamEvent::Usage)
                .into_iter()
                .collect();
            match protocol::stop_reason(result["stopReason"].as_str()) {
                Ok(stop) => {
                    events.push(StreamEvent::Done(stop));
                    Flow::End {
                        events,
                        error: None,
                        keep: true,
                    }
                }
                Err(err) => Flow::End {
                    events,
                    error: Some(err),
                    keep: true,
                },
            }
        }
        Err(message) => {
            let err = protocol::classify_error(&message);
            let keep = !matches!(err, ProviderError::NotLoggedIn { .. });
            Flow::End {
                events: Vec::new(),
                error: Some(err),
                keep,
            }
        }
    }
}

#[async_trait]
impl Provider for GrokSession {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::AgentBackend
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: tokio_util::sync::CancellationToken,
    ) -> ProviderResult<EventStream> {
        let mut state = self.state.clone().lock_owned().await;
        if state.process.is_none() || state.model.as_deref() != Some(req.model.as_str()) {
            self.start(&mut state, &req.model).await?;
        }
        let fresh = state.mapper.is_none() || state.rotate.is_some();
        if fresh {
            self.open_session(&mut state, &req).await?;
        }
        let text = turn_text_with(&req, !fresh, HISTORY_HEADER);
        // Decisions left over from an earlier, cancelled turn.
        while state.records.try_recv().is_ok() {}
        let session = state
            .mapper
            .as_ref()
            .expect("session opened above")
            .session_id()
            .to_owned();
        let prompt = state.next_id;
        state.next_id += 1;
        let request = protocol::request(
            prompt,
            "session/prompt",
            protocol::prompt_params(&session, &text),
        );
        if let Err(err) = send(&mut state, &request).await {
            if let Some(mut process) = state.process.take() {
                process.kill();
            }
            state.mapper = None;
            return Err(err);
        }
        debug!(target: "rozu::cli", %session, prompt, "grok turn started");

        let config = self.config.clone();
        let mut guard = TurnGuard {
            state,
            finished: false,
        };
        Ok(Box::pin(async_stream::stream! {
            loop {
                let state = &mut *guard.state;
                let Some(process) = state.process.as_mut() else {
                    yield Err(ProviderError::UnexpectedEof);
                    break;
                };
                let next = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => Next::Cancelled,
                    Some(record) = state.records.recv() => Next::Record(record),
                    line = process.next_json() => Next::Line(line),
                };
                let flow = match next {
                    Next::Cancelled => {
                        let ended = Self::cancel_turn(&config, state, prompt).await;
                        guard.keep_process(ended);
                        yield Err(ProviderError::Cancelled);
                        break;
                    }
                    Next::Record(event) => Flow::Events(vec![event]),
                    Next::Line(Ok(Some(message))) => {
                        Self::handle(&config, state, message, prompt).await
                    }
                    Next::Line(Ok(None)) => {
                        let err = exited(state).await;
                        yield Err(err);
                        break;
                    }
                    Next::Line(Err(err)) => {
                        yield Err(ProviderError::Decode(format!("grok output: {err}")));
                        break;
                    }
                };
                match flow {
                    Flow::Events(events) => {
                        for event in events {
                            yield Ok(event);
                        }
                    }
                    Flow::End { events, error, keep } => {
                        guard.keep_process(keep);
                        for event in events {
                            yield Ok(event);
                        }
                        if let Some(err) = error {
                            yield Err(err);
                        }
                        break;
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_allows_only_use_tool() {
        let profile = agent_profile(None);
        assert!(
            profile.starts_with("---\nname: rozu-backend\n"),
            "{profile}"
        );
        assert!(profile.contains("\ntools: use_tool\n"), "{profile}");
        assert!(!profile.contains("hooks"), "{profile}");
        let hooked = agent_profile(Some("'/bin/rozu' grok-hook --max-bytes 10"));
        let line = hooked.lines().find(|l| l.starts_with("hooks: ")).unwrap();
        let hooks: Value = serde_json::from_str(line.trim_start_matches("hooks: ")).unwrap();
        assert_eq!(hooks["PostToolUse"][0]["matcher"], hook::MATCHER);
    }

    #[test]
    fn tool_note_lists_rozu_tools_with_schemas() {
        let note = tool_note(&ToolRegistry::standard());
        for tool in [
            "read_file",
            "list_dir",
            "grep",
            "write_file",
            "apply_patch",
            "shell",
        ] {
            assert!(
                note.contains(&format!("- rozu__{tool}: ")),
                "{tool}: {note}"
            );
        }
        assert!(note.contains("Input schema: {"), "{note}");
        assert!(note.contains("without search_tool"), "{note}");
    }
}
