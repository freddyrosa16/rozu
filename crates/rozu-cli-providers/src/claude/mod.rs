//! Claude through the official `claude` CLI (Claude Code), on the user's own subscription.
//!
//! One long-lived `claude -p` process per provider, speaking stream-json on stdio (see
//! [`protocol`]). Claude runs the tool loop itself, so this is a
//! [`ProviderKind::AgentBackend`]: each [`Provider::stream`] call is one user turn. Claude's
//! mutating built-ins are disallowed; it reaches rozu's own tools, and asks permission for
//! anything else, through rozu's MCP server (`rozu mcp-serve`, see `rozu-mcp`), which applies
//! rozu's approval policy and forwards prompts to this process over an [`ApprovalBridge`](rozu_mcp::ApprovalBridge).
//!
//! The CLI does its own login and keeps its own credentials; rozu never reads them. Usage is
//! counted but has no cost (subscription billing). Vendor session persistence is disabled;
//! restarted processes receive the conversation history saved by rozu.

pub mod install;
pub mod protocol;

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use rozu_core::{
    ApprovalGate, ChatRequest, EventStream, Message, Provider, ProviderError, ProviderKind,
    ProviderResult, Role, StreamEvent,
};
use rozu_mcp::claude::{DISALLOWED_BUILTIN_TOOLS, MCP_SERVER};
use rozu_mcp::{APPROVAL_SOCKET_ENV, APPROVE_TOOL};
use serde_json::json;
use tokio::sync::{Mutex, OwnedMutexGuard, mpsc};
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::process::{CliCommand, CliProcess, DEFAULT_MAX_LINE_BYTES, ReadError};
use crate::runtime::ApprovalRuntime;
use crate::turn::turn_text;
use protocol::{Mapper, classify_error, looks_logged_out};

/// Must match the id in `rozu_providers::registry`, which lists this provider's models.
pub const PROVIDER_ID: &str = "claude-cli";

/// Appended to the system prompt so Claude uses rozu's tools.
const TOOL_NOTE: &str = "In this session rozu's tools are available as mcp__rozu__read_file, \
    mcp__rozu__list_dir, mcp__rozu__grep, mcp__rozu__write_file, mcp__rozu__apply_patch and \
    mcp__rozu__shell (paths relative to the workspace root). Use them for every file change and \
    command: Claude Code's own Bash, Edit, Write and web tools are disabled here, and rozu shows \
    the user an approval prompt where its policy requires one.";

/// Environment variables removed from the CLI's environment so it uses the subscription login
/// rather than an API key that happens to be set.
const ENV_REMOVE: &[&str] = &["ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN"];

pub struct ClaudeCliConfig {
    /// The `claude` binary (see [`install::find_claude_binary`]).
    pub binary: PathBuf,
    /// Working directory of the CLI: the workspace.
    pub workspace: PathBuf,
    /// Command line of rozu's MCP server, e.g. `/path/to/rozu mcp-serve --workspace … --approval auto`.
    pub mcp_command: PathBuf,
    pub mcp_args: Vec<String>,
    /// Answers the approval prompts the MCP server forwards (the user).
    pub gate: Arc<dyn ApprovalGate>,
    /// Extra environment for the CLI (tests).
    pub env: Vec<(String, String)>,
    pub max_line_bytes: usize,
    /// `--effort` (low, medium, high, xhigh, max); `None` keeps Claude's default.
    pub effort: Option<String>,
}

impl ClaudeCliConfig {
    pub fn new(
        binary: impl Into<PathBuf>,
        workspace: impl Into<PathBuf>,
        mcp_command: impl Into<PathBuf>,
        mcp_args: Vec<String>,
        gate: Arc<dyn ApprovalGate>,
    ) -> Self {
        Self {
            binary: binary.into(),
            workspace: workspace.into(),
            mcp_command: mcp_command.into(),
            mcp_args,
            gate,
            env: Vec::new(),
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            effort: None,
        }
    }
}

struct State {
    process: Option<CliProcess>,
    /// The model the running process was started with.
    model: Option<String>,
    /// The conversation known by the current process, including its last response.
    history: Vec<Message>,
    /// Decision-log entries from the MCP server and the prompts the bridge opened.
    records: mpsc::UnboundedReceiver<StreamEvent>,
}

pub struct ClaudeCliProvider {
    config: ClaudeCliConfig,
    /// Holds the MCP config file and the approval socket; removed on drop.
    runtime: ApprovalRuntime,
    mcp_config: PathBuf,
    state: Arc<Mutex<State>>,
}

impl ClaudeCliProvider {
    /// Writes the MCP config and starts the approval bridge. Needs a tokio runtime. The CLI
    /// itself starts with the first turn.
    pub fn new(config: ClaudeCliConfig) -> io::Result<Self> {
        let (runtime, records) =
            ApprovalRuntime::new("rozu-claude-", PROVIDER_ID, config.gate.clone())?;
        let mut env = serde_json::Map::new();
        if let Some(socket) = runtime.socket_path() {
            env.insert(
                APPROVAL_SOCKET_ENV.into(),
                socket.display().to_string().into(),
            );
        }
        let mcp = json!({
            "mcpServers": {
                MCP_SERVER: {
                    "type": "stdio",
                    "command": config.mcp_command,
                    "args": config.mcp_args,
                    "env": env,
                }
            }
        });
        let mcp_config = runtime.path().join("mcp.json");
        std::fs::write(&mcp_config, serde_json::to_vec_pretty(&mcp)?)?;
        Ok(Self {
            config,
            runtime,
            mcp_config,
            state: Arc::new(Mutex::new(State {
                process: None,
                model: None,
                history: Vec::new(),
                records,
            })),
        })
    }

    /// The directory holding the MCP config and approval socket.
    pub fn runtime_dir(&self) -> &std::path::Path {
        self.runtime.path()
    }

    /// The arguments `claude` is started with.
    pub fn args(&self, model: &str, system_prompt: &str) -> Vec<OsString> {
        let mut args: Vec<OsString> = [
            "-p",
            "--input-format",
            "stream-json",
            "--output-format",
            "stream-json",
            "--include-partial-messages",
            "--verbose",
            "--no-session-persistence",
            "--model",
            model,
            "--permission-mode",
            "default",
            "--permission-prompt-tool",
            &format!("mcp__{MCP_SERVER}__{APPROVE_TOOL}"),
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        args.push("--mcp-config".into());
        args.push(self.mcp_config.clone().into());
        args.push("--strict-mcp-config".into());
        args.push("--disallowedTools".into());
        args.push(DISALLOWED_BUILTIN_TOOLS.join(",").into());
        args.push("--append-system-prompt".into());
        args.push(system_prompt.into());
        if let Some(effort) = &self.config.effort {
            args.push("--effort".into());
            args.push(effort.into());
        }
        args
    }

    fn spawn(&self, state: &mut State, model: &str, system_prompt: &str) -> ProviderResult<()> {
        state.process = None;
        state.history.clear();
        let mut cmd = CliCommand::new("claude", &self.config.binary);
        cmd.args = self.args(model, system_prompt);
        cmd.cwd = Some(self.config.workspace.clone());
        cmd.env_remove = ENV_REMOVE.iter().map(OsString::from).collect();
        cmd.env = self
            .config
            .env
            .iter()
            .map(|(k, v)| (k.into(), v.into()))
            .collect();
        cmd.max_line_bytes = self.config.max_line_bytes;
        info!(target: "rozu::cli", model, "starting claude without session persistence");
        let process = CliProcess::spawn(&cmd).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound | io::ErrorKind::PermissionDenied => {
                ProviderError::CliNotFound {
                    cli: "claude".into(),
                    message: format!(
                        "{} can't be run ({err}); install Claude Code or set {}",
                        self.config.binary.display(),
                        install::CLAUDE_BIN_ENV
                    ),
                }
            }
            _ => ProviderError::CliNotFound {
                cli: "claude".into(),
                message: err.to_string(),
            },
        })?;
        state.process = Some(process);
        state.model = Some(model.to_owned());
        Ok(())
    }
}

/// The system prompt for `--append-system-prompt`: the session's system messages plus how to
/// reach rozu's tools.
fn system_prompt(req: &ChatRequest) -> String {
    crate::turn::system_prompt(req, TOOL_NOTE)
}

/// Holds the session lock for one turn. If the turn doesn't finish (cancelled, dropped, failed
/// mid-stream), the CLI is killed; the next turn replays rozu's history into a fresh process.
struct TurnGuard {
    state: OwnedMutexGuard<State>,
    history: Vec<Message>,
    response: String,
    finished: bool,
}

impl TurnGuard {
    /// The turn ended cleanly enough that the CLI can take the next one.
    fn keep_process(&mut self, keep: bool) {
        if keep {
            self.state.history = std::mem::take(&mut self.history);
            if !self.response.is_empty() {
                self.state
                    .history
                    .push(Message::assistant(std::mem::take(&mut self.response)));
            }
        }
        self.finished = keep;
    }
}

impl Drop for TurnGuard {
    fn drop(&mut self) {
        if !self.finished
            && let Some(mut process) = self.state.process.take()
        {
            process.kill();
        }
    }
}

enum Next {
    Cancelled,
    Record(StreamEvent),
    Line(Result<Option<serde_json::Value>, ReadError>),
}

#[async_trait]
impl Provider for ClaudeCliProvider {
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
        let has_history = state.process.is_some()
            && state.model.as_deref() == Some(req.model.as_str())
            && req.messages.split_last().is_some_and(|(last, previous)| {
                last.role == Role::User && previous == state.history.as_slice()
            });
        if !has_history {
            self.spawn(&mut state, &req.model, &system_prompt(&req))?;
        }
        let text = turn_text(&req, has_history);
        // Decisions left over from an earlier, interrupted turn.
        while state.records.try_recv().is_ok() {}
        let process = state.process.as_mut().expect("spawned above");
        if let Err(err) = process.send_json(&protocol::user_message(&text)).await {
            state.process = None;
            state.history.clear();
            return Err(ProviderError::Api {
                code: Some("cli".into()),
                message: format!("could not write to claude: {err}"),
            });
        }

        let mut guard = TurnGuard {
            state,
            history: req.messages,
            response: String::new(),
            finished: false,
        };
        Ok(Box::pin(async_stream::stream! {
            let mut mapper = Mapper::new();
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
                match next {
                    Next::Cancelled => {
                        yield Err(ProviderError::Cancelled);
                        break;
                    }
                    Next::Record(event) => yield Ok(event),
                    Next::Line(Ok(Some(line))) => match mapper.map(&line) {
                        Ok(step) => {
                            for event in step.events {
                                if let StreamEvent::TextDelta(text) = &event {
                                    guard.response.push_str(text);
                                }
                                if matches!(event, StreamEvent::Done(_)) {
                                    guard.keep_process(true);
                                }
                                yield Ok(event);
                            }
                            if step.done {
                                break;
                            }
                        }
                        Err(err) => {
                            // The turn is over; the process stays usable unless logged out.
                            guard.keep_process(!matches!(err, ProviderError::NotLoggedIn { .. }));
                            yield Err(err);
                            break;
                        }
                    },
                    Next::Line(Ok(None)) => {
                        let status = process.exit_status(Duration::from_secs(2)).await;
                        let stderr = process.stderr_tail().await.join("\n").to_lowercase();
                        state.process = None;
                        let err = if looks_logged_out(&stderr) {
                            classify_error("", "not logged in")
                        } else {
                            let status = status.map_or("still running".to_owned(), |s| s.to_string());
                            ProviderError::Api {
                                code: Some("exit".into()),
                                message: format!(
                                    "claude stopped ({status}) before finishing the turn; its \
                                     stderr is in rozu's log file"
                                ),
                            }
                        };
                        yield Err(err);
                        break;
                    }
                    Next::Line(Err(err)) => {
                        yield Err(ProviderError::Decode(format!("claude output: {err}")));
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

    fn request(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "sonnet".into(),
            messages,
            tools: Vec::new(),
            max_output_tokens: None,
        }
    }

    #[test]
    fn system_prompt_includes_the_tool_note() {
        let prompt = system_prompt(&request(vec![Message::system("You are rozu.")]));
        assert!(prompt.starts_with("You are rozu.\n\n"));
        assert!(prompt.contains("mcp__rozu__shell"));
    }
}
