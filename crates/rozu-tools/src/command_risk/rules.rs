//! Persistent prefix rules for classified shell commands.

use super::{
    PathScope, SHELLS, classify_command, is_interpreter, program_name, split_segments, words,
};
use serde::{Deserialize, Serialize};
use std::fmt;

// ---------------------------------------------------------------------------------------------
// "Always allow" prefixes

/// Tools whose second word is a subcommand worth keeping in a suggested prefix.
const SUBCOMMAND_TOOLS: &[&str] = &[
    "git", "cargo", "npm", "pnpm", "yarn", "bun", "pip", "pip3", "uv", "go", "docker", "podman",
    "kubectl", "brew", "make", "just", "poetry", "gh", "swift", "dotnet", "mvn", "gradle",
];

/// An "always allow" rule: commands whose every segment starts with `prefix` (word by word).
/// Legacy host fields still deserialize, but host-scoped rules no longer grant permissions.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AllowRule {
    pub prefix: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

impl AllowRule {
    pub fn prefix(prefix: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            host: None,
        }
    }

    pub fn with_host(prefix: impl Into<String>, host: impl Into<String>) -> Self {
        Self {
            prefix: prefix.into(),
            host: Some(host.into()),
        }
    }
}

impl fmt::Display for AllowRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.host {
            Some(host) => write!(f, "{} (host {host})", self.prefix),
            None => f.write_str(&self.prefix),
        }
    }
}

/// The rule "always allow" would store for `command`: the program, plus its subcommand for
/// tools like `cargo`, or its script for interpreters. Network permissions cannot be remembered.
/// `None` when no rule may cover it: compound commands (one prefix can't describe everything
/// that would run), wrappers and assignments, inline interpreter code or output redirects, and
/// network operations.
pub fn suggest_rule(command: &str) -> Option<AllowRule> {
    if rule_scope_check(command) {
        return None;
    }
    let segments = split_segments(command);
    let [segment] = segments.as_slice() else {
        return None;
    };
    let words = words(segment);
    let first = words.first()?;
    let name = program_name(first);
    let args = &words[1..];
    if is_interpreter(name) || SHELLS.contains(&name) {
        // `runs_named_script` holds (inline code was refused above).
        let script = match args.first().map(String::as_str) {
            Some("-m") => format!("-m {}", args.get(1)?),
            Some(file) => file.to_owned(),
            None => return None,
        };
        return Some(AllowRule::prefix(format!("{first} {script}")));
    }
    match args.first() {
        Some(sub)
            if SUBCOMMAND_TOOLS.contains(&name)
                && sub
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && !sub.starts_with('-') =>
        {
            Some(AllowRule::prefix(format!("{first} {sub}")))
        }
        _ => Some(AllowRule::prefix(first.clone())),
    }
}

/// Reject syntax we cannot safely authorize and network grants, which are not host-confined
/// by the OS sandbox. Check the complete command so escaped newlines cannot split a rule.
fn rule_scope_check(command: &str) -> bool {
    let risk = classify_command(command, &PathScope::new("/", Vec::new()));
    risk.unrulable || risk.network || risk.risk == crate::Risk::Destructive
}

/// Every segment must match a stored prefix, with no opaque syntax or network operations.
/// Old host rules are deliberately inert, including ones that used to match curl or wget.
pub fn rules_allow(command: &str, rules: &[AllowRule]) -> bool {
    if rule_scope_check(command) {
        return false;
    }
    let segments = split_segments(command);
    !segments.is_empty()
        && segments.iter().all(|segment| {
            let words = words(segment);
            rules.iter().filter(|rule| rule.host.is_none()).any(|rule| {
                let prefix: Vec<&str> = rule.prefix.split_whitespace().collect();
                !prefix.is_empty()
                    && words.len() >= prefix.len()
                    && words
                        .iter()
                        .zip(prefix)
                        .all(|(word, expected)| word == expected)
            })
        })
}
