//! Interactive terminal sessions for rozu.
//!
//! [`PtySession`] spawns a program (by default the user's `$SHELL`) in a PTY (`portable-pty`)
//! and feeds its output through a VT parser (`alacritty_terminal`) that keeps the screen grid.
//! It is UI-agnostic: a GUI can render [`PtySession::snapshot`] and stream raw bytes from
//! [`PtySession::subscribe`], and an agent tool can read the visible screen as text
//! ([`PtySession::screen_text`]) and, with approval, [`PtySession::write`] to it.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::Processor;
use portable_pty::{ChildKiller, CommandBuilder, MasterPty, PtySize, native_pty_system};

/// Version of this crate.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, thiserror::Error)]
pub enum PtyError {
    #[error("could not open a PTY: {0}")]
    Open(String),
    #[error("could not start {program}: {message}")]
    Spawn { program: String, message: String },
    #[error("could not resize the PTY: {0}")]
    Resize(String),
    #[error(transparent)]
    Io(#[from] io::Error),
}

/// Terminal size in character cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermSize {
    pub cols: u16,
    pub rows: u16,
}

impl Default for TermSize {
    fn default() -> Self {
        Self { cols: 80, rows: 24 }
    }
}

impl TermSize {
    fn pty(self) -> PtySize {
        PtySize {
            rows: self.rows.max(1),
            cols: self.cols.max(1),
            pixel_width: 0,
            pixel_height: 0,
        }
    }
}

impl Dimensions for TermSize {
    fn total_lines(&self) -> usize {
        self.screen_lines()
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows.max(1))
    }

    fn columns(&self) -> usize {
        usize::from(self.cols.max(1))
    }
}

/// What to run in the PTY.
#[derive(Debug, Clone, Default)]
pub struct PtyOptions {
    /// Program to run. Defaults to `$SHELL`, falling back to `/bin/sh`.
    pub program: Option<String>,
    pub args: Vec<String>,
    /// Working directory. Defaults to the current one.
    pub cwd: Option<PathBuf>,
    /// Extra environment variables (`TERM=xterm-256color` is always set).
    pub env: Vec<(String, String)>,
    pub size: TermSize,
}

impl PtyOptions {
    /// The user's shell.
    pub fn shell() -> Self {
        Self::default()
    }

    /// A specific program with arguments.
    pub fn command(
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
    ) -> Self {
        Self {
            program: Some(program.into()),
            args: args.into_iter().map(Into::into).collect(),
            ..Self::default()
        }
    }

    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    pub fn size(mut self, size: TermSize) -> Self {
        self.size = size;
        self
    }

    fn program(&self) -> String {
        self.program
            .clone()
            .or_else(|| std::env::var("SHELL").ok().filter(|s| !s.is_empty()))
            .unwrap_or_else(|| "/bin/sh".to_owned())
    }
}

/// The visible screen at one moment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScreenSnapshot {
    /// One string per visible row, trailing blanks trimmed.
    pub lines: Vec<String>,
    /// Cursor position as (row, column), 0-based.
    pub cursor: (usize, usize),
    pub size: TermSize,
}

impl ScreenSnapshot {
    /// The rows joined with newlines, without trailing empty rows.
    pub fn text(&self) -> String {
        let end = self
            .lines
            .iter()
            .rposition(|line| !line.is_empty())
            .map_or(0, |i| i + 1);
        self.lines[..end].join("\n")
    }
}

/// How the child exited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: u32,
    pub success: bool,
}

type SharedWriter = Arc<Mutex<Box<dyn Write + Send>>>;
type Subscribers = Arc<Mutex<Vec<mpsc::Sender<Vec<u8>>>>>;

/// Answers terminal queries (cursor position, device attributes) by writing to the PTY.
#[derive(Clone)]
struct Listener {
    writer: SharedWriter,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            if let Ok(mut writer) = self.writer.lock() {
                let _ = writer.write_all(text.as_bytes());
                let _ = writer.flush();
            }
        }
    }
}

#[derive(Default)]
struct ExitState {
    status: Mutex<Option<ExitInfo>>,
    changed: Condvar,
}

/// A program running in a PTY, with its screen kept up to date.
pub struct PtySession {
    master: Mutex<Box<dyn MasterPty + Send>>,
    writer: SharedWriter,
    term: Arc<FairMutex<Term<Listener>>>,
    killer: Mutex<Box<dyn ChildKiller + Send + Sync>>,
    subscribers: Subscribers,
    exit: Arc<ExitState>,
    size: Mutex<TermSize>,
    pid: Option<u32>,
}

impl std::fmt::Debug for PtySession {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtySession")
            .field("pid", &self.pid)
            .field("size", &self.size())
            .field("exit", &self.exit_status())
            .finish()
    }
}

impl PtySession {
    /// Opens a PTY and starts `options.program` in it.
    pub fn spawn(options: PtyOptions) -> Result<Self, PtyError> {
        Self::start(options, Vec::new())
    }

    /// Like [`PtySession::spawn`], plus a subscription that sees the output from the very
    /// first byte (a later [`PtySession::subscribe`] could miss the start).
    pub fn spawn_subscribed(
        options: PtyOptions,
    ) -> Result<(Self, mpsc::Receiver<Vec<u8>>), PtyError> {
        let (tx, rx) = mpsc::channel();
        Ok((Self::start(options, vec![tx])?, rx))
    }

    fn start(options: PtyOptions, initial: Vec<mpsc::Sender<Vec<u8>>>) -> Result<Self, PtyError> {
        let size = options.size;
        let pair = native_pty_system()
            .openpty(size.pty())
            .map_err(|err| PtyError::Open(err.to_string()))?;

        let program = options.program();
        let mut cmd = CommandBuilder::new(&program);
        cmd.args(&options.args);
        if let Some(cwd) = &options.cwd {
            cmd.cwd(cwd);
        } else if let Ok(cwd) = std::env::current_dir() {
            cmd.cwd(cwd);
        }
        cmd.env("TERM", "xterm-256color");
        for (key, value) in &options.env {
            cmd.env(key, value);
        }
        let mut child = pair
            .slave
            .spawn_command(cmd)
            .map_err(|err| PtyError::Spawn {
                program: program.clone(),
                message: err.to_string(),
            })?;
        // Only the child keeps the slave open, so the reader sees EOF when it exits.
        drop(pair.slave);

        let pid = child.process_id();
        let killer = child.clone_killer();
        let mut reader = pair
            .master
            .try_clone_reader()
            .map_err(|err| PtyError::Open(err.to_string()))?;
        let writer: SharedWriter = Arc::new(Mutex::new(
            pair.master
                .take_writer()
                .map_err(|err| PtyError::Open(err.to_string()))?,
        ));
        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &size,
            Listener {
                writer: writer.clone(),
            },
        )));
        let subscribers: Subscribers = Arc::new(Mutex::new(initial));
        let exit = Arc::new(ExitState::default());

        {
            let term = term.clone();
            let subscribers = subscribers.clone();
            thread::Builder::new()
                .name("rozu-pty-reader".into())
                .spawn(move || {
                    let mut parser: Processor = Processor::new();
                    let mut buf = vec![0u8; 64 * 1024];
                    loop {
                        match reader.read(&mut buf) {
                            Ok(0) | Err(_) => break,
                            Ok(n) => {
                                parser.advance(&mut *term.lock(), &buf[..n]);
                                if let Ok(mut subs) = subscribers.lock() {
                                    subs.retain(|tx| tx.send(buf[..n].to_vec()).is_ok());
                                }
                            }
                        }
                    }
                    // Disconnect subscribers so their receivers end.
                    if let Ok(mut subs) = subscribers.lock() {
                        subs.clear();
                    }
                })?;
        }
        {
            let exit = exit.clone();
            thread::Builder::new()
                .name("rozu-pty-wait".into())
                .spawn(move || {
                    let info = match child.wait() {
                        Ok(status) => ExitInfo {
                            code: status.exit_code(),
                            success: status.success(),
                        },
                        Err(_) => ExitInfo {
                            code: 1,
                            success: false,
                        },
                    };
                    if let Ok(mut status) = exit.status.lock() {
                        *status = Some(info);
                    }
                    exit.changed.notify_all();
                })?;
        }

        Ok(Self {
            master: Mutex::new(pair.master),
            writer,
            term,
            killer: Mutex::new(killer),
            subscribers,
            exit,
            size: Mutex::new(size),
            pid,
        })
    }

    /// The child's process id, if known.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Sends input (keystrokes, pasted text) to the program.
    pub fn write(&self, bytes: &[u8]) -> io::Result<()> {
        let mut writer = self
            .writer
            .lock()
            .map_err(|_| io::Error::other("PTY writer poisoned"))?;
        writer.write_all(bytes)?;
        writer.flush()
    }

    /// Resizes the PTY (the program gets SIGWINCH) and the screen grid.
    pub fn resize(&self, size: TermSize) -> Result<(), PtyError> {
        self.master
            .lock()
            .map_err(|_| PtyError::Resize("PTY poisoned".into()))?
            .resize(size.pty())
            .map_err(|err| PtyError::Resize(err.to_string()))?;
        self.term.lock().resize(size);
        if let Ok(mut current) = self.size.lock() {
            *current = size;
        }
        Ok(())
    }

    pub fn size(&self) -> TermSize {
        self.size.lock().map(|s| *s).unwrap_or_default()
    }

    /// Raw output from now on. The channel closes when the program's output ends.
    pub fn subscribe(&self) -> mpsc::Receiver<Vec<u8>> {
        let (tx, rx) = mpsc::channel();
        if let Ok(mut subs) = self.subscribers.lock() {
            subs.push(tx);
        }
        rx
    }

    /// The visible screen.
    pub fn snapshot(&self) -> ScreenSnapshot {
        let term = self.term.lock();
        let grid = term.grid();
        let (rows, cols) = (grid.screen_lines(), grid.columns());
        let mut lines = Vec::with_capacity(rows);
        for row in 0..rows {
            let cells = &grid[Line(row as i32)];
            let mut line = String::with_capacity(cols);
            for col in 0..cols {
                let cell = &cells[Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                line.push(cell.c);
            }
            lines.push(line.trim_end().to_owned());
        }
        let cursor = grid.cursor.point;
        ScreenSnapshot {
            lines,
            cursor: (cursor.line.0.max(0) as usize, cursor.column.0),
            size: TermSize {
                cols: cols as u16,
                rows: rows as u16,
            },
        }
    }

    /// The visible screen as text (see [`ScreenSnapshot::text`]).
    pub fn screen_text(&self) -> String {
        self.snapshot().text()
    }

    /// Polls the screen until it contains `needle`. Returns whether it appeared in time.
    pub fn wait_for_text(&self, needle: &str, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if self.screen_text().contains(needle) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// The exit status, once the program has exited.
    pub fn exit_status(&self) -> Option<ExitInfo> {
        self.exit.status.lock().ok().and_then(|s| s.clone())
    }

    /// Waits up to `timeout` for the program to exit.
    pub fn wait_timeout(&self, timeout: Duration) -> Option<ExitInfo> {
        let guard = self.exit.status.lock().ok()?;
        let (guard, _) = self
            .exit
            .changed
            .wait_timeout_while(guard, timeout, |status| status.is_none())
            .ok()?;
        guard.clone()
    }

    /// Kills the program.
    pub fn kill(&self) -> io::Result<()> {
        if self.exit_status().is_some() {
            return Ok(());
        }
        self.killer
            .lock()
            .map_err(|_| io::Error::other("PTY killer poisoned"))?
            .kill()
    }
}

impl Drop for PtySession {
    fn drop(&mut self) {
        let _ = self.kill();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    const WAIT: Duration = Duration::from_secs(10);

    fn sh() -> PtySession {
        PtySession::spawn(
            PtyOptions::command("/bin/sh", Vec::<String>::new())
                .env("PS1", "$ ")
                .env("ENV", ""),
        )
        .unwrap()
    }

    fn drain(rx: &mpsc::Receiver<Vec<u8>>) -> String {
        let mut seen = Vec::new();
        while let Ok(chunk) = rx.recv_timeout(WAIT) {
            seen.extend(chunk);
        }
        String::from_utf8_lossy(&seen).into_owned()
    }

    #[test]
    fn version_is_semver() {
        assert_eq!(VERSION.split('.').count(), 3);
    }

    #[test]
    fn captures_program_output_on_screen() {
        let session = PtySession::spawn(PtyOptions::command(
            "/bin/sh",
            // The pause keeps the slave open until the output is read (Linux can drop
            // unread PTY output once the child exits).
            ["-c", "printf 'hello from pty\\n'; sleep 0.5; exit 3"],
        ))
        .unwrap();
        assert!(
            session.wait_for_text("hello from pty", WAIT),
            "{:?}",
            session.snapshot()
        );
        let exit = session.wait_timeout(WAIT).expect("exited");
        assert_eq!(exit.code, 3);
        assert!(!exit.success);
    }

    #[test]
    fn interactive_shell_round_trip() {
        let session = sh();
        session.write(b"echo $((40 + 2))\n").unwrap();
        assert!(
            session.wait_for_text("42", WAIT),
            "{}",
            session.screen_text()
        );
        session.write(b"exit\n").unwrap();
        assert!(session.wait_timeout(WAIT).is_some_and(|e| e.success));
    }

    #[test]
    fn resize_updates_pty_and_grid() {
        let session = sh();
        let size = TermSize {
            cols: 100,
            rows: 30,
        };
        session.resize(size).unwrap();
        assert_eq!(session.size(), size);
        let snapshot = session.snapshot();
        assert_eq!(snapshot.size, size);
        assert_eq!(snapshot.lines.len(), 30);
        session.write(b"stty size\n").unwrap();
        assert!(
            session.wait_for_text("30 100", WAIT),
            "{}",
            session.screen_text()
        );
    }

    #[test]
    fn subscribers_get_raw_bytes_until_exit() {
        let session = sh();
        let rx = session.subscribe();
        session
            .write(b"echo stream-test; sleep 0.5; exit\n")
            .unwrap();
        let seen = drain(&rx);
        assert!(seen.contains("stream-test"), "{seen}");
    }

    #[test]
    fn spawn_subscribed_sees_the_first_bytes() {
        let (_session, rx) = PtySession::spawn_subscribed(PtyOptions::command(
            "/bin/sh",
            ["-c", "echo first-bytes; sleep 0.5"],
        ))
        .unwrap();
        assert!(drain(&rx).contains("first-bytes"));
    }

    #[test]
    fn kill_stops_the_program() {
        let session = PtySession::spawn(PtyOptions::command("/bin/sleep", ["60"])).unwrap();
        session.kill().unwrap();
        let exit = session.wait_timeout(WAIT).expect("exited after kill");
        assert!(!exit.success);
    }

    #[test]
    fn snapshot_text_trims_trailing_blank_rows() {
        let snapshot = ScreenSnapshot {
            lines: vec!["a".into(), "".into(), "b".into(), "".into(), "".into()],
            cursor: (0, 0),
            size: TermSize { cols: 1, rows: 5 },
        };
        assert_eq!(snapshot.text(), "a\n\nb");
    }
}
