//! Risk classification for shell command lines.
//!
//! Deliberately simple: the command is split into segments on `&&`, `||`, `;`, `|`, `&`,
//! newlines, parentheses and backticks (ignoring quotes, which can only add segments), each
//! segment is classified on its own, and the highest risk wins. Unknown commands count as
//! [`Risk::WriteWorkspace`]; the sandbox, not this classifier, is what actually confines them.

use std::path::{Path, PathBuf};

use crate::Risk;
use crate::workspace::normalize;

/// The classification of one command line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandRisk {
    pub risk: Risk,
    /// Some segment talks to the network (even if another one is riskier).
    pub network: bool,
}

impl CommandRisk {
    const READ: Self = Self {
        risk: Risk::Read,
        network: false,
    };
    const WRITE: Self = Self {
        risk: Risk::WriteWorkspace,
        network: false,
    };
    const OUTSIDE: Self = Self {
        risk: Risk::WriteOutside,
        network: false,
    };
    const NETWORK: Self = Self {
        risk: Risk::Network,
        network: true,
    };
    const DESTRUCTIVE: Self = Self {
        risk: Risk::Destructive,
        network: false,
    };

    fn max(self, other: Self) -> Self {
        Self {
            risk: self.risk.max(other.risk),
            network: self.network || other.network,
        }
    }
}

/// Where relative paths start and which directories count as "inside".
#[derive(Debug, Clone)]
pub struct PathScope {
    /// Directory relative paths are resolved against.
    pub cwd: PathBuf,
    /// Directories writes may go to without counting as outside (workspace, temp dirs).
    pub inside: Vec<PathBuf>,
}

impl PathScope {
    pub fn new(cwd: impl Into<PathBuf>, inside: Vec<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            inside,
        }
    }

    /// Whether a path argument stays inside. `~` and `$VAR` paths are unknown, so outside.
    fn contains(&self, arg: &str) -> bool {
        if arg.starts_with('~') || arg.contains('$') {
            return false;
        }
        let path = Path::new(arg);
        let joined = if path.is_absolute() {
            path.to_path_buf()
        } else {
            self.cwd.join(path)
        };
        let normalized = normalize(&joined);
        self.inside.iter().any(|root| normalized.starts_with(root))
    }
}

/// Classifies a command line (see the module docs).
pub fn classify_command(command: &str, scope: &PathScope) -> CommandRisk {
    classify_depth(command, scope, 0)
}

/// Nested `sh -c` / `eval` deeper than this is treated as destructive rather than parsed.
const MAX_DEPTH: usize = 4;

fn classify_depth(command: &str, scope: &PathScope, depth: usize) -> CommandRisk {
    if depth > MAX_DEPTH {
        return CommandRisk::DESTRUCTIVE;
    }
    split_segments(command)
        .into_iter()
        .map(|segment| classify_segment(segment, scope, depth))
        .fold(CommandRisk::READ, CommandRisk::max)
}

/// Splits a command line into simple commands. Quotes are ignored, so a quoted separator just
/// produces an extra segment (which can only raise the risk).
pub fn split_segments(command: &str) -> Vec<&str> {
    let bytes = command.as_bytes();
    let mut segments = Vec::new();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        let prev = i.checked_sub(1).map(|p| bytes[p]);
        let next = bytes.get(i + 1).copied();
        let width = match b {
            b'&' | b'|' if next == Some(b) => 2,
            // `2>&1`, `&>file`, `>&2` and `>|file` are redirections, not separators.
            b'&' if matches!(prev, Some(b'>' | b'<')) || next == Some(b'>') => 0,
            b'|' if prev == Some(b'>') => 0,
            b';' | b'&' | b'|' | b'\n' | b'(' | b')' | b'`' => 1,
            _ => 0,
        };
        if width == 0 {
            i += 1;
            continue;
        }
        segments.push(command[start..i].trim());
        i += width;
        start = i;
    }
    segments.push(command[start..].trim());
    segments.retain(|s| !s.is_empty());
    segments
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Token {
    Word(String),
    /// A redirection operator (`>`, `>>`, `<`, `>&`, `&>`, …) and its target word, if any.
    Redirect {
        op: String,
        target: Option<String>,
    },
}

/// Splits one segment into words, honoring quotes and backslashes, and pulls out redirections.
fn tokenize(segment: &str) -> Vec<Token> {
    let mut raw: Vec<(String, bool)> = Vec::new(); // (text, is_operator)
    let mut word = String::new();
    let mut in_word = false;
    let mut quoted = false;
    let mut chars = segment.chars().peekable();
    let flush = |raw: &mut Vec<(String, bool)>, word: &mut String, in_word: &mut bool| {
        if *in_word {
            raw.push((std::mem::take(word), false));
            *in_word = false;
        }
    };
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                flush(&mut raw, &mut word, &mut in_word);
                quoted = false;
            }
            '\'' => {
                in_word = true;
                quoted = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    word.push(c);
                }
            }
            '"' => {
                in_word = true;
                quoted = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(&next) = chars.peek() {
                                if matches!(next, '"' | '\\' | '$' | '`') {
                                    word.push(next);
                                    chars.next();
                                    continue;
                                }
                            }
                            word.push('\\');
                        }
                        c => word.push(c),
                    }
                }
            }
            '\\' => {
                in_word = true;
                if let Some(next) = chars.next() {
                    word.push(next);
                }
            }
            '>' | '<' | '&' if c != '&' || chars.peek() == Some(&'>') => {
                // A bare fd number right before the operator (`2>`) belongs to it.
                if in_word
                    && !quoted
                    && !word.is_empty()
                    && word.chars().all(|d| d.is_ascii_digit())
                {
                    word.clear();
                    in_word = false;
                }
                flush(&mut raw, &mut word, &mut in_word);
                let mut op = c.to_string();
                while let Some(&next) = chars.peek() {
                    if matches!(next, '>' | '<' | '&' | '|') {
                        op.push(next);
                        chars.next();
                    } else {
                        break;
                    }
                }
                raw.push((op, true));
                quoted = false;
            }
            c => {
                in_word = true;
                word.push(c);
            }
        }
    }
    flush(&mut raw, &mut word, &mut in_word);

    let mut tokens = Vec::new();
    let mut iter = raw.into_iter().peekable();
    while let Some((text, is_op)) = iter.next() {
        if is_op {
            let target = match iter.peek() {
                Some((_, false)) => iter.next().map(|(t, _)| t),
                _ => None,
            };
            tokens.push(Token::Redirect { op: text, target });
        } else {
            tokens.push(Token::Word(text));
        }
    }
    tokens
}

/// The plain words of a segment (redirections dropped).
fn words(segment: &str) -> Vec<String> {
    tokenize(segment)
        .into_iter()
        .filter_map(|t| match t {
            Token::Word(w) => Some(w),
            Token::Redirect { .. } => None,
        })
        .collect()
}

fn classify_segment(segment: &str, scope: &PathScope, depth: usize) -> CommandRisk {
    let tokens = tokenize(segment);
    let mut result = CommandRisk::READ;
    let mut words = Vec::new();
    for token in tokens {
        match token {
            Token::Word(w) => words.push(w),
            Token::Redirect { op, target } => {
                result = result.max(classify_redirect(&op, target.as_deref(), scope));
            }
        }
    }
    result.max(classify_words(&words, scope, depth))
}

fn classify_redirect(op: &str, target: Option<&str>, scope: &PathScope) -> CommandRisk {
    if !op.contains('>') {
        return CommandRisk::READ; // input redirection or heredoc
    }
    let Some(target) = target else {
        return CommandRisk::READ; // `>&2` style with the fd glued on, or nothing to write
    };
    if op.ends_with('&') && (target == "-" || target.chars().all(|c| c.is_ascii_digit())) {
        return CommandRisk::READ; // fd duplication
    }
    if matches!(
        target,
        "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty"
    ) {
        return CommandRisk::READ;
    }
    if scope.contains(target) {
        CommandRisk::WRITE
    } else {
        CommandRisk::DESTRUCTIVE
    }
}

/// Program name without its directory (`/bin/rm` → `rm`).
fn program_name(word: &str) -> &str {
    word.rsplit('/').next().unwrap_or(word)
}

fn is_assignment(word: &str) -> bool {
    match word.split_once('=') {
        Some((name, _)) => {
            !name.is_empty()
                && name.chars().enumerate().all(|(i, c)| {
                    c == '_' || c.is_ascii_alphabetic() || (i > 0 && c.is_ascii_digit())
                })
        }
        None => false,
    }
}

/// Short flags in a word like `-rf` (empty for long options and non-flags).
fn short_flags(word: &str) -> &str {
    match word.strip_prefix('-') {
        Some(rest) if !rest.starts_with('-') => rest,
        _ => "",
    }
}

fn has_short_flag(args: &[String], flag: char) -> bool {
    args.iter().any(|a| short_flags(a).contains(flag))
}

fn has_arg(args: &[String], names: &[&str]) -> bool {
    args.iter().any(|a| {
        names
            .iter()
            .any(|n| a == n || (n.starts_with("--") && a.starts_with(&format!("{n}="))))
    })
}

/// Non-flag arguments (after a `--`, everything counts).
fn operands(args: &[String]) -> Vec<&str> {
    let mut out = Vec::new();
    let mut only_operands = false;
    for arg in args {
        if only_operands {
            out.push(arg.as_str());
        } else if arg == "--" {
            only_operands = true;
        } else if !arg.starts_with('-') || arg == "-" {
            out.push(arg.as_str());
        }
    }
    out
}

const READ_COMMANDS: &[&str] = &[
    "ls",
    "cat",
    "head",
    "tail",
    "wc",
    "pwd",
    "echo",
    "printf",
    "which",
    "whereis",
    "type",
    "file",
    "stat",
    "du",
    "df",
    "tree",
    "diff",
    "cmp",
    "sort",
    "uniq",
    "cut",
    "tr",
    "true",
    "false",
    "date",
    "whoami",
    "id",
    "uname",
    "hostname",
    "basename",
    "dirname",
    "realpath",
    "readlink",
    "grep",
    "egrep",
    "fgrep",
    "rg",
    "ag",
    "fd",
    "less",
    "more",
    "nl",
    "od",
    "hexdump",
    "xxd",
    "strings",
    "column",
    "jq",
    "test",
    "[",
    "seq",
    "md5",
    "md5sum",
    "shasum",
    "sha1sum",
    "sha256sum",
    "cksum",
    "printenv",
    "ps",
    "uptime",
    "sleep",
    "tac",
    "rev",
    "fold",
    "fmt",
    "expand",
    "comm",
    "join",
    "paste",
    "cd",
    "export",
    "set",
    "unset",
    "alias",
    "locale",
    "lsof",
];

const NETWORK_COMMANDS: &[&str] = &[
    "curl",
    "wget",
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
    "http",
    "https",
    "aria2c",
    "npx",
    "pnpx",
    "bunx",
    "uvx",
    "gh",
];

const ALWAYS_DESTRUCTIVE: &[&str] = &[
    "sudo", "doas", "su", "pkexec", "dd", "fdisk", "sfdisk", "parted", "diskutil", "shred",
    "wipefs", "shutdown", "reboot", "halt", "poweroff", "mkswap",
];

/// Commands that write to the paths they are given.
const PATH_WRITERS: &[&str] = &["cp", "mv", "touch", "mkdir", "ln", "install", "truncate"];

fn classify_words(words: &[String], scope: &PathScope, depth: usize) -> CommandRisk {
    // Skip `FOO=bar`, shell keywords and transparent wrappers.
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_str();
        if is_assignment(w)
            || matches!(
                w,
                "!" | "{"
                    | "}"
                    | "if"
                    | "then"
                    | "else"
                    | "elif"
                    | "fi"
                    | "do"
                    | "done"
                    | "while"
                    | "until"
                    | "time"
                    | "nohup"
                    | "command"
                    | "builtin"
                    | "exec"
            )
        {
            i += 1;
        } else if w == "env" || w == "nice" || w == "timeout" || w == "stdbuf" {
            i += 1;
            // Their own flags (and a timeout's duration) come before the wrapped command.
            while i < words.len()
                && (words[i].starts_with('-')
                    || is_assignment(&words[i])
                    || (w == "timeout" && words[i].starts_with(|c: char| c.is_ascii_digit())))
            {
                i += 1;
            }
        } else {
            break;
        }
    }
    let Some(first) = words.get(i) else {
        return CommandRisk::READ;
    };
    let args = &words[i + 1..];
    let name = program_name(first);

    if ALWAYS_DESTRUCTIVE.contains(&name) || name.starts_with("mkfs") {
        return CommandRisk::DESTRUCTIVE;
    }
    if NETWORK_COMMANDS.contains(&name) {
        return CommandRisk::NETWORK;
    }
    match name {
        "sh" | "bash" | "zsh" | "dash" | "ksh" | "fish" => {
            match args
                .iter()
                .position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))
            {
                Some(pos) => match args.get(pos + 1) {
                    Some(script) => classify_depth(script, scope, depth + 1),
                    None => CommandRisk::WRITE,
                },
                // Running a script file (or stdin): could do anything the sandbox allows.
                None => CommandRisk::WRITE,
            }
        }
        "eval" => classify_depth(&args.join(" "), scope, depth + 1),
        "xargs" => {
            let mut j = 0;
            while j < args.len() && args[j].starts_with('-') {
                let takes_value = matches!(
                    args[j].as_str(),
                    "-n" | "-I" | "-L" | "-P" | "-d" | "-s" | "-E" | "-a"
                );
                j += if takes_value { 2 } else { 1 };
            }
            if j >= args.len() {
                CommandRisk::WRITE // xargs defaults to echo, but keep it simple
            } else {
                classify_words(&args[j..], scope, depth + 1)
            }
        }
        "find" => classify_find(args, scope, depth),
        "sed" => {
            if args.iter().any(|a| {
                a == "--in-place" || a.starts_with("--in-place=") || short_flags(a).contains('i')
            }) {
                CommandRisk::WRITE
            } else {
                CommandRisk::READ
            }
        }
        "git" => classify_git(args),
        "rm" | "rmdir" | "unlink" => {
            let recursive = has_short_flag(args, 'r')
                || has_short_flag(args, 'R')
                || has_arg(args, &["--recursive"]);
            if recursive || operands(args).iter().any(|p| !scope.contains(p)) {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            }
        }
        "chmod" | "chown" | "chgrp" => {
            if has_short_flag(args, 'R') || has_arg(args, &["--recursive"]) {
                return CommandRisk::DESTRUCTIVE;
            }
            // The first operand is the mode / owner.
            if operands(args).iter().skip(1).any(|p| !scope.contains(p)) {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            }
        }
        "tee" => {
            if operands(args)
                .iter()
                .any(|p| !scope.contains(p) && *p != "/dev/null")
            {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            }
        }
        "kill" | "killall" | "pkill" | "launchctl" | "systemctl" | "crontab" => {
            CommandRisk::OUTSIDE
        }
        "rsync" => {
            let ops = operands(args);
            if ops
                .iter()
                .any(|p| p.contains("::") || p.starts_with("rsync://") || is_remote(p))
            {
                CommandRisk::NETWORK
            } else if ops.last().is_some_and(|dest| !scope.contains(dest)) {
                CommandRisk::OUTSIDE
            } else {
                CommandRisk::WRITE
            }
        }
        name if PATH_WRITERS.contains(&name) => {
            if operands(args).iter().any(|p| !scope.contains(p)) {
                CommandRisk::OUTSIDE
            } else {
                CommandRisk::WRITE
            }
        }
        name if READ_COMMANDS.contains(&name) => CommandRisk::READ,
        "npm" | "pnpm" | "yarn" | "bun" => {
            let sub = operands(args).first().copied();
            match sub {
                None if name == "yarn" => CommandRisk::NETWORK,
                Some(
                    "install" | "i" | "ci" | "add" | "update" | "upgrade" | "up" | "publish"
                    | "dlx" | "create" | "exec" | "x" | "audit" | "outdated" | "view" | "info"
                    | "login" | "fetch",
                ) => CommandRisk::NETWORK,
                _ => CommandRisk::WRITE,
            }
        }
        "pip" | "pip3" => pip_risk(args),
        "python" | "python3" => {
            if args.first().is_some_and(|a| a == "-m")
                && args.get(1).is_some_and(|m| m == "pip" || m == "pip3")
            {
                pip_risk(&args[2..])
            } else {
                CommandRisk::WRITE
            }
        }
        "uv" => match operands(args).as_slice() {
            ["pip", "install" | "download" | "sync", ..]
            | ["add" | "sync" | "lock" | "tool", ..]
            | ["python", "install", ..] => CommandRisk::NETWORK,
            _ => CommandRisk::WRITE,
        },
        "poetry" => match operands(args).first() {
            Some(&("install" | "add" | "update" | "lock" | "publish")) => CommandRisk::NETWORK,
            _ => CommandRisk::WRITE,
        },
        "cargo" => match operands(args).first() {
            Some(
                &("install" | "fetch" | "update" | "publish" | "add" | "search" | "login" | "yank"
                | "owner" | "generate-lockfile"),
            ) => CommandRisk::NETWORK,
            _ => CommandRisk::WRITE,
        },
        "go" => match operands(args).as_slice() {
            ["get" | "install", ..] | ["mod", "download" | "tidy", ..] => CommandRisk::NETWORK,
            _ => CommandRisk::WRITE,
        },
        "gem" | "bundle" | "bundler" => match operands(args).first() {
            Some(&("install" | "update" | "fetch" | "push")) => CommandRisk::NETWORK,
            // A bare `bundle` installs.
            None if name != "gem" => CommandRisk::NETWORK,
            _ => CommandRisk::WRITE,
        },
        "brew" | "apt" | "apt-get" | "yum" | "dnf" | "pacman" | "apk" | "port" => {
            match operands(args).first() {
                Some(
                    &("install" | "update" | "upgrade" | "reinstall" | "tap" | "fetch" | "add"),
                ) => CommandRisk::NETWORK,
                Some(&("uninstall" | "remove" | "purge" | "autoremove" | "del")) => {
                    CommandRisk::DESTRUCTIVE
                }
                _ => CommandRisk::WRITE,
            }
        }
        "docker" | "podman" => match operands(args).first() {
            Some(&("pull" | "push" | "login" | "run" | "build" | "compose" | "search")) => {
                CommandRisk::NETWORK
            }
            Some(&("rm" | "rmi" | "prune" | "system" | "volume")) => CommandRisk::DESTRUCTIVE,
            _ => CommandRisk::WRITE,
        },
        _ => CommandRisk::WRITE,
    }
}

/// `host:path` or `user@host:path` (not a local path with a colon after a slash).
fn is_remote(arg: &str) -> bool {
    match arg.find(':') {
        Some(colon) => !arg[..colon].contains('/'),
        None => false,
    }
}

fn pip_risk(args: &[String]) -> CommandRisk {
    match operands(args).first() {
        Some(&("install" | "download" | "wheel" | "search" | "index")) => CommandRisk::NETWORK,
        Some(&"uninstall") => CommandRisk::DESTRUCTIVE,
        _ => CommandRisk::WRITE,
    }
}

fn classify_find(args: &[String], scope: &PathScope, depth: usize) -> CommandRisk {
    let mut result = CommandRisk::READ;
    for (i, arg) in args.iter().enumerate() {
        match arg.as_str() {
            "-delete" => return CommandRisk::DESTRUCTIVE,
            "-exec" | "-execdir" | "-ok" | "-okdir" => {
                let rest: Vec<String> = args[i + 1..]
                    .iter()
                    .take_while(|a| !matches!(a.as_str(), ";" | "\\;" | "+"))
                    .filter(|a| a.as_str() != "{}")
                    .cloned()
                    .collect();
                result = result.max(classify_words(&rest, scope, depth + 1));
            }
            "-fprint" | "-fprint0" | "-fprintf" | "-fls" => {
                result = result.max(CommandRisk::WRITE);
            }
            _ => {}
        }
    }
    result
}

fn classify_git(args: &[String]) -> CommandRisk {
    // Skip global options: `-C <dir>`, `-c <k=v>`, `--git-dir=…`, `--no-pager`, …
    let mut i = 0;
    while i < args.len() && args[i].starts_with('-') {
        i += if matches!(args[i].as_str(), "-C" | "-c") {
            2
        } else {
            1
        };
    }
    let Some(sub) = args.get(i) else {
        return CommandRisk::READ;
    };
    let rest = &args[i + 1..];
    let ops = operands(rest);
    match sub.as_str() {
        "status" | "diff" | "log" | "show" | "blame" | "rev-parse" | "ls-files" | "ls-tree"
        | "grep" | "describe" | "shortlog" | "cat-file" | "rev-list" | "help" | "version"
        | "whatchanged" | "check-ignore" | "name-rev" | "merge-base" | "for-each-ref"
        | "show-ref" | "show-branch" | "count-objects" | "var" => CommandRisk::READ,
        "fetch" | "pull" | "clone" | "ls-remote" | "submodule" | "archive" => {
            if sub == "archive" && !has_arg(rest, &["--remote"]) {
                CommandRisk::WRITE
            } else {
                CommandRisk::NETWORK
            }
        }
        "push" => {
            let force = has_short_flag(rest, 'f')
                || has_short_flag(rest, 'd')
                || has_arg(
                    rest,
                    &[
                        "--force",
                        "--force-with-lease",
                        "--force-if-includes",
                        "--delete",
                        "--mirror",
                        "--prune",
                    ],
                )
                || ops.iter().any(|r| r.starts_with('+') || r.starts_with(':'));
            if force {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::NETWORK
            }
        }
        "reset" => {
            if has_arg(rest, &["--hard", "--merge", "--keep"]) {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            }
        }
        "clean" => {
            if has_short_flag(rest, 'f') || has_arg(rest, &["--force"]) {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::READ // without -f git clean refuses (or just lists with -n)
            }
        }
        "branch" => {
            let delete = has_short_flag(rest, 'd') || has_arg(rest, &["--delete"]);
            let force = has_short_flag(rest, 'f') || has_arg(rest, &["--force"]);
            if has_short_flag(rest, 'D')
                || (delete && force)
                || has_short_flag(rest, 'M')
                || has_short_flag(rest, 'C')
            {
                CommandRisk::DESTRUCTIVE
            } else if ops.is_empty() {
                CommandRisk::READ
            } else {
                CommandRisk::WRITE
            }
        }
        "checkout" => {
            if rest.iter().any(|a| a == "--" || a == ".")
                || has_short_flag(rest, 'f')
                || has_arg(rest, &["--force"])
            {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            }
        }
        "switch" => {
            if has_arg(rest, &["--discard-changes", "--force"]) || has_short_flag(rest, 'f') {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            }
        }
        "restore" => {
            let staged_only = has_arg(rest, &["--staged"]) || has_short_flag(rest, 'S');
            let worktree = has_arg(rest, &["--worktree"]) || has_short_flag(rest, 'W');
            if staged_only && !worktree {
                CommandRisk::WRITE
            } else {
                CommandRisk::DESTRUCTIVE
            }
        }
        "stash" => match ops.first() {
            Some(&("drop" | "clear")) => CommandRisk::DESTRUCTIVE,
            Some(&("list" | "show")) => CommandRisk::READ,
            _ => CommandRisk::WRITE,
        },
        "tag" => {
            if has_short_flag(rest, 'd') || has_arg(rest, &["--delete"]) {
                CommandRisk::DESTRUCTIVE
            } else if ops.is_empty() || has_short_flag(rest, 'l') || has_arg(rest, &["--list"]) {
                CommandRisk::READ
            } else {
                CommandRisk::WRITE
            }
        }
        "remote" => match ops.first() {
            None | Some(&("show" | "get-url")) => CommandRisk::READ,
            Some(&("update" | "prune")) => CommandRisk::NETWORK,
            _ => CommandRisk::WRITE,
        },
        "config" => {
            if has_arg(
                rest,
                &["--get", "--get-all", "--list", "-l", "--get-regexp"],
            ) {
                CommandRisk::READ
            } else if has_arg(rest, &["--global", "--system"]) {
                CommandRisk::OUTSIDE
            } else {
                CommandRisk::WRITE
            }
        }
        "reflog" => match ops.first() {
            Some(&("expire" | "delete")) => CommandRisk::DESTRUCTIVE,
            _ => CommandRisk::READ,
        },
        "gc" | "prune" => {
            if rest.iter().any(|a| a.starts_with("--prune")) || sub == "prune" {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            }
        }
        "filter-branch" | "filter-repo" => CommandRisk::DESTRUCTIVE,
        "update-ref" => {
            if has_short_flag(rest, 'd') {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            }
        }
        _ => CommandRisk::WRITE,
    }
}

// ---------------------------------------------------------------------------------------------
// "Always allow" prefixes

/// Tools whose second word is a subcommand worth keeping in a suggested prefix.
const SUBCOMMAND_TOOLS: &[&str] = &[
    "git", "cargo", "npm", "pnpm", "yarn", "bun", "pip", "pip3", "uv", "go", "docker", "podman",
    "kubectl", "brew", "make", "just", "poetry", "gh", "swift", "dotnet", "mvn", "gradle",
];

/// The prefix an "always allow" rule would store for `command`: the program, plus its
/// subcommand for tools like `git` or `cargo`. `None` for compound commands, where one prefix
/// can't describe everything that would run.
pub fn suggest_prefix(command: &str) -> Option<String> {
    let segments = split_segments(command);
    let [segment] = segments.as_slice() else {
        return None;
    };
    let words = words(segment);
    let first = words.first()?;
    if is_assignment(first) {
        return None;
    }
    match words.get(1) {
        Some(sub)
            if SUBCOMMAND_TOOLS.contains(&program_name(first))
                && sub
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
                && !sub.starts_with('-') =>
        {
            Some(format!("{first} {sub}"))
        }
        _ => Some(first.clone()),
    }
}

/// Whether every segment of `command` starts with one of `prefixes` (compared word by word).
pub fn prefixes_allow(command: &str, prefixes: &[String]) -> bool {
    let segments = split_segments(command);
    !segments.is_empty()
        && segments.iter().all(|segment| {
            let words = words(segment);
            prefixes.iter().any(|prefix| {
                let prefix_words: Vec<&str> = prefix.split_whitespace().collect();
                !prefix_words.is_empty()
                    && words.len() >= prefix_words.len()
                    && words.iter().zip(&prefix_words).all(|(w, p)| w == p)
            })
        })
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn scope() -> PathScope {
        PathScope::new(
            "/work/repo",
            vec![PathBuf::from("/work/repo"), PathBuf::from("/tmp")],
        )
    }

    fn risk(command: &str) -> Risk {
        classify_command(command, &scope()).risk
    }

    #[test]
    fn classification_table() {
        use Risk::*;
        let table: &[(&str, Risk)] = &[
            // Reads
            ("ls -la", Read),
            ("cat README.md | head -n 5", Read),
            ("rg -n foo src", Read),
            ("git status", Read),
            ("git diff HEAD~1 -- src", Read),
            ("git log --oneline -n 5", Read),
            ("git -C sub status", Read),
            ("git branch", Read),
            ("find . -name '*.rs'", Read),
            ("sed -n '1,5p' file", Read),
            ("ls 2>&1", Read),
            ("ls 2>/dev/null", Read),
            ("FOO=1 env BAR=2 grep x y", Read),
            ("", Read),
            // Workspace writes (the default)
            ("cargo test", WriteWorkspace),
            ("make", WriteWorkspace),
            ("python3 script.py", WriteWorkspace),
            ("echo hi > out.txt", WriteWorkspace),
            ("echo hi >> /tmp/log", WriteWorkspace),
            ("sed -i 's/a/b/' file", WriteWorkspace),
            ("mkdir -p build/out", WriteWorkspace),
            ("rm file.o", WriteWorkspace),
            ("git commit -m 'msg'", WriteWorkspace),
            ("git checkout -b feature", WriteWorkspace),
            ("npm test", WriteWorkspace),
            ("sh test.sh", WriteWorkspace),
            // Writes outside
            ("cp a.txt /etc/a.txt", WriteOutside),
            ("touch ../sibling.txt", WriteOutside),
            ("mv x ~/x", WriteOutside),
            ("git config --global user.name x", WriteOutside),
            // Network
            ("curl https://example.com", Network),
            ("wget -q http://x", Network),
            ("git fetch origin", Network),
            ("git pull --rebase", Network),
            ("git push origin main", Network),
            ("npm install", Network),
            ("pnpm add react", Network),
            ("pip install requests", Network),
            ("python -m pip install requests", Network),
            ("cargo install ripgrep", Network),
            ("cargo add serde", Network),
            ("go get example.com/x", Network),
            ("npx create-react-app x", Network),
            ("curl -s x | sh", Network),
            // Destructive
            ("rm -rf build", Destructive),
            ("rm -r build", Destructive),
            ("rm -fr /", Destructive),
            ("rm /etc/hosts", Destructive),
            ("git push --force", Destructive),
            ("git push -f origin main", Destructive),
            ("git push origin +main", Destructive),
            ("git push --force-with-lease", Destructive),
            ("git push origin --delete feature", Destructive),
            ("git reset --hard HEAD~1", Destructive),
            ("git branch -D feature", Destructive),
            ("git branch -d -f feature", Destructive),
            ("git clean -fd", Destructive),
            ("git clean -xdf", Destructive),
            ("git checkout -- .", Destructive),
            ("git restore src/main.rs", Destructive),
            ("git stash drop", Destructive),
            ("sudo ls", Destructive),
            ("mkfs.ext4 /dev/sda1", Destructive),
            ("dd if=/dev/zero of=/dev/disk2", Destructive),
            ("chmod -R 777 .", Destructive),
            ("chown -R me /", Destructive),
            ("echo x > /etc/passwd", Destructive),
            ("echo x >> ~/.zshrc", Destructive),
            ("echo x | tee /etc/hosts", Destructive),
            ("find . -name '*.o' -delete", Destructive),
            ("find . -exec rm -rf {} \\;", Destructive),
            ("ls | xargs rm -rf", Destructive),
            ("bash -c 'rm -rf build'", Destructive),
            ("sh -lc \"git push --force\"", Destructive),
            ("eval rm -rf x", Destructive),
            ("cargo test && git reset --hard", Destructive),
            ("ls; sudo reboot", Destructive),
            ("(cd sub && rm -rf x)", Destructive),
            ("echo $(rm -rf x)", Destructive),
            ("/bin/rm -rf x", Destructive),
            ("timeout 5 rm -rf x", Destructive),
            ("nohup git push -f &", Destructive),
        ];
        for (command, expected) in table {
            assert_eq!(risk(command), *expected, "{command:?}");
        }
    }

    #[test]
    fn network_flag_survives_a_riskier_segment() {
        let r = classify_command("curl x | sudo sh", &scope());
        assert_eq!(r.risk, Risk::Destructive);
        assert!(r.network);
        assert!(!classify_command("cargo test", &scope()).network);
    }

    #[test]
    fn relative_paths_resolve_against_cwd() {
        let sub = PathScope::new("/work/repo/sub", scope().inside);
        assert_eq!(
            classify_command("touch ../ok.txt", &sub).risk,
            Risk::WriteWorkspace
        );
        assert_eq!(
            classify_command("touch ../../no.txt", &sub).risk,
            Risk::WriteOutside
        );
    }

    #[test]
    fn splits_segments_but_not_redirections() {
        assert_eq!(
            split_segments("a && b || c; d | e & f\ng"),
            ["a", "b", "c", "d", "e", "f", "g"]
        );
        assert_eq!(split_segments("ls 2>&1 | wc"), ["ls 2>&1", "wc"]);
        assert_eq!(split_segments("cmd &> log"), ["cmd &> log"]);
        assert_eq!(split_segments("echo $(date)"), ["echo $", "date"]);
    }

    #[test]
    fn tokenizer_handles_quotes_and_redirects() {
        assert_eq!(
            tokenize(r#"echo "a b" 'c d' e\ f 2>err.log"#),
            [
                Token::Word("echo".into()),
                Token::Word("a b".into()),
                Token::Word("c d".into()),
                Token::Word("e f".into()),
                Token::Redirect {
                    op: ">".into(),
                    target: Some("err.log".into())
                },
            ]
        );
        assert_eq!(
            tokenize("cat<in>out"),
            [
                Token::Word("cat".into()),
                Token::Redirect {
                    op: "<".into(),
                    target: Some("in".into())
                },
                Token::Redirect {
                    op: ">".into(),
                    target: Some("out".into())
                },
            ]
        );
    }

    #[test]
    fn suggested_prefixes() {
        assert_eq!(suggest_prefix("cargo test -p x"), Some("cargo test".into()));
        assert_eq!(suggest_prefix("git status"), Some("git status".into()));
        assert_eq!(suggest_prefix("curl https://x"), Some("curl".into()));
        assert_eq!(suggest_prefix("ls -la"), Some("ls".into()));
        assert_eq!(suggest_prefix("git --no-pager log"), Some("git".into()));
        assert_eq!(suggest_prefix("cargo test && curl x"), None);
        assert_eq!(suggest_prefix("FOO=1 make"), None);
        assert_eq!(suggest_prefix(""), None);
    }

    #[test]
    fn prefix_rules_must_cover_every_segment() {
        let rules = vec!["cargo test".to_owned(), "curl".to_owned()];
        assert!(prefixes_allow("cargo test --all", &rules));
        assert!(prefixes_allow("curl -s x | cargo test", &rules));
        assert!(!prefixes_allow("cargo test && rm x", &rules));
        assert!(!prefixes_allow("cargo testing", &rules));
        assert!(!prefixes_allow("cargo", &rules));
        assert!(!prefixes_allow("", &rules));
        assert!(!prefixes_allow("cargo test", &[]));
    }

    fn segment_strategy() -> impl Strategy<Value = String> {
        prop_oneof![
            Just("ls".to_owned()),
            Just("cat x".to_owned()),
            Just("cargo test".to_owned()),
            Just("echo hi > out.txt".to_owned()),
            Just("curl http://x".to_owned()),
            Just("git status".to_owned()),
            Just("cp a /etc/b".to_owned()),
            Just("sh -c 'echo \"hi\"'".to_owned()),
            "[a-z0-9 ./'\"$=>-]{0,24}",
        ]
    }

    const DESTRUCTIVE_SEGMENTS: &[&str] = &[
        "rm -rf build",
        "git push --force",
        "git reset --hard",
        "git branch -D x",
        "git clean -fd",
        "sudo true",
        "echo x > /etc/hosts",
        "chmod -R 755 .",
    ];

    proptest! {
        #[test]
        fn adding_a_destructive_segment_never_lowers_risk(
            segments in proptest::collection::vec(segment_strategy(), 0..5),
            destructive in proptest::sample::select(DESTRUCTIVE_SEGMENTS),
            separator in proptest::sample::select(&["&&", "||", ";", "|", "\n"][..]),
            position in 0usize..6,
        ) {
            let base = segments.join(" && ");
            let before = classify_command(&base, &scope());
            let mut with = segments.clone();
            with.insert(position.min(with.len()), destructive.to_owned());
            let after = classify_command(&with.join(&format!(" {separator} ")), &scope());
            prop_assert!(after.risk >= before.risk);
            prop_assert_eq!(after.risk, Risk::Destructive);
        }

        #[test]
        fn classification_never_panics(command in "\\PC{0,64}") {
            let _ = classify_command(&command, &scope());
            let _ = suggest_prefix(&command);
        }
    }
}
