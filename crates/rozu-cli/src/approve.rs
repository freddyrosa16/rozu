//! Interactive approvals for `rozu chat`.
//!
//! The agent asks [`CliGate`], which forwards the request to the turn's render loop. The loop
//! prints the approval card in order with the other output and reads the answer from the shared
//! stdin [`Input`]: `y` approves (still sandboxed), `n` denies (with an optional reason sent back
//! to the model), `a` approves and always allows the command prefix in this project, and `u`
//! runs this one command without the sandbox. Without a terminal on stdin every prompt is
//! denied.

use std::io::{self, IsTerminal, Read, Write};
use std::sync::Arc;

use async_trait::async_trait;
use clap::ValueEnum;
use rozu_core::{ApprovalGate, ApprovalMode, ApprovalRequest, Decision};
use tokio::sync::{Mutex, mpsc, oneshot};

/// `--approval` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ApprovalArg {
    /// Only reads run on their own; everything else asks.
    ReadOnly,
    /// Reads and sandboxed workspace writes run on their own; network and outside writes ask.
    Auto,
    /// Everything runs without asking, still sandboxed, except destructive commands,
    /// interpreter code and output redirects.
    Full,
}

impl From<ApprovalArg> for ApprovalMode {
    fn from(arg: ApprovalArg) -> Self {
        match arg {
            ApprovalArg::ReadOnly => ApprovalMode::ReadOnly,
            ApprovalArg::Auto => ApprovalMode::AutoWorkspace,
            ApprovalArg::Full => ApprovalMode::FullAccess,
        }
    }
}

/// Lines from stdin, read on one background thread and shared by the chat prompt and the
/// approval prompt, so a cancelled approval doesn't leave a reader that swallows the next line.
#[derive(Clone)]
pub struct Input {
    lines: Arc<Mutex<mpsc::UnboundedReceiver<String>>>,
}

impl Input {
    /// Starts the reader. On a terminal, each `^C` byte (which arrives as input while a turn
    /// runs; see [`crate::interrupt`]) calls `on_ctrl_c` once and discards the partial line.
    pub fn spawn(on_ctrl_c: Option<Box<dyn Fn() + Send>>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        let tty = io::stdin().is_terminal();
        std::thread::spawn(move || {
            let mut stdin = io::stdin().lock();
            let mut buf = [0u8; 4096];
            let mut line = Vec::new();
            loop {
                let n = match stdin.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => n,
                    Err(err) if err.kind() == io::ErrorKind::Interrupted => continue,
                    Err(_) => break,
                };
                for &byte in &buf[..n] {
                    match byte {
                        // Every byte counts: two ^C in one read are two presses.
                        CTRL_C if tty => {
                            line.clear();
                            if let Some(on_ctrl_c) = &on_ctrl_c {
                                on_ctrl_c();
                            }
                        }
                        b'\n' => {
                            if tx.send(finish_line(&mut line)).is_err() {
                                return;
                            }
                        }
                        byte => line.push(byte),
                    }
                }
            }
            if !line.is_empty() {
                let _ = tx.send(finish_line(&mut line));
            }
        });
        Self {
            lines: Arc::new(Mutex::new(rx)),
        }
    }

    /// The next line, or `None` at end of input.
    pub async fn line(&self) -> Option<String> {
        self.lines.lock().await.recv().await
    }
}

const CTRL_C: u8 = 0x03;

/// The buffered bytes as a line (lossy UTF-8, without a trailing `\r`), emptying the buffer.
fn finish_line(line: &mut Vec<u8>) -> String {
    let mut text = String::from_utf8_lossy(line).into_owned();
    line.clear();
    if text.ends_with('\r') {
        text.pop();
    }
    text
}

/// A request waiting for the render loop to answer.
pub struct Ask {
    pub request: ApprovalRequest,
    pub reply: oneshot::Sender<Decision>,
}

/// The [`ApprovalGate`] handed to the agent.
pub struct CliGate {
    asks: mpsc::Sender<Ask>,
}

impl CliGate {
    pub fn new() -> (Arc<Self>, mpsc::Receiver<Ask>) {
        let (tx, rx) = mpsc::channel(4);
        (Arc::new(Self { asks: tx }), rx)
    }
}

#[async_trait]
impl ApprovalGate for CliGate {
    async fn decide(&self, request: ApprovalRequest) -> Decision {
        let (reply, answer) = oneshot::channel();
        if self.asks.send(Ask { request, reply }).await.is_err() {
            return deny("no one is available to approve it");
        }
        answer
            .await
            .unwrap_or_else(|_| deny("the approval prompt was closed"))
    }
}

fn deny(reason: &str) -> Decision {
    Decision::Deny {
        reason: reason.to_owned(),
    }
}

/// What the `u` key does, spelled out on every card that offers it.
const UNSANDBOXED_KEY: &str =
    "[u] run UNSANDBOXED, this command only (no sandbox: full access to your files and network)";

/// The approval card: what will run, how it is confined and the available keys.
pub fn render_card(req: &ApprovalRequest, color: bool) -> String {
    let (bold, reset) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let mut out = if req.blocked_retry {
        format!(
            "╭─ {bold}the sandbox blocked {}: rerun it unsandboxed?{reset} · risk: {}\n",
            req.tool, req.risk
        )
    } else {
        format!(
            "╭─ {bold}approve {}?{reset} · risk: {}\n",
            req.tool, req.risk
        )
    };
    for line in req.preview.lines() {
        out.push_str(&format!("│ {line}\n"));
    }
    if req.blocked_retry {
        out.push_str(
            "│ It was stopped from writing outside the workspace (or from using the network).\n",
        );
    } else {
        out.push_str(&format!(
            "│ sandbox: {} · network: {}\n",
            req.sandbox,
            if req.network { "on" } else { "off" }
        ));
        for path in &req.extra_writes {
            out.push_str(&format!(
                "│ also writable for this command: {}\n",
                path.display()
            ));
        }
        if req.outside_unknown && req.can_run_unsandboxed() {
            out.push_str(
                "│ note: it may write outside the workspace where rozu can't tell; the sandbox \
                 will block that (use [u] if it must)\n",
            );
        }
    }
    out.push_str("╰─ ");
    let keys = Keys::for_request(req);
    let mut parts = Vec::new();
    if keys.approve {
        parts.push("[y] approve".to_owned());
    }
    parts.push(if req.blocked_retry {
        "[n] keep the failure".to_owned()
    } else {
        "[n] deny (optionally `n <reason>`)".to_owned()
    });
    if let Some(rule) = req.allow_rule.as_ref().filter(|_| keys.always_allow) {
        parts.push(format!("[a] always allow `{rule}` in this project"));
    }
    if keys.unsandboxed {
        parts.push(UNSANDBOXED_KEY.to_owned());
    }
    out.push_str(&parts.join("  "));
    out
}

/// The keys a card accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Keys {
    pub approve: bool,
    pub always_allow: bool,
    pub unsandboxed: bool,
}

impl Keys {
    pub fn for_request(req: &ApprovalRequest) -> Self {
        Self {
            // After a sandbox block, `y` would just repeat the blocked run.
            approve: !req.blocked_retry,
            always_allow: req.allow_rule.is_some() && !req.blocked_retry,
            unsandboxed: req.can_run_unsandboxed(),
        }
    }

    fn describe(self) -> String {
        let mut keys = Vec::new();
        if self.approve {
            keys.push("y");
        }
        if self.always_allow {
            keys.push("a");
        }
        if self.unsandboxed {
            keys.push("u");
        }
        keys.push("n");
        match keys.as_slice() {
            [only] => (*only).to_owned(),
            [rest @ .., last] => format!("{} or {last}", rest.join(", ")),
            [] => String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Approve,
    /// Deny; `None` asks for a reason next.
    Deny(Option<String>),
    AlwaysAllow,
    Unsandboxed,
}

/// Parses a typed answer; `None` if it isn't one of the offered keys.
pub fn parse_answer(line: &str, keys: Keys) -> Option<Answer> {
    let line = line.trim();
    let (key, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(k, r)| (k, r.trim()));
    match key.to_ascii_lowercase().as_str() {
        "y" | "yes" if rest.is_empty() && keys.approve => Some(Answer::Approve),
        "n" | "no" => Some(Answer::Deny((!rest.is_empty()).then(|| rest.to_owned()))),
        "a" | "always" if rest.is_empty() && keys.always_allow => Some(Answer::AlwaysAllow),
        "u" | "unsandboxed" if rest.is_empty() && keys.unsandboxed => Some(Answer::Unsandboxed),
        _ => None,
    }
}

const DECLINED: &str = "the user declined";

/// Shows the card for `ask` and sends back the user's decision. Returns early (without an
/// answer) if the agent stops waiting, e.g. because the turn was cancelled.
pub async fn answer(ask: Ask, input: &Input, interactive: bool, color: bool) {
    let Ask { request, mut reply } = ask;
    if reply.is_closed() {
        // Withdrawn before it was shown (e.g. the claude CLI was cancelled meanwhile).
        return;
    }
    eprintln!("{}", render_card(&request, color));
    if !interactive {
        // The decision line that follows says why; no extra line here.
        let _ = reply.send(deny(
            "no terminal to ask on: approval is required but rozu is running \
             non-interactively; the user can rerun in a terminal or with a broader --approval \
             mode",
        ));
        return;
    }
    let keys = Keys::for_request(&request);
    let decision = loop {
        prompt("> ");
        let line = tokio::select! {
            line = input.line() => line,
            _ = reply.closed() => {
                eprintln!("  (approval cancelled)");
                return;
            }
        };
        let Some(line) = line else {
            break deny("no answer (end of input)");
        };
        match parse_answer(&line, keys) {
            Some(Answer::Approve) => break Decision::Approve,
            Some(Answer::AlwaysAllow) => break Decision::AlwaysAllowPrefix,
            Some(Answer::Unsandboxed) => break Decision::ApproveUnsandboxed,
            Some(Answer::Deny(Some(reason))) => break Decision::Deny { reason },
            Some(Answer::Deny(None)) => {
                prompt("reason for the agent (optional): ");
                let reason = tokio::select! {
                    line = input.line() => line.unwrap_or_default(),
                    _ = reply.closed() => return,
                };
                let reason = reason.trim();
                break deny(if reason.is_empty() { DECLINED } else { reason });
            }
            None => eprintln!("  please answer {}", keys.describe()),
        }
    };
    let _ = reply.send(decision);
}

fn prompt(text: &str) {
    eprint!("{text}");
    let _ = io::stderr().flush();
}

/// Whether approval prompts can be answered: stdin must be a terminal.
pub fn stdin_is_interactive() -> bool {
    io::stdin().is_terminal()
}

#[cfg(test)]
mod tests {
    use rozu_core::SandboxMode;
    use rozu_tools::{AllowRule, Risk};
    use serde_json::json;

    use super::*;

    fn request(rule: Option<AllowRule>) -> ApprovalRequest {
        ApprovalRequest {
            call_id: "c1".into(),
            tool: "shell".into(),
            args: json!({"command": "curl https://example.com"}),
            risk: Risk::Network,
            preview: "$ curl https://example.com".into(),
            sandbox: SandboxMode::Enforced("seatbelt"),
            network: true,
            extra_writes: Vec::new(),
            outside_unknown: false,
            blocked_retry: false,
            allow_rule: rule,
        }
    }

    fn curl_rule() -> Option<AllowRule> {
        Some(AllowRule::with_host("curl", "example.com"))
    }

    const ALL_KEYS: Keys = Keys {
        approve: true,
        always_allow: true,
        unsandboxed: true,
    };

    #[test]
    fn card_shows_command_sandbox_network_and_keys() {
        assert_eq!(
            render_card(&request(curl_rule()), false),
            format!(
                "╭─ approve shell? · risk: network\n\
                 │ $ curl https://example.com\n\
                 │ sandbox: seatbelt (writes: workspace + session tmp) · network: on\n\
                 ╰─ [y] approve  [n] deny (optionally `n <reason>`)  [a] always allow `curl (host example.com)` in this project  {UNSANDBOXED_KEY}"
            )
        );
        let card = render_card(&request(None), false);
        assert!(!card.contains("[a]"), "{card}");
        // No sandbox here, so nothing to lift.
        let mut off = request(None);
        off.sandbox = SandboxMode::Off;
        assert!(!render_card(&off, false).contains("[u]"));
    }

    #[test]
    fn card_lists_granted_paths_and_unknown_writes() {
        let mut req = request(None);
        req.extra_writes = vec!["/Users/me/scratch/a.txt".into()];
        req.outside_unknown = true;
        let card = render_card(&req, false);
        assert!(
            card.contains("│ also writable for this command: /Users/me/scratch/a.txt\n"),
            "{card}"
        );
        assert!(card.contains("the sandbox will block that"), "{card}");
    }

    #[test]
    fn blocked_retry_card_offers_only_u_and_n() {
        let mut req = request(curl_rule());
        req.blocked_retry = true;
        let card = render_card(&req, false);
        assert!(
            card.starts_with("╭─ the sandbox blocked shell: rerun it unsandboxed?"),
            "{card}"
        );
        assert!(
            card.contains("[n] keep the failure") && card.contains("[u]"),
            "{card}"
        );
        assert!(!card.contains("[y]") && !card.contains("[a]"), "{card}");
        let keys = Keys::for_request(&req);
        assert_eq!(parse_answer("y", keys), None);
        assert_eq!(parse_answer("u", keys), Some(Answer::Unsandboxed));
        assert_eq!(keys.describe(), "u or n");
    }

    #[test]
    fn parses_answers() {
        let no_extras = Keys {
            approve: true,
            always_allow: false,
            unsandboxed: false,
        };
        assert_eq!(parse_answer("y", ALL_KEYS), Some(Answer::Approve));
        assert_eq!(parse_answer(" YES \n", no_extras), Some(Answer::Approve));
        assert_eq!(parse_answer("n", ALL_KEYS), Some(Answer::Deny(None)));
        assert_eq!(
            parse_answer("n  use rg instead", ALL_KEYS),
            Some(Answer::Deny(Some("use rg instead".into())))
        );
        assert_eq!(parse_answer("a", ALL_KEYS), Some(Answer::AlwaysAllow));
        assert_eq!(parse_answer("a", no_extras), None);
        assert_eq!(parse_answer("u", ALL_KEYS), Some(Answer::Unsandboxed));
        assert_eq!(parse_answer("u", no_extras), None);
        assert_eq!(parse_answer("u now", ALL_KEYS), None);
        assert_eq!(parse_answer("", ALL_KEYS), None);
        assert_eq!(parse_answer("maybe", ALL_KEYS), None);
        assert_eq!(parse_answer("y please", ALL_KEYS), None);
        assert_eq!(ALL_KEYS.describe(), "y, a, u or n");
        assert_eq!(no_extras.describe(), "y or n");
    }

    #[test]
    fn lines_are_split_and_carriage_returns_dropped() {
        let mut buf = b"answer\r".to_vec();
        assert_eq!(finish_line(&mut buf), "answer");
        assert!(buf.is_empty());
    }

    #[tokio::test]
    async fn non_interactive_prompts_are_denied() {
        let (gate, mut asks) = CliGate::new();
        let input = Input {
            lines: Arc::new(Mutex::new(mpsc::unbounded_channel().1)),
        };
        let answering = tokio::spawn(async move {
            let ask = asks.recv().await.unwrap();
            answer(ask, &input, false, false).await;
        });
        let decision = gate.decide(request(curl_rule())).await;
        answering.await.unwrap();
        assert!(
            matches!(&decision, Decision::Deny { reason } if reason.contains("non-interactively")),
            "{decision:?}"
        );
    }

    #[tokio::test]
    async fn interactive_answers_become_decisions() {
        for (typed, expected) in [
            (vec!["y"], Decision::Approve),
            (vec!["huh", "a"], Decision::AlwaysAllowPrefix),
            (vec!["u"], Decision::ApproveUnsandboxed),
            (
                vec!["n", "too risky"],
                Decision::Deny {
                    reason: "too risky".into(),
                },
            ),
            (
                vec!["n", ""],
                Decision::Deny {
                    reason: DECLINED.into(),
                },
            ),
        ] {
            let (tx, rx) = mpsc::unbounded_channel();
            for line in typed {
                tx.send(line.to_owned()).unwrap();
            }
            let input = Input {
                lines: Arc::new(Mutex::new(rx)),
            };
            let (gate, mut asks) = CliGate::new();
            let answering = tokio::spawn(async move {
                let ask = asks.recv().await.unwrap();
                answer(ask, &input, true, false).await;
            });
            assert_eq!(gate.decide(request(curl_rule())).await, expected);
            answering.await.unwrap();
        }
    }
}
