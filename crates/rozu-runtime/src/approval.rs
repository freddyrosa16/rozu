//! Approval choices and sandbox labels shared by the terminal and desktop frontends.

use rozu_core::{ApprovalRequest, CODEX_SANDBOX, SandboxMode};
use rozu_tools::Risk;

/// Keep approval text readable without terminal or bidirectional control effects.
pub fn approval_text(text: &str) -> String {
    let mut shown = String::with_capacity(text.len());
    for c in text.chars() {
        if c != '\n'
            && (c.is_control()
                || matches!(c, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'))
        {
            shown.extend(c.escape_default());
        } else {
            shown.push(c);
        }
    }
    shown
}

/// The approval actions a frontend may offer, in addition to denying the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalOptions {
    pub approve: bool,
    pub always_allow: bool,
    pub unsandboxed: bool,
}

impl ApprovalOptions {
    pub fn for_request(request: &ApprovalRequest) -> Self {
        Self {
            approve: !request.blocked_retry,
            always_allow: !request.blocked_retry
                && !matches!(
                    request.risk,
                    Risk::Opaque | Risk::Destructive | Risk::Network
                )
                && (!request.network || request.sandbox == SandboxMode::Off)
                && request.allow_rule.is_some(),
            unsandboxed: request.can_run_unsandboxed(),
        }
    }
}

/// A short label for the request's exceptional one-time approval action.
pub fn unsandboxed_action_label(request: &ApprovalRequest) -> &'static str {
    if request.sandbox == SandboxMode::Enforced(CODEX_SANDBOX) {
        "Approve this command once"
    } else {
        "Run without the sandbox"
    }
}

/// The effect of an exceptional one-time approval, including a vendor-owned sandbox.
pub fn unsandboxed_approval_label(request: &ApprovalRequest) -> &'static str {
    if request.sandbox == SandboxMode::Enforced(CODEX_SANDBOX) {
        "Approved once in Codex's sandbox"
    } else {
        "Allowed without the sandbox"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rozu_tools::AllowRule;

    #[test]
    fn controls_are_visible_without_losing_unicode_or_lines() {
        let text = "rm file #\u{1b}[2J\r\u{8}\u{9d}title\u{7}\u{202e}safe\u{200f}\nrosé";
        let shown = approval_text(text);
        assert_eq!(
            shown,
            "rm file #\\u{1b}[2J\\r\\u{8}\\u{9d}title\\u{7}\\u{202e}safe\\u{200f}\nrosé"
        );
        assert!(!shown.chars().any(|c| c.is_control() && c != '\n'));
    }

    fn request(risk: Risk, sandbox: SandboxMode, blocked_retry: bool) -> ApprovalRequest {
        ApprovalRequest {
            call_id: "call".into(),
            tool: "shell".into(),
            args: Default::default(),
            risk,
            preview: "command".into(),
            sandbox,
            network: false,
            extra_writes: Vec::new(),
            outside_unknown: false,
            blocked_retry,
            allow_rule: Some(AllowRule::prefix("command")),
        }
    }

    #[test]
    fn frontend_choices_respect_risk_sandbox_and_retry_boundaries() {
        let sandboxed = SandboxMode::Enforced("seatbelt");
        for (risk, sandbox, retry, expected) in [
            (
                Risk::Read,
                SandboxMode::InProcess,
                false,
                (true, true, false),
            ),
            (Risk::Read, SandboxMode::Off, false, (true, true, false)),
            (Risk::Network, sandboxed, false, (true, false, true)),
            (Risk::Opaque, sandboxed, false, (true, false, true)),
            (Risk::Destructive, sandboxed, false, (true, false, true)),
            (Risk::Network, sandboxed, true, (false, false, true)),
            (
                Risk::Network,
                SandboxMode::Enforced(CODEX_SANDBOX),
                false,
                (true, false, true),
            ),
        ] {
            let options = ApprovalOptions::for_request(&request(risk, sandbox, retry));
            assert_eq!(
                (options.approve, options.always_allow, options.unsandboxed),
                expected,
                "{risk:?}, {sandbox:?}, retry={retry}"
            );
        }
        let mut no_rule = request(Risk::Network, sandboxed, false);
        no_rule.allow_rule = None;
        assert!(!ApprovalOptions::for_request(&no_rule).always_allow);
    }

    #[test]
    fn codex_labels_do_not_claim_to_lift_its_sandbox() {
        let codex = request(Risk::Network, SandboxMode::Enforced(CODEX_SANDBOX), false);
        assert_eq!(
            unsandboxed_action_label(&codex),
            "Approve this command once"
        );
        assert_eq!(
            unsandboxed_approval_label(&codex),
            "Approved once in Codex's sandbox"
        );

        let shell = request(Risk::Network, SandboxMode::Enforced("seatbelt"), true);
        assert_eq!(unsandboxed_action_label(&shell), "Run without the sandbox");
        assert_eq!(
            unsandboxed_approval_label(&shell),
            "Allowed without the sandbox"
        );
    }
}
