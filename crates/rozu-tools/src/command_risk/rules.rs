//! Persistent prefix and host rules for classified shell commands.

use super::{
    PathScope, SHELLS, classify_segment, is_assignment, is_interpreter, is_remote, program_name,
    split_segments, words,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;

// ---------------------------------------------------------------------------------------------
// "Always allow" prefixes

/// Tools whose second word is a subcommand worth keeping in a suggested prefix.
const SUBCOMMAND_TOOLS: &[&str] = &[
    "git", "cargo", "npm", "pnpm", "yarn", "bun", "pip", "pip3", "uv", "go", "docker", "podman",
    "kubectl", "brew", "make", "just", "poetry", "gh", "swift", "dotnet", "mvn", "gradle",
];

/// Network clients whose "always allow" rules are scoped to one host (never a blanket `curl`).
const HOST_SCOPED: &[&str] = &[
    "curl",
    "wget",
    "http",
    "https",
    "aria2c",
    "ssh",
    "scp",
    "sftp",
    "ftp",
    "telnet",
    "nc",
    "ncat",
    "netcat",
    "ping",
    "dig",
    "nslookup",
    "host",
    "traceroute",
];

/// Clients that name the host in `user@host` / `host:path` form.
const SSH_LIKE: &[&str] = &["ssh", "scp", "sftp"];
/// Clients that only reach URLs (a bare word is a file or an option value).
const URL_CLIENTS: &[&str] = &["curl", "wget", "http", "https", "aria2c"];

/// An "always allow" rule: commands whose every segment starts with `prefix` (word by word).
/// For network clients like `curl` the rule also names the one `host` it may talk to.
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

/// The hosts a network client segment would reach, as far as its arguments say (lowercase).
fn segment_hosts(name: &str, args: &[String]) -> BTreeSet<String> {
    let mut hosts = BTreeSet::new();
    let mut push = |host: &str| {
        let host = host.trim_matches(['[', ']']).to_ascii_lowercase();
        if !host.is_empty() {
            hosts.insert(host);
        }
    };
    for arg in args {
        if let Some(pos) = arg.find("://") {
            let authority = arg[pos + 3..]
                .split(['/', '?', '#'])
                .next()
                .unwrap_or_default();
            let authority = authority.rsplit('@').next().unwrap_or_default();
            let host = if authority.starts_with('[') {
                authority.split(']').next().unwrap_or_default()
            } else {
                authority.split(':').next().unwrap_or_default()
            };
            push(host);
        } else if arg.starts_with(['-', '+', '@']) || URL_CLIENTS.contains(&name) {
            // Options, `dig +short`/`@server`; URL clients only count URLs.
        } else if SSH_LIKE.contains(&name) {
            if let Some((_, rest)) = arg.split_once('@') {
                push(rest.split(':').next().unwrap_or_default());
            } else if is_remote(arg) {
                push(arg.split(':').next().unwrap_or_default());
            }
        } else if arg == "localhost"
            || (arg.contains('.')
                && arg
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-'))
        {
            push(arg);
        }
    }
    hosts
}

/// The rule "always allow" would store for `command`: the program, plus its subcommand for
/// tools like `git` or `cargo`, its script for interpreters, and its host for network clients.
/// `None` when no rule may cover it: compound commands (one prefix can't describe everything
/// that would run), wrappers and assignments, inline interpreter code or output redirects, and
/// network clients without exactly one host.
pub fn suggest_rule(command: &str) -> Option<AllowRule> {
    let segments = split_segments(command);
    let [segment] = segments.as_slice() else {
        return None;
    };
    if rule_scope_check(segment) {
        return None;
    }
    let words = words(segment);
    let first = words.first()?;
    let name = program_name(first);
    if is_assignment(first)
        || matches!(
            name,
            "env"
                | "nice"
                | "timeout"
                | "stdbuf"
                | "nohup"
                | "command"
                | "builtin"
                | "exec"
                | "time"
                | "xargs"
                | "sudo"
                | "doas"
        )
    {
        return None;
    }
    let args = &words[1..];
    if HOST_SCOPED.contains(&name) {
        let hosts = segment_hosts(name, args);
        let [host] = hosts.into_iter().collect::<Vec<_>>().try_into().ok()?;
        return Some(AllowRule::with_host(first.clone(), host));
    }
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

/// Whether `segment` has something no rule may cover (inline code, a redirect to a file). Paths
/// don't matter here, so it's classified against an empty scope.
fn rule_scope_check(segment: &str) -> bool {
    classify_segment(segment, &PathScope::new("/", Vec::new()), 0).unrulable
}

/// Whether `rules` cover every segment of `command`: the segment starts with the rule's prefix
/// (word by word), a network client reaches only the rule's host, and nothing in it is inline
/// code or an output redirect to a file.
pub fn rules_allow(command: &str, rules: &[AllowRule]) -> bool {
    let segments = split_segments(command);
    !segments.is_empty()
        && segments.iter().all(|segment| {
            if rule_scope_check(segment) {
                return false;
            }
            let words = words(segment);
            let name = words.first().map(|w| program_name(w)).unwrap_or_default();
            let scoped = HOST_SCOPED.contains(&name);
            rules.iter().any(|rule| {
                let prefix_words: Vec<&str> = rule.prefix.split_whitespace().collect();
                let prefix_matches = !prefix_words.is_empty()
                    && words.len() >= prefix_words.len()
                    && words.iter().zip(&prefix_words).all(|(w, p)| w == p);
                let host_matches = match (&rule.host, scoped) {
                    (Some(host), true) => {
                        let hosts = segment_hosts(name, &words[1..]);
                        hosts.len() == 1 && hosts.contains(host)
                    }
                    (None, false) => true,
                    _ => false,
                };
                prefix_matches && host_matches
            })
        })
}
