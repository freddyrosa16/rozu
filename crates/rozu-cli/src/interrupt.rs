//! Ctrl-C handling for `rozu chat`: the first Ctrl-C during a turn cancels the turn; a second one
//! during the same turn, or one at the prompt, exits with status 130.
//!
//! While a turn runs on a terminal, rozu reads Ctrl-C itself: [`tty::enter_turn_mode`] turns
//! off `ISIG` (so the kernel sends no `SIGINT`) and makes `^C` an extra line delimiter (`VEOL`),
//! so the stdin reader sees every `0x03` byte as soon as it's typed and counts each one through
//! [`Interrupts::tty_press`]. That matters because POSIX doesn't queue signals: two `^C` bytes
//! arriving in one write (pasted input, tmux `send-keys`, a slow SSH link) used to merge into
//! one `SIGINT`. Line editing and echo stay as they are, so the approval prompt still works.
//! The terminal settings are restored when the turn ends, on exit and on panic.
//!
//! Signals remain the path at the prompt and when stdin isn't a terminal: on Unix a raw
//! `SIGINT` handler bumps an atomic press counter and, when the press means "exit", calls
//! `_exit(130)` right there. Exiting doesn't wait on the async runtime, a lock, stdin or the turn
//! winding down, so a fast double Ctrl-C can't hang. The tokio signal stream only does the part
//! that isn't signal-safe: cancelling the turn and printing the hint.

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
        tty::restore_on_panic();
        let current = this.current.clone();
        tokio::spawn(async move {
            let mut presses = Presses::listen();
            while presses.next().await {
                // On Unix the raw handler has already exited if this press meant exit.
                #[cfg(not(unix))]
                if PRESSES.press() == Press::Exit {
                    exit_now();
                }
                cancel_current(&current);
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
        tty::enter_turn_mode();
        token
    }

    pub fn end_turn(&self) {
        tty::restore();
        self.state.end_turn();
        if let Ok(mut current) = self.current.lock() {
            *current = None;
        }
    }

    /// One `^C` byte read from the terminal (turn mode): counted exactly like a `SIGINT`.
    pub fn tty_press(&self) {
        match self.state.press() {
            Press::Exit => exit_now(),
            Press::CancelTurn => cancel_current(&self.current),
        }
    }
}

/// Cancels the running turn, if any, and says how to exit.
fn cancel_current(current: &Mutex<Option<CancellationToken>>) {
    let token = current.lock().ok().and_then(|token| token.clone());
    if let Some(token) = token.filter(|token| !token.is_cancelled()) {
        token.cancel();
        eprintln!("\n^C cancelling; press Ctrl-C again to exit");
    }
}

/// The terminal's turn mode (see the module docs).
#[cfg(unix)]
pub mod tty {
    use std::cell::UnsafeCell;
    use std::mem::MaybeUninit;
    use std::sync::atomic::{AtomicBool, Ordering};

    /// The settings from before the first turn; written once, before `VALID` is set.
    struct Saved(UnsafeCell<MaybeUninit<libc::termios>>);

    // SAFETY: written only once, by the thread that sets `VALID` afterwards (Release); read only
    // after `VALID` is seen set (Acquire).
    unsafe impl Sync for Saved {}

    static SAVED: Saved = Saved(UnsafeCell::new(MaybeUninit::uninit()));
    static VALID: AtomicBool = AtomicBool::new(false);
    /// Turn mode is on.
    static ACTIVE: AtomicBool = AtomicBool::new(false);

    /// While a turn runs: no `SIGINT` from the terminal; `^C` ends a line so the reader sees it
    /// at once. Does nothing (and returns false) when stdin isn't a terminal.
    pub fn enter_turn_mode() -> bool {
        let fd = libc::STDIN_FILENO;
        // SAFETY: isatty/tcgetattr/tcsetattr on our own stdin, with a valid termios buffer.
        unsafe {
            if libc::isatty(fd) != 1 {
                return false;
            }
            let mut current = MaybeUninit::<libc::termios>::uninit();
            if libc::tcgetattr(fd, current.as_mut_ptr()) != 0 {
                return false;
            }
            let current = current.assume_init();
            if !VALID.load(Ordering::Acquire) {
                (*SAVED.0.get()).write(current);
                VALID.store(true, Ordering::Release);
            }
            let mut turn = current;
            turn.c_lflag &= !libc::ISIG;
            turn.c_cc[libc::VEOL] = super::CTRL_C;
            if libc::tcsetattr(fd, libc::TCSANOW, &turn) != 0 {
                return false;
            }
        }
        ACTIVE.store(true, Ordering::SeqCst);
        true
    }

    /// Puts the terminal back as it was. Async-signal-safe (atomics and `tcsetattr`).
    pub fn restore() {
        if ACTIVE.swap(false, Ordering::SeqCst) && VALID.load(Ordering::Acquire) {
            // SAFETY: SAVED was written before VALID was set; tcsetattr only reads it.
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, (*SAVED.0.get()).as_ptr());
            }
        }
    }

    /// Restores the terminal before the default panic output.
    pub fn restore_on_panic() {
        static INSTALLED: std::sync::Once = std::sync::Once::new();
        INSTALLED.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                restore();
                previous(info);
            }));
        });
    }
}

#[cfg(not(unix))]
pub mod tty {
    pub fn enter_turn_mode() -> bool {
        false
    }
    pub fn restore() {}
    pub fn restore_on_panic() {}
}

const CTRL_C: u8 = 0x03;

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

/// Restores the terminal, ends the line the user was on and exits with [`EXIT_INTERRUPTED`]
/// without running destructors or flushing through locks. Async-signal-safe on Unix.
fn exit_now() -> ! {
    tty::restore();
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
        assert_eq!(
            results.iter().filter(|p| **p == Press::CancelTurn).count(),
            1
        );
        assert_eq!(results.iter().filter(|p| **p == Press::Exit).count(), 7);
    }
}
