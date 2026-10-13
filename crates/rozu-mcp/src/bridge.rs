//! The approval channel between `rozu mcp-serve` (started by the vendor CLI) and the parent
//! `rozu` process that owns the terminal.
//!
//! The parent binds an [`ApprovalBridge`] on a Unix socket in a private (0700) temp directory
//! and passes its path to the MCP server in [`APPROVAL_SOCKET_ENV`]. The server's
//! [`SocketGate`] opens one connection per message and writes one JSON line:
//!
//! - `{"type":"ask","request":{…}}`: the parent asks its [`ApprovalGate`] (the user) and answers
//!   with one line, `{"decision":{"kind":"approve"|"always_allow_prefix"|"approve_unsandboxed"|
//!   "deny","reason":…}}`.
//!   If the server hangs up first (the CLI was cancelled), the question is withdrawn.
//! - `{"type":"record","record":{…}}`: a decision-log entry for the UI; no answer.
//!
//! Default-deny: when the socket is missing, unreachable or answers garbage, the gate denies.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use futures::StreamExt;
use rozu_core::{
    ApprovalGate, ApprovalMode, ApprovalRecord, ApprovalRequest, Decision, DecisionSource,
    SandboxMode,
};
use rozu_tools::{AllowRule, Risk};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};
use tracing::{debug, warn};

/// Environment variable carrying the bridge's socket path to `rozu mcp-serve`.
pub const APPROVAL_SOCKET_ENV: &str = "ROZU_APPROVAL_SOCKET";
/// Longest message accepted on the socket.
const MAX_MESSAGE_BYTES: usize = 4 << 20;

// ---------------------------------------------------------------------------------------------
// Wire format

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Message {
    Ask { request: WireRequest },
    Record { record: WireRecord },
}

#[derive(Debug, Serialize, Deserialize)]
struct Reply {
    decision: WireDecision,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireRequest {
    call_id: String,
    tool: String,
    args: Value,
    risk: WireRisk,
    preview: String,
    sandbox: WireSandbox,
    network: bool,
    #[serde(default)]
    extra_writes: Vec<PathBuf>,
    #[serde(default)]
    outside_unknown: bool,
    #[serde(default)]
    blocked_retry: bool,
    allow_rule: Option<AllowRule>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WireRecord {
    call_id: String,
    tool: String,
    risk: WireRisk,
    preview: String,
    sandbox: WireSandbox,
    network: bool,
    #[serde(default)]
    extra_writes: Vec<PathBuf>,
    #[serde(default)]
    blocked_retry: bool,
    decision: WireDecision,
    source: WireSource,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireRisk {
    Read,
    WriteWorkspace,
    WriteOutside,
    Network,
    Opaque,
    Destructive,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireSandbox {
    Enforced { name: String },
    Off,
    InProcess,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireDecision {
    Approve,
    AlwaysAllowPrefix,
    ApproveUnsandboxed,
    Deny { reason: String },
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum WireMode {
    ReadOnly,
    AutoWorkspace,
    FullAccess,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireSource {
    Mode { mode: WireMode },
    Rule { prefix: String },
    Gate,
    Cancelled,
    Policy,
}

impl From<Risk> for WireRisk {
    fn from(risk: Risk) -> Self {
        match risk {
            Risk::Read => Self::Read,
            Risk::WriteWorkspace => Self::WriteWorkspace,
            Risk::WriteOutside => Self::WriteOutside,
            Risk::Network => Self::Network,
            Risk::Opaque => Self::Opaque,
            Risk::Destructive => Self::Destructive,
        }
    }
}

impl From<WireRisk> for Risk {
    fn from(risk: WireRisk) -> Self {
        match risk {
            WireRisk::Read => Self::Read,
            WireRisk::WriteWorkspace => Self::WriteWorkspace,
            WireRisk::WriteOutside => Self::WriteOutside,
            WireRisk::Network => Self::Network,
            WireRisk::Opaque => Self::Opaque,
            WireRisk::Destructive => Self::Destructive,
        }
    }
}

impl From<SandboxMode> for WireSandbox {
    fn from(mode: SandboxMode) -> Self {
        match mode {
            SandboxMode::Enforced(name) => Self::Enforced { name: name.into() },
            SandboxMode::Off => Self::Off,
            SandboxMode::InProcess => Self::InProcess,
        }
    }
}

impl From<WireSandbox> for SandboxMode {
    fn from(mode: WireSandbox) -> Self {
        match mode {
            // `SandboxMode` holds a static name; only rozu's own sandboxes cross the wire.
            WireSandbox::Enforced { name } => Self::Enforced(if name == "seatbelt" {
                "seatbelt"
            } else {
                "sandbox"
            }),
            WireSandbox::Off => Self::Off,
            WireSandbox::InProcess => Self::InProcess,
        }
    }
}

impl From<Decision> for WireDecision {
    fn from(decision: Decision) -> Self {
        match decision {
            Decision::Approve => Self::Approve,
            Decision::AlwaysAllowPrefix => Self::AlwaysAllowPrefix,
            Decision::ApproveUnsandboxed => Self::ApproveUnsandboxed,
            Decision::Deny { reason } => Self::Deny { reason },
        }
    }
}

impl From<WireDecision> for Decision {
    fn from(decision: WireDecision) -> Self {
        match decision {
            WireDecision::Approve => Self::Approve,
            WireDecision::AlwaysAllowPrefix => Self::AlwaysAllowPrefix,
            WireDecision::ApproveUnsandboxed => Self::ApproveUnsandboxed,
            WireDecision::Deny { reason } => Self::Deny { reason },
        }
    }
}

impl From<ApprovalMode> for WireMode {
    fn from(mode: ApprovalMode) -> Self {
        match mode {
            ApprovalMode::ReadOnly => Self::ReadOnly,
            ApprovalMode::AutoWorkspace => Self::AutoWorkspace,
            ApprovalMode::FullAccess => Self::FullAccess,
        }
    }
}

impl From<WireMode> for ApprovalMode {
    fn from(mode: WireMode) -> Self {
        match mode {
            WireMode::ReadOnly => Self::ReadOnly,
            WireMode::AutoWorkspace => Self::AutoWorkspace,
            WireMode::FullAccess => Self::FullAccess,
        }
    }
}

impl From<DecisionSource> for WireSource {
    fn from(source: DecisionSource) -> Self {
        match source {
            DecisionSource::Mode(mode) => Self::Mode { mode: mode.into() },
            DecisionSource::Rule { prefix } => Self::Rule { prefix },
            DecisionSource::Gate => Self::Gate,
            DecisionSource::Cancelled => Self::Cancelled,
            DecisionSource::Policy => Self::Policy,
        }
    }
}

impl From<WireSource> for DecisionSource {
    fn from(source: WireSource) -> Self {
        match source {
            WireSource::Mode { mode } => Self::Mode(mode.into()),
            WireSource::Rule { prefix } => Self::Rule { prefix },
            WireSource::Gate => Self::Gate,
            WireSource::Cancelled => Self::Cancelled,
            WireSource::Policy => Self::Policy,
        }
    }
}

impl From<ApprovalRequest> for WireRequest {
    fn from(req: ApprovalRequest) -> Self {
        Self {
            call_id: req.call_id,
            tool: req.tool,
            args: req.args,
            risk: req.risk.into(),
            preview: req.preview,
            sandbox: req.sandbox.into(),
            network: req.network,
            extra_writes: req.extra_writes,
            outside_unknown: req.outside_unknown,
            blocked_retry: req.blocked_retry,
            allow_rule: req.allow_rule,
        }
    }
}

impl From<WireRequest> for ApprovalRequest {
    fn from(req: WireRequest) -> Self {
        Self {
            call_id: req.call_id,
            tool: req.tool,
            args: req.args,
            risk: req.risk.into(),
            preview: req.preview,
            sandbox: req.sandbox.into(),
            network: req.network,
            extra_writes: req.extra_writes,
            outside_unknown: req.outside_unknown,
            blocked_retry: req.blocked_retry,
            allow_rule: req.allow_rule,
        }
    }
}

impl From<ApprovalRecord> for WireRecord {
    fn from(record: ApprovalRecord) -> Self {
        Self {
            call_id: record.call_id,
            tool: record.tool,
            risk: record.risk.into(),
            preview: record.preview,
            sandbox: record.sandbox.into(),
            network: record.network,
            extra_writes: record.extra_writes,
            blocked_retry: record.blocked_retry,
            decision: record.decision.into(),
            source: record.source.into(),
        }
    }
}

impl From<WireRecord> for ApprovalRecord {
    fn from(record: WireRecord) -> Self {
        Self {
            call_id: record.call_id,
            tool: record.tool,
            risk: record.risk.into(),
            preview: record.preview,
            sandbox: record.sandbox.into(),
            network: record.network,
            extra_writes: record.extra_writes,
            blocked_retry: record.blocked_retry,
            decision: record.decision.into(),
            source: record.source.into(),
        }
    }
}

fn to_line<T: Serialize>(value: &T) -> io::Result<Vec<u8>> {
    let mut line = serde_json::to_vec(value).map_err(io::Error::other)?;
    line.push(b'\n');
    Ok(line)
}

// ---------------------------------------------------------------------------------------------
// Parent side

/// The parent's end: answers `ask`s with `gate` and forwards `record`s to `records`. Stops
/// listening and removes the socket file when dropped.
pub struct ApprovalBridge {
    path: PathBuf,
    task: tokio::task::JoinHandle<()>,
}

impl ApprovalBridge {
    /// Binds `path` (which must not exist; put it in a private directory) and starts serving.
    /// Must be called inside a tokio runtime.
    #[cfg(unix)]
    pub fn bind(
        path: impl Into<PathBuf>,
        gate: Arc<dyn ApprovalGate>,
        records: mpsc::UnboundedSender<ApprovalRecord>,
    ) -> io::Result<Self> {
        let path = path.into();
        let listener = tokio::net::UnixListener::bind(&path)?;
        let task = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, _)) => {
                        tokio::spawn(serve_connection(stream, gate.clone(), records.clone()));
                    }
                    Err(err) => {
                        warn!(error = %err, "approval socket accept failed");
                        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                    }
                }
            }
        });
        Ok(Self { path, task })
    }

    #[cfg(not(unix))]
    pub fn bind(
        _path: impl Into<PathBuf>,
        _gate: Arc<dyn ApprovalGate>,
        _records: mpsc::UnboundedSender<ApprovalRecord>,
    ) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the approval bridge needs Unix sockets",
        ))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for ApprovalBridge {
    fn drop(&mut self) {
        self.task.abort();
        let _ = std::fs::remove_file(&self.path);
    }
}

#[cfg(unix)]
async fn serve_connection(
    stream: tokio::net::UnixStream,
    gate: Arc<dyn ApprovalGate>,
    records: mpsc::UnboundedSender<ApprovalRecord>,
) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let (read, mut write) = stream.into_split();
    let mut lines = FramedRead::new(read, LinesCodec::new_with_max_length(MAX_MESSAGE_BYTES));
    let line = match next_line(&mut lines).await {
        Ok(Some(line)) => line,
        Ok(None) => return,
        Err(err) => {
            warn!(error = %err, "unreadable approval message");
            return;
        }
    };
    match serde_json::from_str::<Message>(&line) {
        Ok(Message::Ask { request }) => {
            let request = ApprovalRequest::from(request);
            debug!(tool = %request.tool, call_id = %request.call_id, "approval asked over the bridge");
            let mut read = lines.into_inner();
            let decision = tokio::select! {
                decision = gate.decide(request) => decision,
                // The server hung up (the CLI was cancelled or exited): withdraw the question.
                _ = async { while read.read(&mut [0u8; 64]).await.is_ok_and(|n| n > 0) {} } => {
                    debug!("approval asker went away");
                    return;
                }
            };
            let reply = Reply {
                decision: decision.into(),
            };
            match to_line(&reply) {
                Ok(line) => {
                    if let Err(err) = write.write_all(&line).await {
                        warn!(error = %err, "could not send the approval decision");
                    }
                }
                Err(err) => warn!(error = %err, "could not encode the approval decision"),
            }
        }
        Ok(Message::Record { record }) => {
            let _ = records.send(record.into());
        }
        Err(err) => warn!(error = %err, "malformed approval message"),
    }
}

/// The next line of `lines`, with codec errors as `io::Error`s.
pub(crate) async fn next_line<R: tokio::io::AsyncRead + Unpin>(
    lines: &mut FramedRead<R, LinesCodec>,
) -> io::Result<Option<String>> {
    match lines.next().await {
        None => Ok(None),
        Some(Ok(line)) => Ok(Some(line)),
        Some(Err(LinesCodecError::Io(err))) => Err(err),
        Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
            Err(io::Error::new(io::ErrorKind::InvalidData, "line too long"))
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Server side

/// The [`ApprovalGate`] used inside `rozu mcp-serve`: asks the parent over the socket and
/// denies when it can't.
#[derive(Debug, Clone, Default)]
pub struct SocketGate {
    path: Option<PathBuf>,
}

impl SocketGate {
    pub fn new(path: Option<PathBuf>) -> Self {
        Self { path }
    }

    /// The socket from [`APPROVAL_SOCKET_ENV`], if set.
    pub fn from_env() -> Self {
        Self::new(
            std::env::var_os(APPROVAL_SOCKET_ENV)
                .filter(|path| !path.is_empty())
                .map(PathBuf::from),
        )
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Sends a decision-log entry to the parent; best effort.
    pub async fn send_record(&self, record: ApprovalRecord) {
        let message = Message::Record {
            record: record.into(),
        };
        if let Err(err) = self.exchange(&message, false).await {
            debug!(error = %err, "could not forward the approval record");
        }
    }

    async fn ask(&self, request: ApprovalRequest) -> io::Result<Decision> {
        let message = Message::Ask {
            request: request.into(),
        };
        let reply = self.exchange(&message, true).await?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "rozu closed without answering",
            )
        })?;
        let reply: Reply = serde_json::from_str(&reply)
            .map_err(|err| io::Error::new(io::ErrorKind::InvalidData, err))?;
        Ok(reply.decision.into())
    }

    /// Writes `message` on a fresh connection and, if `answer`, reads one line back.
    #[cfg(unix)]
    async fn exchange(&self, message: &Message, answer: bool) -> io::Result<Option<String>> {
        use tokio::io::AsyncWriteExt;

        let path = self.path.as_deref().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("{APPROVAL_SOCKET_ENV} is not set"),
            )
        })?;
        let mut stream = tokio::net::UnixStream::connect(path).await?;
        stream.write_all(&to_line(message)?).await?;
        if !answer {
            return Ok(None);
        }
        let mut lines = FramedRead::new(stream, LinesCodec::new_with_max_length(MAX_MESSAGE_BYTES));
        next_line(&mut lines).await
    }

    #[cfg(not(unix))]
    async fn exchange(&self, _message: &Message, _answer: bool) -> io::Result<Option<String>> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the approval bridge needs Unix sockets",
        ))
    }
}

#[async_trait]
impl ApprovalGate for SocketGate {
    async fn decide(&self, request: ApprovalRequest) -> Decision {
        let tool = request.tool.clone();
        match self.ask(request).await {
            Ok(decision) => decision,
            Err(err) => {
                warn!(target: "rozu::approval", %tool, error = %err,
                    "approval channel to rozu unreachable; denying");
                Decision::Deny {
                    reason: format!(
                        "rozu could not ask the user ({err}), so the call is denied by default"
                    ),
                }
            }
        }
    }
}
