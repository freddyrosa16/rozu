//! OpenAI through the official Codex CLI's `codex app-server` (JSON-RPC over stdio), on the
//! user's ChatGPT subscription ("Sign in with ChatGPT" is done by `codex login`, never by rozu).
//!
//! One long-lived app-server per provider, one Codex thread per session; each
//! [`Provider::stream`] call is one turn (`turn/start`), so this is a
//! [`ProviderKind::AgentBackend`]. Codex runs its own tool loop:
//!
//! - **rozu's tools** reach it as the `rozu` MCP server (`rozu mcp-serve`), configured with
//!   `-c mcp_servers.rozu.*`; they apply rozu's policy and sandbox themselves and ask the user
//!   through the [`ApprovalBridge`]. So Codex is told not to ask before them
//!   (`default_tools_approval_mode="approve"`, for that server only), and the approval
//!   elicitations it may still send for them are accepted.
//! - **Codex's own shell and apply_patch** stay enabled (Codex 0.147's config schema has no
//!   switch to turn them off), under `approval_policy="untrusted"` and its `workspace-write`
//!   (or `read-only`) sandbox with the network off. Every approval request Codex sends is
//!   answered by rozu's [`ApprovalPolicy`](rozu_core::ApprovalPolicy); see [`approvals`].
//!
//! Cancel sends `turn/interrupt`; the process survives unless it doesn't stop in time. Login is
//! checked with `account/read` (only the account type is read). Usage is counted, never priced.

pub mod approvals;
pub mod install;
pub mod protocol;

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rozu_core::{
    ApprovalGate, ApprovalMode, ApprovalRecord, ApprovalRequest, ChatRequest, Decision,
    EventStream, Provider, ProviderError, ProviderKind, ProviderResult, StreamEvent, ToolExecutor,
};
use rozu_mcp::{APPROVAL_SOCKET_ENV, ApprovalBridge, SERVER_NAME};
use rozu_tools::Workspace;
use serde_json::{Value, json};
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::process::{CliCommand, CliProcess, DEFAULT_MAX_LINE_BYTES, ReadError};
use crate::turn::{system_prompt, turn_text};
use approvals::ApprovalCtx;
use protocol::{AccountStatus, Incoming, LOGIN_HINT, Mapper};

/// Must match the id in `rozu_providers::registry`, which lists this provider's models.
pub const PROVIDER_ID: &str = "codex";
/// The model id meaning "whatever Codex is configured to use": no `model` is sent.
pub const DEFAULT_MODEL: &str = "default";
/// Codex approval policy rozu runs it with: everything but known-safe reads asks rozu.
/// (`on-request` would let Codex run sandboxed commands without asking.)
pub const APPROVAL_POLICY: &str = "untrusted";
/// Codex's approval mode for the `rozu` MCP server's tools (`mcp_servers.rozu.
/// default_tools_approval_mode`): run without Codex asking, since rozu gates each call itself.
pub const ROZU_TOOLS_APPROVAL_MODE: &str = "approve";

/// Removed from Codex's environment so it uses its own (ChatGPT) login, not an API key.
pub(crate) const ENV_REMOVE: &[&str] = &["OPENAI_API_KEY", "CODEX_API_KEY"];

/// Added to the developer instructions so Codex knows about rozu's tools.
const TOOL_NOTE: &str = "In this session rozu's tools are available from the `rozu` MCP \
    server: read_file, list_dir, grep, write_file, apply_patch and shell (paths relative to the \
    workspace root). Prefer them for file changes and commands: they run under rozu's sandbox \
    and policy. Your own shell commands and patches also work, but rozu asks the user before \
    any of them that its policy doesn't allow.";

pub struct CodexConfig {
    /// The `codex` binary (see [`install::find_codex_binary`]).
    pub binary: PathBuf,
    /// The workspace: Codex's working directory and the root rozu checks paths against.
    pub workspace: Workspace,
    /// Command line of rozu's MCP server, e.g. `/path/to/rozu mcp-serve --workspace … --approval auto`.
    pub mcp_command: PathBuf,
    pub mcp_args: Vec<String>,
    /// Decides Codex's approval requests: the project's policy, the user's gate and the audit
    /// log (tag it with `provider: codex`).
    pub exec: ToolExecutor,
    /// Extra environment for the CLI (tests).
    pub env: Vec<(String, String)>,
    pub max_line_bytes: usize,
    /// How long starting the app-server (initialize, login check, thread) may take.
    pub handshake_timeout: Duration,
    /// How long to wait for an interrupted turn to end before killing the app-server.
    pub interrupt_grace: Duration,
}

impl CodexConfig {
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
            interrupt_grace: Duration::from_secs(10),
        }
    }
}

struct State {
    process: Option<CliProcess>,
    next_id: u64,
    /// The thread (and its token totals); `None` until `thread/start` succeeded.
    mapper: Option<Mapper>,
    /// Notifications read while waiting for a response, not yet handled.
    pending: VecDeque<(String, Value)>,
    /// Decision-log entries from `rozu mcp-serve`.
    records: mpsc::UnboundedReceiver<ApprovalRecord>,
}

pub struct CodexAppServerProvider {
    config: Arc<CodexConfig>,
    /// Holds the approval socket; removed on drop.
    runtime_dir: tempfile::TempDir,
    bridge: Option<ApprovalBridge>,
    state: Arc<Mutex<State>>,
}

impl CodexAppServerProvider {
    /// Starts the approval bridge for `rozu mcp-serve`. Needs a tokio runtime. The app-server
    /// itself starts with the first turn.
    pub fn new(mut config: CodexConfig) -> io::Result<Self> {
        let user_gate = config.exec.gate.clone();
        config.exec.gate = Arc::new(OneTimeGate(user_gate.clone()));
        // Short base path: Unix socket paths are limited to ~100 bytes.
        let base = if cfg!(unix) {
            PathBuf::from("/tmp")
        } else {
            std::env::temp_dir()
        };
        let runtime_dir = tempfile::Builder::new()
            .prefix("rozu-codex-")
            .tempdir_in(base)?;
        let (records_tx, records) = mpsc::unbounded_channel();
        let socket = runtime_dir.path().join("approval.sock");
        // rozu's own tools (via `rozu mcp-serve`) get the user's real answers, `u` included.
        let bridge = match ApprovalBridge::bind(&socket, user_gate, records_tx) {
            Ok(bridge) => Some(bridge),
            Err(err) => {
                warn!(error = %err, "no approval bridge; prompts from rozu's MCP tools will be denied");
                None
            }
        };
        Ok(Self {
            config: Arc::new(config),
            runtime_dir,
            bridge,
            state: Arc::new(Mutex::new(State {
                process: None,
                next_id: 1,
                mapper: None,
                pending: VecDeque::new(),
                records,
            })),
        })
    }

    /// The directory holding the approval socket.
    pub fn runtime_dir(&self) -> &std::path::Path {
        self.runtime_dir.path()
    }

    /// Codex's sandbox for rozu's approval mode: read-only stays read-only, otherwise
    /// workspace-write (rozu's full-access mode doesn't lift Codex's sandbox either).
    pub fn sandbox_mode(&self) -> &'static str {
        if self.config.exec.policy.mode() == ApprovalMode::ReadOnly {
            "read-only"
        } else {
            "workspace-write"
        }
    }

    /// The `-c key=value` config overrides (TOML values) Codex is started with.
    pub fn config_overrides(&self) -> Vec<String> {
        let server = SERVER_NAME;
        let mut overrides = vec![
            format!("approval_policy={}", toml_string(APPROVAL_POLICY)),
            format!("sandbox_mode={}", toml_string(self.sandbox_mode())),
            "sandbox_workspace_write.network_access=false".to_owned(),
            format!("web_search={}", toml_string("disabled")),
            format!(
                "mcp_servers.{server}.command={}",
                toml_string(&self.config.mcp_command.display().to_string())
            ),
            format!(
                "mcp_servers.{server}.args=[{}]",
                self.config
                    .mcp_args
                    .iter()
                    .map(|a| toml_string(a))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            // Codex needn't ask before rozu's own tools: `rozu mcp-serve` applies the policy,
            // sandbox and audit log to every call. Set for this server only; prompts Codex
            // still sends for it are accepted (see `approvals`), everything else is declined.
            format!(
                "mcp_servers.{server}.default_tools_approval_mode={}",
                toml_string(ROZU_TOOLS_APPROVAL_MODE)
            ),
        ];
        if let Some(bridge) = &self.bridge {
            overrides.push(format!(
                "mcp_servers.{server}.env={{ {APPROVAL_SOCKET_ENV} = {} }}",
                toml_string(&bridge.path().display().to_string())
            ));
        }
        overrides
    }

    /// The arguments `codex` is started with: `-c` overrides, then `app-server`.
    pub fn args(&self) -> Vec<OsString> {
        let mut args = Vec::new();
        for value in self.config_overrides() {
            args.push(OsString::from("-c"));
            args.push(value.into());
        }
        args.push("app-server".into());
        args
    }

    /// Starts the app-server and a thread: `initialize`, `initialized`, `account/read`,
    /// `thread/start`.
    async fn start(
        &self,
        state: &mut State,
        model: &str,
        instructions: &str,
    ) -> ProviderResult<()> {
        if let Some(mut old) = state.process.take() {
            old.kill();
        }
        state.mapper = None;
        state.pending.clear();
        let mut cmd = CliCommand::new("codex", &self.config.binary);
        cmd.args = self.args();
        cmd.cwd = Some(self.config.workspace.root().to_path_buf());
        cmd.env_remove = ENV_REMOVE.iter().map(OsString::from).collect();
        cmd.env = self
            .config
            .env
            .iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        cmd.max_line_bytes = self.config.max_line_bytes;
        info!(target: "rozu::cli", model, sandbox = self.sandbox_mode(), "starting codex app-server");
        let process = CliProcess::spawn(&cmd).map_err(|err| ProviderError::CliNotFound {
            cli: "codex".into(),
            message: format!(
                "{} can't be run ({err}); install the Codex CLI or set {}",
                self.config.binary.display(),
                install::CODEX_BIN_ENV
            ),
        })?;
        state.process = Some(process);

        let handshake = async {
            let init = self
                .call(state, "initialize", protocol::initialize_params())
                .await?;
            info!(target: "rozu::cli", user_agent = init["userAgent"].as_str().unwrap_or(""),
                "codex app-server initialized");
            send(state, &protocol::notification("initialized")).await?;
            let account = self
                .call(state, "account/read", json!({ "refreshToken": false }))
                .await?;
            match protocol::account_status(&account) {
                AccountStatus::LoggedIn(mode) => {
                    info!(target: "rozu::cli", auth_mode = %mode, "codex is logged in");
                }
                AccountStatus::NotRequired => {
                    info!(target: "rozu::cli", "codex needs no OpenAI login (custom provider)");
                }
                AccountStatus::NotLoggedIn => {
                    return Err(ProviderError::NotLoggedIn {
                        cli: "codex".into(),
                        hint: LOGIN_HINT.into(),
                    });
                }
            }
            let mut params = json!({
                "cwd": self.config.workspace.root().display().to_string(),
                "approvalPolicy": APPROVAL_POLICY,
                "sandbox": self.sandbox_mode(),
                "developerInstructions": instructions,
            });
            if model != DEFAULT_MODEL {
                params["model"] = model.into();
            }
            let started = self.call(state, "thread/start", params).await?;
            let thread = started["thread"]["id"].as_str().ok_or_else(|| {
                ProviderError::Decode("thread/start returned no thread id".into())
            })?;
            info!(target: "rozu::cli", thread, model = started["model"].as_str().unwrap_or(""),
                "codex thread started");
            state.mapper = Some(Mapper::new(thread));
            Ok(())
        };
        let result = match tokio::time::timeout(self.config.handshake_timeout, handshake).await {
            Ok(result) => result,
            Err(_) => Err(ProviderError::Api {
                code: Some("timeout".into()),
                message: "codex app-server didn't start in time".into(),
            }),
        };
        if result.is_err() {
            if let Some(mut process) = state.process.take() {
                process.kill();
            }
            state.mapper = None;
        }
        result
    }

    /// Sends a request and waits for its response. Server requests that arrive meanwhile are
    /// answered (by policy and the user); notifications are kept for the turn.
    async fn call(&self, state: &mut State, method: &str, params: Value) -> ProviderResult<Value> {
        let id = state.next_id;
        state.next_id += 1;
        send(state, &protocol::request(id, method, params)).await?;
        loop {
            let message = next_message(state).await?;
            match Incoming::parse(message) {
                Some(Incoming::Response { id: got, result }) if got == json!(id) => {
                    return result.map_err(|message| ProviderError::Api {
                        code: Some(method.to_owned()),
                        message,
                    });
                }
                Some(Incoming::Response { id, .. }) => {
                    debug!(target: "rozu::cli", %id, "late codex response");
                }
                Some(Incoming::Request {
                    id: req,
                    method,
                    params,
                }) => {
                    let cancel = CancellationToken::new();
                    let answer = approvals::answer(
                        &req,
                        &method,
                        &params,
                        &ApprovalCtx {
                            exec: &self.config.exec,
                            workspace: &self.config.workspace,
                            cancel: &cancel,
                            mapper: state.mapper.as_ref(),
                        },
                    )
                    .await;
                    send(state, &answer.reply).await?;
                }
                Some(Incoming::Notification { method, params }) => {
                    state.pending.push_back((method, params));
                }
                None => debug!(target: "rozu::cli", "ignored a non-JSON-RPC line from codex"),
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
            message: format!("could not write to codex: {err}"),
        })
}

/// The next message from the app-server; its exit is a typed error.
async fn next_message(state: &mut State) -> ProviderResult<Value> {
    let process = state.process.as_mut().ok_or(ProviderError::UnexpectedEof)?;
    match process.next_json().await {
        Ok(Some(message)) => Ok(message),
        Ok(None) => Err(exited(state).await),
        Err(err) => Err(ProviderError::Decode(format!("codex output: {err}"))),
    }
}

/// Why the app-server stopped (its stderr goes to the log file; here it's only classified).
async fn exited(state: &mut State) -> ProviderError {
    let Some(mut process) = state.process.take() else {
        return ProviderError::UnexpectedEof;
    };
    state.mapper = None;
    let status = process.exit_status(Duration::from_secs(2)).await;
    let stderr = process.stderr_tail().join("\n").to_lowercase();
    if protocol::looks_logged_out(&stderr) {
        return ProviderError::NotLoggedIn {
            cli: "codex".into(),
            hint: LOGIN_HINT.into(),
        };
    }
    let status = status.map_or("still running".to_owned(), |s| s.to_string());
    ProviderError::Api {
        code: Some("exit".into()),
        message: format!(
            "codex app-server stopped ({status}) before finishing; its stderr is in rozu's log file"
        ),
    }
}

/// The user's gate for Codex's own requests: rozu can't lift Codex's sandbox, so the
/// "unsandboxed" answer (`u`) is the one-time approval it really is, and is logged as such.
struct OneTimeGate(Arc<dyn ApprovalGate>);

#[async_trait]
impl ApprovalGate for OneTimeGate {
    async fn decide(&self, request: ApprovalRequest) -> Decision {
        match self.0.decide(request).await {
            Decision::ApproveUnsandboxed => Decision::Approve,
            other => other,
        }
    }
}

/// A TOML basic string (for `-c` values): JSON's escapes are valid TOML ones; DEL, which TOML
/// doesn't allow raw, is escaped too.
pub fn toml_string(value: &str) -> String {
    serde_json::to_string(value)
        .unwrap_or_else(|_| "\"\"".to_owned())
        .replace('\u{7f}', "\\u007f")
}

/// Holds the session lock for one turn. If the turn doesn't finish cleanly (dropped, failed
/// mid-stream, interrupt not acknowledged), the app-server is killed; the next turn starts a
/// fresh one with the conversation so far as a transcript.
struct TurnGuard {
    state: OwnedMutexGuard<State>,
    finished: bool,
}

impl TurnGuard {
    /// The turn ended cleanly enough that the app-server can take the next one.
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
            self.state.pending.clear();
        }
    }
}

enum Next {
    Cancelled,
    Record(ApprovalRecord),
    Line(Result<Option<Value>, ReadError>),
    Message(ProviderResult<Value>),
    Pending(String, Value),
}

/// What handling one message meant for the stream.
enum Flow {
    Events(Vec<StreamEvent>),
    /// The turn ended; keep the process for the next one?
    End {
        events: Vec<StreamEvent>,
        error: Option<ProviderError>,
        keep: bool,
    },
}

impl CodexAppServerProvider {
    async fn handle(
        config: &CodexConfig,
        state: &mut State,
        message: Value,
        cancel: &CancellationToken,
    ) -> Flow {
        match Incoming::parse(message) {
            Some(Incoming::Notification { method, params }) => {
                Self::notification(state, &method, &params)
            }
            Some(Incoming::Request { id, method, params }) => {
                let answer = approvals::answer(
                    &id,
                    &method,
                    &params,
                    &ApprovalCtx {
                        exec: &config.exec,
                        workspace: &config.workspace,
                        cancel,
                        mapper: state.mapper.as_ref(),
                    },
                )
                .await;
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
            Some(Incoming::Response { id, .. }) => {
                debug!(target: "rozu::cli", %id, "late codex response");
                Flow::Events(Vec::new())
            }
            None => Flow::Events(Vec::new()),
        }
    }

    fn notification(state: &mut State, method: &str, params: &Value) -> Flow {
        let Some(mapper) = state.mapper.as_mut() else {
            return Flow::Events(Vec::new());
        };
        let step = mapper.map(method, params);
        if step.done || step.error.is_some() {
            let keep = !matches!(step.error, Some(ProviderError::NotLoggedIn { .. }));
            Flow::End {
                events: step.events,
                error: step.error,
                keep,
            }
        } else {
            Flow::Events(step.events)
        }
    }

    /// Interrupts the running turn and waits (up to the grace period) for it to end, answering
    /// any approval request meanwhile as cancelled. Returns whether the turn ended, i.e. the
    /// app-server can take the next turn.
    async fn interrupt(config: &CodexConfig, state: &mut State, turn_id: &str) -> bool {
        let Some(thread) = state.mapper.as_ref().map(|m| m.thread_id().to_owned()) else {
            return false;
        };
        let id = state.next_id;
        state.next_id += 1;
        let request = protocol::request(
            id,
            "turn/interrupt",
            json!({ "threadId": thread, "turnId": turn_id }),
        );
        if send(state, &request).await.is_err() {
            return false;
        }
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let drain = async {
            loop {
                let message = match state.pending.pop_front() {
                    Some((method, params)) => json!({ "method": method, "params": params }),
                    None => match next_message(state).await {
                        Ok(message) => message,
                        Err(_) => return false,
                    },
                };
                if let Flow::End { keep, .. } =
                    Self::handle(config, state, message, &cancelled).await
                {
                    return keep;
                }
            }
        };
        tokio::time::timeout(config.interrupt_grace, drain)
            .await
            .unwrap_or(false)
    }
}

#[async_trait]
impl Provider for CodexAppServerProvider {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::AgentBackend
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> ProviderResult<EventStream> {
        let mut state = self.state.clone().lock_owned().await;
        let has_thread = state.process.is_some() && state.mapper.is_some();
        if !has_thread {
            self.start(&mut state, &req.model, &system_prompt(&req, TOOL_NOTE))
                .await?;
        }
        let text = turn_text(&req, has_thread);
        // Decisions left over from an earlier, interrupted turn.
        while state.records.try_recv().is_ok() {}
        let thread = {
            let mapper = state.mapper.as_mut().expect("thread started above");
            mapper.start_turn();
            mapper.thread_id().to_owned()
        };
        let mut params = json!({ "threadId": thread, "input": protocol::text_input(&text) });
        if req.model != DEFAULT_MODEL {
            params["model"] = req.model.clone().into();
        }
        let started = match self.call(&mut state, "turn/start", params).await {
            Ok(started) => started,
            Err(err) => {
                if let Some(mut process) = state.process.take() {
                    process.kill();
                }
                state.mapper = None;
                return Err(err);
            }
        };
        let turn_id = started["turn"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        debug!(target: "rozu::cli", turn = %turn_id, "codex turn started");

        let config = self.config.clone();
        let mut guard = TurnGuard {
            state,
            finished: false,
        };
        Ok(Box::pin(async_stream::stream! {
            loop {
                let state = &mut *guard.state;
                let next = match state.pending.pop_front() {
                    Some((method, params)) => Next::Pending(method, params),
                    None => {
                        let Some(process) = state.process.as_mut() else {
                            yield Err(ProviderError::UnexpectedEof);
                            break;
                        };
                        tokio::select! {
                            biased;
                            _ = cancel.cancelled() => Next::Cancelled,
                            Some(record) = state.records.recv() => Next::Record(record),
                            line = process.next_json() => Next::Line(line),
                        }
                    }
                };
                let next = match next {
                    Next::Line(Ok(Some(message))) => Next::Message(Ok(message)),
                    Next::Line(Ok(None)) => Next::Message(Err(exited(state).await)),
                    Next::Line(Err(err)) => {
                        Next::Message(Err(ProviderError::Decode(format!("codex output: {err}"))))
                    }
                    other => other,
                };
                let flow = match next {
                    Next::Line(_) => unreachable!("lines are converted above"),
                    Next::Cancelled => {
                        let ended = Self::interrupt(&config, state, &turn_id).await;
                        guard.keep_process(ended);
                        yield Err(ProviderError::Cancelled);
                        break;
                    }
                    Next::Record(record) => Flow::Events(vec![StreamEvent::Approval(record)]),
                    Next::Pending(method, params) => Self::notification(state, &method, &params),
                    Next::Message(Ok(message)) => {
                        Self::handle(&config, state, message, &cancel).await
                    }
                    Next::Message(Err(err)) => {
                        yield Err(err);
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
    fn toml_strings_are_quoted_and_escaped() {
        assert_eq!(toml_string("untrusted"), "\"untrusted\"");
        assert_eq!(toml_string("/a b/\"x\"\\"), r#""/a b/\"x\"\\""#);
        assert_eq!(toml_string("a\u{7f}"), "\"a\\u007f\"");
    }
}
