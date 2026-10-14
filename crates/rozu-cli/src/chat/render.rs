//! Terminal presentation of chat state and streamed events.

use std::io::{self, IsTerminal, Write};

use rozu_core::{
    AgentEvent, ApprovalRecord, Decision, DecisionSource, Session, TurnOutcome, TurnUsage,
};
use rozu_providers::ModelChoice;
use rozu_store::{ProjectMemory, SessionWriter};
use rozu_tools::{Risk, platform_sandbox};

pub(super) fn session_label(
    writer: Option<&SessionWriter>,
    session: &Session,
    memory: &ProjectMemory,
) -> String {
    let saved = match writer {
        None => "not saved".to_owned(),
        Some(w) => {
            let turns = session
                .messages
                .iter()
                .filter(|m| m.role == rozu_core::Role::User)
                .count();
            if turns == 0 {
                format!("session {}", w.id())
            } else {
                format!("resumed {} ({turns} earlier turns)", w.id())
            }
        }
    };
    let names: Vec<_> = memory.sources.iter().map(|(l, _)| l.as_str()).collect();
    if names.is_empty() {
        saved
    } else {
        format!("{saved} · memory: {}", names.join(", "))
    }
}

/// How shell commands are confined on this machine (the same in every approval mode).
pub(super) fn sandbox_label() -> String {
    let sandbox = platform_sandbox();
    if sandbox.enforced() {
        format!(
            "{} in every mode (writes: workspace + session tmp; network only when approved)",
            sandbox.name()
        )
    } else {
        "unavailable on this OS (workspace writes by shell ask first)".to_owned()
    }
}

pub(super) fn compacted_line(done: &rozu_core::Compaction) -> String {
    let mut what = Vec::new();
    if done.masked > 0 {
        what.push(format!("hid {} old tool outputs", done.masked));
    }
    if done.summarized > 0 {
        what.push(format!("summarized {} older messages", done.summarized));
    }
    format!(
        "[compacted: {} · ~{} → ~{} tokens; pinned memory kept]",
        what.join(", "),
        done.tokens_before,
        done.tokens_after
    )
}

/// Output styling shared by all turns.
pub(super) struct Printer {
    pub(super) dim: bool,
}

impl Printer {
    pub(super) fn new() -> Self {
        let dim = io::stderr().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        Self { dim }
    }

    pub(super) fn unpriced_note(&self, choice: &ModelChoice) {
        match &choice.info {
            None => self.status(&format!(
                "note: {} isn't in `rozu models`, so cost won't be shown",
                choice.spec()
            )),
            Some(info) if info.caps.memory_features != rozu_core::MemoryFeatures::Full => {
                self.status(&format!(
                    "note: rozu's memory features are limited with {} (no per-turn control of \
                     the prompt prefix; cache writes aren't reported)",
                    choice.provider.id
                ));
            }
            Some(_) => {}
        }
    }

    /// `[12 in (8 cached, 2 cache-written) / 4 out tokens · ~$0.0001]` after a turn that used
    /// tokens.
    pub(super) fn usage_line(&self, turn: &TurnUsage) {
        let usage = &turn.usage;
        if usage.input_tokens == 0 && usage.output_tokens == 0 {
            return;
        }
        let mut line = format!("[{} in", usage.input_tokens);
        let mut cache = Vec::new();
        if usage.cache_read_tokens > 0 {
            cache.push(format!("{} cached", usage.cache_read_tokens));
        }
        if usage.cache_write_tokens > 0 {
            cache.push(format!("{} cache-written", usage.cache_write_tokens));
        }
        if !cache.is_empty() {
            line.push_str(&format!(" ({})", cache.join(", ")));
        }
        line.push_str(&format!(" / {} out tokens", usage.output_tokens));
        if let Some(cost) = turn.cost_usd {
            line.push_str(&format!(" · ~${cost:.4}"));
        }
        line.push(']');
        self.status(&line);
    }

    pub(super) fn status(&self, text: &str) {
        if self.dim {
            eprintln!("\x1b[2m{text}\x1b[0m");
        } else {
            eprintln!("{text}");
        }
    }

    pub(super) fn start_turn(&self) -> TurnPrinter<'_> {
        TurnPrinter {
            printer: self,
            mid_line: false,
            last_denial: None,
        }
    }
}

pub(super) struct TurnPrinter<'a> {
    printer: &'a Printer,
    /// Whether stdout has text not yet ended by a newline.
    mid_line: bool,
    /// The reason of the denial just printed, so the tool result repeating it isn't printed
    /// again.
    last_denial: Option<String>,
}

impl TurnPrinter<'_> {
    pub(super) fn handle(&mut self, event: AgentEvent) {
        match event {
            AgentEvent::TextDelta(text) => {
                let mut stdout = io::stdout().lock();
                let _ = stdout.write_all(text.as_bytes());
                let _ = stdout.flush();
                if !text.is_empty() {
                    self.mid_line = !text.ends_with('\n');
                }
            }
            AgentEvent::ToolCallStarted {
                name, args_json, ..
            } => {
                self.last_denial = None;
                self.status(&format!("→ {name} {}", clip(&args_json, 120)));
            }
            AgentEvent::ToolCallFinished {
                output, is_error, ..
            } => {
                let first = output.lines().next().unwrap_or("");
                let denial = self.last_denial.take();
                if is_error && denial.is_some_and(|reason| repeats_denial(first, &reason)) {
                    return; // the "✗ denied" line already said it
                }
                let summary = if is_error {
                    format!("  ✗ {}", clip(first, 120))
                } else {
                    let lines = output.lines().count();
                    format!("  ✓ {lines} line{}", if lines == 1 { "" } else { "s" })
                };
                self.status(&summary);
            }
            AgentEvent::Approval(record) => {
                if let Some(line) = approval_line(&record) {
                    self.status(&line);
                }
                self.last_denial = match (&record.decision, &record.source, record.blocked_retry) {
                    (Decision::Deny { reason }, source, false)
                        if *source != DecisionSource::Cancelled =>
                    {
                        Some(reason.clone())
                    }
                    _ => None,
                };
            }
            // The card and the decision line already show the prompt and its answer.
            AgentEvent::ApprovalPending(_) | AgentEvent::ApprovalResolved { .. } => {}
            // Totals come from the session once the turn ends (see `Printer::usage_line`).
            AgentEvent::Usage(_) => {}
            AgentEvent::TurnFinished(outcome) => match outcome {
                TurnOutcome::Completed => {}
                TurnOutcome::Cancelled => self.status("[cancelled]"),
                TurnOutcome::MaxSteps => {
                    self.status("[stopped: reached the step limit for this turn]")
                }
            },
        }
    }

    pub(super) fn finish(&mut self) {
        self.end_line();
    }

    pub(super) fn end_line(&mut self) {
        if self.mid_line {
            println!();
            self.mid_line = false;
        }
    }

    fn status(&mut self, text: &str) {
        self.end_line();
        self.printer.status(text);
    }
}

/// Whether a failed tool result's first line just repeats the denial `reason`.
fn repeats_denial(first_line: &str, reason: &str) -> bool {
    first_line == reason
        || first_line.strip_prefix("denied: ") == Some(reason)
        || first_line.ends_with(reason)
}

/// One status line per notable approval decision; reads approved by the mode are silent.
fn approval_line(record: &ApprovalRecord) -> Option<String> {
    let mut confinement = match record.sandbox {
        rozu_core::SandboxMode::InProcess => String::new(),
        sandbox => format!(
            " · sandbox: {sandbox} · network: {}",
            if record.network { "on" } else { "off" }
        ),
    };
    if !record.extra_writes.is_empty() {
        confinement.push_str(&format!(
            " · also writable: {}",
            record
                .extra_writes
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    if record.blocked_retry {
        return match (&record.decision, &record.source) {
            (_, DecisionSource::Cancelled) => None,
            (Decision::ApproveUnsandboxed, _) => {
                Some("  ⚠ rerunning UNSANDBOXED (this command only)".to_owned())
            }
            _ => Some("  ✗ kept the sandbox's refusal".to_owned()),
        };
    }
    match (&record.decision, &record.source) {
        (_, DecisionSource::Cancelled) => None,
        (Decision::Approve, DecisionSource::Mode(_)) if record.risk == Risk::Read => None,
        // `rozu mcp-serve` decides the call itself and its own entry follows.
        (Decision::Approve, DecisionSource::Delegated) => None,
        // Only the user can choose it (anything else runs sandboxed; see the executor).
        (Decision::ApproveUnsandboxed, _) => {
            Some("  ⚠ approved UNSANDBOXED (this command only)".to_owned())
        }
        (Decision::Approve, DecisionSource::Mode(mode)) => Some(format!(
            "  ✓ auto-approved ({}, {mode} mode){confinement}",
            record.risk
        )),
        (Decision::Approve, DecisionSource::Rule { prefix }) => Some(format!(
            "  ✓ allowed by project rule `{prefix}`{confinement}"
        )),
        (Decision::Approve, _) => Some(format!("  ✓ approved{confinement}")),
        (Decision::AlwaysAllowPrefix, _) => Some(format!(
            "  ✓ approved; always allowing this prefix in {}",
            rozu_tools::PROTECTED_DIR
        )),
        (Decision::Deny { reason }, _) => Some(format!("  ✗ denied: {}", clip(reason, 120))),
    }
}

/// `text` on one line, cut to `max` characters.
fn clip(text: &str, max: usize) -> String {
    let flat: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if flat.chars().count() <= max {
        flat
    } else {
        let cut: String = flat.chars().take(max).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clip_flattens_and_cuts() {
        assert_eq!(clip("a\nb", 10), "a b");
        assert_eq!(clip("abcdef", 3), "abc…");
        assert_eq!(clip("ééé", 3), "ééé");
    }
}
