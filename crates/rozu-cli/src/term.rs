//! `rozu term`: runs `$SHELL` in a rozu PTY session and proxies it to this terminal.
//!
//! A smoke test for `rozu-pty`: keystrokes go to the PTY byte-for-byte (the local terminal is in
//! raw mode), output comes back through the session's byte stream, and size changes are
//! forwarded. Exit the shell to return.

use std::io::{self, IsTerminal, Read, Write};
use std::process::ExitCode;
use std::sync::Arc;
use std::sync::mpsc::RecvTimeoutError;
use std::time::Duration;

use anyhow::{Context, bail};
use crossterm::terminal;
use rozu_pty::{PtyOptions, PtySession, TermSize};

/// Restores the terminal mode even on early return or panic.
struct RawMode;

impl RawMode {
    fn enable() -> io::Result<Self> {
        terminal::enable_raw_mode()?;
        Ok(Self)
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
    }
}

fn current_size() -> TermSize {
    terminal::size()
        .map(|(cols, rows)| TermSize { cols, rows })
        .unwrap_or_default()
}

pub fn run() -> anyhow::Result<ExitCode> {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        bail!("rozu term needs an interactive terminal");
    }
    let mut size = current_size();
    let (session, output) = PtySession::spawn_subscribed(PtyOptions::shell().size(size))
        .context("could not start the shell")?;
    let session = Arc::new(session);
    eprintln!("rozu term: your shell in a rozu PTY. Exit the shell to come back.\r");

    let raw = RawMode::enable().context("could not switch the terminal to raw mode")?;

    // Keystrokes → PTY. The thread stays blocked on stdin until the process exits.
    {
        let session = session.clone();
        std::thread::spawn(move || {
            let mut stdin = io::stdin();
            let mut buf = [0u8; 4096];
            while let Ok(n) = stdin.read(&mut buf) {
                if n == 0 || session.write(&buf[..n]).is_err() {
                    break;
                }
            }
        });
    }

    // PTY → stdout, checking for size changes between chunks.
    let mut stdout = io::stdout();
    loop {
        match output.recv_timeout(Duration::from_millis(100)) {
            Ok(bytes) => {
                stdout.write_all(&bytes)?;
                stdout.flush()?;
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => break,
        }
        let now = current_size();
        if now != size {
            size = now;
            let _ = session.resize(size);
        }
    }

    let exit = session.wait_timeout(Duration::from_secs(2));
    drop(raw);
    match exit {
        Some(exit) => {
            eprintln!("rozu term: shell exited with code {}", exit.code);
            Ok(ExitCode::from(u8::try_from(exit.code).unwrap_or(1)))
        }
        None => {
            eprintln!("rozu term: shell output closed");
            Ok(ExitCode::SUCCESS)
        }
    }
}
