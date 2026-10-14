//! [`CliProcess`]: a vendor CLI running as a child process that speaks JSON lines on stdio.
//!
//! The child runs in its own process group with piped stdio and `kill_on_drop`; [`CliProcess::kill`]
//! (also run on drop) kills the whole group, so helpers the CLI started (its MCP servers) go too.
//! stdout is read as JSON lines of at most [`CliCommand::max_line_bytes`]; stderr goes to the log
//! file (target `rozu::cli`), never to the terminal, and its last lines are kept in memory only
//! to classify failures.

use std::collections::VecDeque;
use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{ExitStatus, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::StreamExt;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};
use tokio_util::codec::{FramedRead, LinesCodec, LinesCodecError};
use tracing::{info, warn};

/// Default longest stdout line accepted (tool results with whole files can be large).
pub const DEFAULT_MAX_LINE_BYTES: usize = 16 << 20;
/// stderr lines kept for error classification.
const STDERR_TAIL_LINES: usize = 20;

/// What to run.
#[derive(Debug, Clone)]
pub struct CliCommand {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub cwd: Option<PathBuf>,
    /// Added to the inherited environment.
    pub env: Vec<(OsString, OsString)>,
    /// Removed from the inherited environment.
    pub env_remove: Vec<OsString>,
    pub max_line_bytes: usize,
    /// Short name for logs, e.g. `claude`.
    pub label: String,
}

impl CliCommand {
    pub fn new(label: impl Into<String>, program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            env_remove: Vec::new(),
            max_line_bytes: DEFAULT_MAX_LINE_BYTES,
            label: label.into(),
        }
    }
}

/// Reading the child's stdout failed.
#[derive(Debug)]
pub enum ReadError {
    Io(io::Error),
    /// A line over the size limit; the stream can't continue after it.
    LineTooLong(usize),
}

impl std::error::Error for ReadError {}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadError::Io(err) => write!(f, "{err}"),
            ReadError::LineTooLong(max) => write!(f, "output line longer than {max} bytes"),
        }
    }
}

/// A running CLI.
pub struct CliProcess {
    label: String,
    child: Child,
    stdin: Option<ChildStdin>,
    lines: FramedRead<ChildStdout, LinesCodec>,
    max_line_bytes: usize,
    pgid: Option<u32>,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    /// The task draining stderr into `stderr_tail`; ends when the child closes stderr.
    stderr_reader: Option<tokio::task::JoinHandle<()>>,
}

impl CliProcess {
    /// Starts `cmd`. A missing binary is an [`io::ErrorKind::NotFound`] error.
    pub fn spawn(cmd: &CliCommand) -> io::Result<Self> {
        let mut command = Command::new(&cmd.program);
        command
            .args(&cmd.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        if let Some(cwd) = &cmd.cwd {
            command.current_dir(cwd);
        }
        for var in &cmd.env_remove {
            command.env_remove(var);
        }
        for (key, value) in &cmd.env {
            command.env(key, value);
        }
        #[cfg(unix)]
        command.process_group(0);

        let mut child = command.spawn()?;
        let pgid = child.id();
        let stdin = child.stdin.take();
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout pipe"))?;
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));
        let stderr_reader = child.stderr.take().map(|stderr| {
            let tail = stderr_tail.clone();
            let label = cmd.label.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    info!(target: "rozu::cli", cli = %label, "stderr: {line}");
                    if let Ok(mut tail) = tail.lock() {
                        if tail.len() == STDERR_TAIL_LINES {
                            tail.pop_front();
                        }
                        tail.push_back(line);
                    }
                }
            })
        });
        info!(target: "rozu::cli", cli = %cmd.label, pid = ?pgid, program = %cmd.program.display(), "started");
        Ok(Self {
            label: cmd.label.clone(),
            child,
            stdin,
            lines: FramedRead::new(stdout, LinesCodec::new_with_max_length(cmd.max_line_bytes)),
            max_line_bytes: cmd.max_line_bytes,
            pgid,
            stderr_tail,
            stderr_reader,
        })
    }

    /// Writes `value` as one line to the child's stdin.
    pub async fn send_json(&mut self, value: &Value) -> io::Result<()> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "stdin is closed"))?;
        let mut line = value.to_string();
        line.push('\n');
        stdin.write_all(line.as_bytes()).await?;
        stdin.flush().await
    }

    /// The next JSON line from stdout; `None` at end of output. Lines that aren't JSON are logged
    /// and skipped.
    pub async fn next_json(&mut self) -> Result<Option<Value>, ReadError> {
        loop {
            let line = match self.lines.next().await {
                None => return Ok(None),
                Some(Ok(line)) => line,
                Some(Err(LinesCodecError::Io(err))) => return Err(ReadError::Io(err)),
                Some(Err(LinesCodecError::MaxLineLengthExceeded)) => {
                    return Err(ReadError::LineTooLong(self.max_line_bytes));
                }
            };
            if line.trim().is_empty() {
                continue;
            }
            match serde_json::from_str(&line) {
                Ok(value) => return Ok(Some(value)),
                Err(err) => {
                    warn!(target: "rozu::cli", cli = %self.label, error = %err,
                        "skipping a stdout line that isn't JSON");
                }
            }
        }
    }

    /// The last few stderr lines (for classifying failures; never shown verbatim). Once the
    /// child has exited this waits briefly for the reader to drain what it wrote last, so a
    /// "not logged in" line printed just before exit is not missed.
    pub async fn stderr_tail(&mut self) -> Vec<String> {
        if let Some(reader) = self.stderr_reader.take() {
            let _ = tokio::time::timeout(Duration::from_millis(500), reader).await;
        }
        self.stderr_tail
            .lock()
            .map(|tail| tail.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Waits up to `grace` for the child to exit, e.g. after its output ended.
    pub async fn exit_status(&mut self, grace: Duration) -> Option<ExitStatus> {
        tokio::time::timeout(grace, self.child.wait())
            .await
            .ok()
            .and_then(Result::ok)
    }

    /// Kills the child's whole process group.
    pub fn kill(&mut self) {
        self.stdin = None;
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.and_then(|p| libc::pid_t::try_from(p).ok()) {
            // SAFETY: killpg only sends a signal; an already-gone group returns ESRCH.
            unsafe {
                libc::killpg(pgid, libc::SIGKILL);
            }
        }
        let _ = self.child.start_kill();
    }
}

impl Drop for CliProcess {
    fn drop(&mut self) {
        self.kill();
    }
}
