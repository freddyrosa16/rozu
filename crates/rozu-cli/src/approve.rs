//! Interactive approvals for `rozu chat`.
//!
//! The agent asks [`CliGate`], which forwards the request to the turn's render loop. The loop
//! prints the approval card in order with the other output and reads the answer from the shared
//! stdin [`Input`]: `y` approves, `n` denies (with an optional reason sent back to the model),
//! `a` approves and always allows the command prefix in this project. Without a terminal on
//! stdin every prompt is denied.

use std::io::{self, BufRead, IsTerminal, Write};
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
    /// Everything runs unsandboxed without asking, except destructive commands.
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
    pub fn spawn() -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        std::thread::spawn(move || {
            let stdin = io::stdin();
            for line in stdin.lock().lines() {
                let Ok(line) = line else { break };
                if tx.send(line).is_err() {
                    break;
                }
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

/// The approval card: what will run, how it is confined and the available keys.
pub fn render_card(req: &ApprovalRequest, color: bool) -> String {
    let (bold, reset) = if color {
        ("\x1b[1m", "\x1b[0m")
    } else {
        ("", "")
    };
    let mut out = format!(
        "╭─ {bold}approve {}?{reset} · risk: {}\n",
        req.tool, req.risk
    );
    for line in req.preview.lines() {
        out.push_str(&format!("│ {line}\n"));
    }
    out.push_str(&format!(
        "│ sandbox: {} · network: {}\n",
        req.sandbox,
        if req.network { "on" } else { "off" }
    ));
    out.push_str("╰─ [y] approve  [n] deny (optionally `n <reason>`)");
    if let Some(prefix) = &req.allow_prefix {
        out.push_str(&format!("  [a] always allow `{prefix}` in this project"));
    }
    out
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Approve,
    /// Deny; `None` asks for a reason next.
    Deny(Option<String>),
    AlwaysAllow,
}

/// Parses a typed answer; `None` if it isn't one of the offered keys.
pub fn parse_answer(line: &str, can_always_allow: bool) -> Option<Answer> {
    let line = line.trim();
    let (key, rest) = line
        .split_once(char::is_whitespace)
        .map_or((line, ""), |(k, r)| (k, r.trim()));
    match key.to_ascii_lowercase().as_str() {
        "y" | "yes" if rest.is_empty() => Some(Answer::Approve),
        "n" | "no" => Some(Answer::Deny((!rest.is_empty()).then(|| rest.to_owned()))),
        "a" | "always" if rest.is_empty() && can_always_allow => Some(Answer::AlwaysAllow),
        _ => None,
    }
}

const DECLINED: &str = "the user declined";

/// Shows the card for `ask` and sends back the user's decision. Returns early (without an
/// answer) if the agent stops waiting, e.g. because the turn was cancelled.
pub async fn answer(ask: Ask, input: &Input, interactive: bool, color: bool) {
    let Ask { request, mut reply } = ask;
    eprintln!("{}", render_card(&request, color));
    if !interactive {
        eprintln!("  ✗ denied: no terminal to ask on (stdin is not a TTY)");
        let _ = reply.send(deny(
            "approval is required but rozu is running non-interactively; the user can rerun \
             in a terminal or with a broader --approval mode",
        ));
        return;
    }
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
        match parse_answer(&line, request.allow_prefix.is_some()) {
            Some(Answer::Approve) => break Decision::Approve,
            Some(Answer::AlwaysAllow) => break Decision::AlwaysAllowPrefix,
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
            None => {
                let keys = if request.allow_prefix.is_some() {
                    "y, n or a"
                } else {
                    "y or n"
                };
                eprintln!("  please answer {keys}");
            }
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
    use rozu_tools::Risk;
    use serde_json::json;

    use super::*;

    fn request(prefix: Option<&str>) -> ApprovalRequest {
        ApprovalRequest {
            call_id: "c1".into(),
            tool: "shell".into(),
            args: json!({"command": "curl https://example.com"}),
            risk: Risk::Network,
            preview: "$ curl https://example.com".into(),
            sandbox: SandboxMode::Enforced("seatbelt"),
            network: true,
            allow_prefix: prefix.map(Into::into),
        }
    }

    #[test]
    fn card_shows_command_sandbox_network_and_keys() {
        assert_eq!(
            render_card(&request(Some("curl")), false),
            "╭─ approve shell? · risk: network\n\
             │ $ curl https://example.com\n\
             │ sandbox: seatbelt (writes: workspace + tmp) · network: on\n\
             ╰─ [y] approve  [n] deny (optionally `n <reason>`)  [a] always allow `curl` in this project"
        );
        let card = render_card(&request(None), false);
        assert!(!card.contains("[a]"), "{card}");
    }

    #[test]
    fn parses_answers() {
        assert_eq!(parse_answer("y", true), Some(Answer::Approve));
        assert_eq!(parse_answer(" YES \n", false), Some(Answer::Approve));
        assert_eq!(parse_answer("n", true), Some(Answer::Deny(None)));
        assert_eq!(
            parse_answer("n  use rg instead", true),
            Some(Answer::Deny(Some("use rg instead".into())))
        );
        assert_eq!(parse_answer("a", true), Some(Answer::AlwaysAllow));
        assert_eq!(parse_answer("a", false), None);
        assert_eq!(parse_answer("", true), None);
        assert_eq!(parse_answer("maybe", true), None);
        assert_eq!(parse_answer("y please", true), None);
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
        let decision = gate.decide(request(Some("curl"))).await;
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
            assert_eq!(gate.decide(request(Some("curl"))).await, expected);
            answering.await.unwrap();
        }
    }
}
