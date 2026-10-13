//! Ctrl-C handling for `rozu chat`: the first Ctrl-C during a turn cancels the turn; a second one
//! during the same turn, or one at the prompt, exits with status 130.
//!
//! Signals are counted, not coalesced: on Unix a raw `SIGINT` handler bumps an atomic press
//! counter and, when the press means "exit", calls `_exit(130)` right there. Exiting doesn't wait
//! on the async runtime, a lock, stdin or the turn winding down, so a fast double Ctrl-C can't
//! hang. The tokio signal stream only does the part that isn't signal-safe: cancelling the turn
//! and printing the hint.

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;

/// Exit status for an interrupted run (128 + SIGINT).
pub const EXIT_INTERRUPTED: i32 = 130;

/// What a Ctrl-C press should do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Press {
    /// Cancel the running turn and keep going.
    CancelTurn,
    /// Exit the process now.
    Exit,
}

/// Lock-free press counter shared by the signal handler and the chat loop.
#[derive(Debug, Default)]
pub struct PressState {
    /// Presses since the current turn started.
    presses: AtomicUsize,
    turn_active: AtomicBool,
}

impl PressState {
    pub const fn new() -> Self {
        Self {
            presses: AtomicUsize::new(0),
            turn_active: AtomicBool::new(false),
        }
    }

    /// Records one press and says what to do. Async-signal-safe (atomics only).
    pub fn press(&self) -> Press {
        let presses = self.presses.fetch_add(1, Ordering::SeqCst) + 1;
        if self.turn_active.load(Ordering::SeqCst) && presses == 1 {
            Press::CancelTurn
        } else {
            Press::Exit
        }
    }

    pub fn begin_turn(&self) {
        self.presses.store(0, Ordering::SeqCst);
        self.turn_active.store(true, Ordering::SeqCst);
    }

    pub fn end_turn(&self) {
        self.turn_active.store(false, Ordering::SeqCst);
    }
}

/// The process-wide state the `SIGINT` handler reads.
static PRESSES: PressState = PressState::new();

/// Installed once per process by [`Interrupts::install`].
#[derive(Clone)]
pub struct Interrupts {
    state: &'static PressState,
    current: Arc<Mutex<Option<CancellationToken>>>,
}

impl Interrupts {
    /// Installs the Ctrl-C handling. Must be called inside a tokio runtime.
    pub fn install() -> Self {
        let this = Self {
            state: &PRESSES,
            current: Arc::default(),
        };
        install_exit_handler();
        let current = this.current.clone();
        tokio::spawn(async move {
            let mut presses = Presses::listen();
            while presses.next().await {
                // On Unix the raw handler has already exited if this press meant exit.
                #[cfg(not(unix))]
                if PRESSES.press() == Press::Exit {
                    exit_now();
                }
                let token = current.lock().ok().and_then(|token| token.clone());
                if let Some(token) = token.filter(|token| !token.is_cancelled()) {
                    token.cancel();
                    eprintln!("\n^C cancelling; press Ctrl-C again to exit");
                }
            }
        });
        this
    }

    pub fn begin_turn(&self) -> CancellationToken {
        let token = CancellationToken::new();
        if let Ok(mut current) = self.current.lock() {
            *current = Some(token.clone());
        }
        self.state.begin_turn();
        token
    }

    pub fn end_turn(&self) {
        self.state.end_turn();
        if let Ok(mut current) = self.current.lock() {
            *current = None;
        }
    }
}

/// Wakes the async side once per delivered `SIGINT` (several may arrive as one wake-up; the
/// count that matters lives in [`PRESSES`]).
struct Presses {
    #[cfg(unix)]
    signal: Option<tokio::signal::unix::Signal>,
}

impl Presses {
    fn listen() -> Self {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Self {
                signal: signal(SignalKind::interrupt()).ok(),
            }
        }
        #[cfg(not(unix))]
        Self {}
    }

    /// Waits for the next press; `false` if presses can no longer be observed.
    async fn next(&mut self) -> bool {
        #[cfg(unix)]
        {
            match &mut self.signal {
                Some(signal) => signal.recv().await.is_some(),
                None => false,
            }
        }
        #[cfg(not(unix))]
        tokio::signal::ctrl_c().await.is_ok()
    }
}

/// Registers the raw `SIGINT` action that counts presses and exits on an exit press.
#[cfg(unix)]
fn install_exit_handler() {
    static INSTALLED: std::sync::Once = std::sync::Once::new();
    INSTALLED.call_once(|| {
        // SAFETY: the action only touches atomics and calls `write` and `_exit`, all of which
        // are async-signal-safe.
        let result = unsafe {
            signal_hook_registry::register(libc::SIGINT, || {
                if PRESSES.press() == Press::Exit {
                    exit_now();
                }
            })
        };
        if let Err(err) = result {
            tracing::warn!(error = %err, "could not install the Ctrl-C handler");
        }
    });
}

#[cfg(not(unix))]
fn install_exit_handler() {}

/// Ends the line the user was on and exits with [`EXIT_INTERRUPTED`] without running
/// destructors or flushing through locks. Async-signal-safe on Unix.
fn exit_now() -> ! {
    #[cfg(unix)]
    // SAFETY: `write` and `_exit` are async-signal-safe; the buffer is a static byte string.
    unsafe {
        libc::write(libc::STDERR_FILENO, b"\n".as_ptr().cast(), 1);
        libc::_exit(EXIT_INTERRUPTED);
    }
    #[cfg(not(unix))]
    {
        eprintln!();
        std::process::exit(EXIT_INTERRUPTED);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn press_at_the_prompt_exits() {
        let state = PressState::new();
        assert_eq!(state.press(), Press::Exit);
    }

    #[test]
    fn first_press_in_a_turn_cancels_and_second_exits() {
        let state = PressState::new();
        state.begin_turn();
        assert_eq!(state.press(), Press::CancelTurn);
        assert_eq!(state.press(), Press::Exit);
        assert_eq!(state.press(), Press::Exit);
    }

    #[test]
    fn every_turn_gets_a_fresh_count() {
        let state = PressState::new();
        state.begin_turn();
        assert_eq!(state.press(), Press::CancelTurn);
        state.end_turn();
        state.begin_turn();
        assert_eq!(state.press(), Press::CancelTurn);
        state.end_turn();
        // Back at the prompt.
        assert_eq!(state.press(), Press::Exit);
    }

    #[test]
    fn presses_are_counted_not_coalesced_across_threads() {
        let state = std::sync::Arc::new(PressState::new());
        state.begin_turn();
        let results: Vec<Press> = (0..8)
            .map(|_| {
                let state = state.clone();
                std::thread::spawn(move || state.press())
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        // Exactly one press cancels; every other one exits.
        assert_eq!(
            results.iter().filter(|p| **p == Press::CancelTurn).count(),
            1
        );
        assert_eq!(results.iter().filter(|p| **p == Press::Exit).count(), 7);
    }
}
