//! Risk classification for shell command lines.
//!
//! Deliberately simple: the command is split into segments on `&&`, `||`, `;`, `|`, `&`,
//! newlines, parentheses and backticks (ignoring quotes, which can only add segments), each
//! segment is classified on its own, and the highest risk wins. Unknown commands count as
//! [`Risk::WriteWorkspace`].
//!
//! The classifier only decides when to ask; the sandbox is the boundary. Whatever can't be read
//! off the command line is [`Risk::Opaque`] and asks in every mode (unless the user's own rule
//! covers a named script): inline code (`python -c`, `perl -e`, `node -e`, `sh -c` / `bash -c` /
//! `zsh -c`, scripts fed to an interpreter on stdin or a heredoc, `eval`, `source`, `exec`,
//! those behind `xargs` or `find -exec`), other interpreters (`osascript`, …) and every output
//! redirect to a file (`>`, `>>`, `tee`).
//!
//! One rule covers script files: a shell, `python`, `perl`, `ruby` or `node` running a named
//! script inside the workspace (`sh test.sh`, `python3 tools/gen.py`) counts like `make`, a
//! sandboxed workspace write. The same script outside the workspace is opaque.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::Risk;
use crate::workspace::normalize;

/// The classification of one command line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRisk {
    pub risk: Risk,
    /// Some segment talks to the network (even if another one is riskier).
    pub network: bool,
    /// Paths outside the scope that the command writes, as far as the command line says
    /// (absolute and normalized, not canonicalized).
    pub outside_writes: Vec<PathBuf>,
    /// Some write goes outside the scope to a place that can't be read off the command line
    /// (`$VAR` paths, globs, `sudo`, `launchctl`, …).
    pub outside_unknown: bool,
    /// Runs code or writes files in a way an "always allow" rule must never cover: inline
    /// interpreter code (`python -c`, `eval`, `source`, a script on a shell's stdin) or an output
    /// redirect to a file.
    pub unrulable: bool,
}

impl CommandRisk {
    const fn new(risk: Risk, network: bool, unrulable: bool) -> Self {
        Self {
            risk,
            network,
            outside_writes: Vec::new(),
            outside_unknown: false,
            unrulable,
        }
    }

    const READ: Self = Self::new(Risk::Read, false, false);
    const WRITE: Self = Self::new(Risk::WriteWorkspace, false, false);
    const OUTSIDE: Self = Self::new(Risk::WriteOutside, false, false);
    const NETWORK: Self = Self::new(Risk::Network, true, false);
    /// Runs code rozu can't classify (a named script): asks unless a rule covers it.
    const OPAQUE: Self = Self::new(Risk::Opaque, false, false);
    /// Inline code or a redirect: opaque, and never covered by a rule.
    const OPAQUE_INLINE: Self = Self::new(Risk::Opaque, false, true);
    const DESTRUCTIVE: Self = Self::new(Risk::Destructive, false, false);

    fn max(mut self, other: Self) -> Self {
        self.risk = self.risk.max(other.risk);
        self.network |= other.network;
        for path in other.outside_writes {
            if !self.outside_writes.contains(&path) {
                self.outside_writes.push(path);
            }
        }
        self.outside_unknown |= other.outside_unknown;
        self.unrulable |= other.unrulable;
        self
    }

    /// `self` plus a write to `target` (outside: also recorded as an outside write).
    fn writing(self, target: Target) -> Self {
        match target {
            Target::Inside => self,
            Target::Outside(path) => Self {
                outside_writes: vec![path],
                ..self
            },
            Target::Unknown => Self {
                outside_unknown: true,
                ..self
            },
        }
    }

    /// Writes somewhere outside that the command line doesn't name.
    fn unknown_outside(self) -> Self {
        Self {
            outside_unknown: true,
            ..self
        }
    }
}

/// Where a path argument points.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Target {
    Inside,
    /// Outside, at this absolute (normalized) path.
    Outside(PathBuf),
    /// Outside or undeterminable: `$VAR`s rozu doesn't know, `~user`, globs reaching out.
    Unknown,
}

/// Where relative paths start and which directories count as "inside".
#[derive(Debug, Clone)]
pub struct PathScope {
    /// Directory relative paths are resolved against.
    pub cwd: PathBuf,
    /// Directories writes may go to without counting as outside (workspace, session temp dir).
    pub inside: Vec<PathBuf>,
    /// Variables expanded at the start of a path (`HOME` also for `~`), e.g. `TMPDIR`, which
    /// shell commands get set to the session temp dir.
    pub vars: Vec<(String, PathBuf)>,
}

impl PathScope {
    pub fn new(cwd: impl Into<PathBuf>, inside: Vec<PathBuf>) -> Self {
        Self {
            cwd: cwd.into(),
            inside,
            vars: Vec::new(),
        }
    }

    /// Expands `$name` / `${name}` (and `~` for `HOME`) at the start of paths.
    pub fn with_var(mut self, name: &str, value: impl Into<PathBuf>) -> Self {
        self.vars.push((name.to_owned(), value.into()));
        self
    }

    fn var(&self, name: &str) -> Option<&Path> {
        self.vars
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_path())
    }

    /// `arg` with a leading `~` or `$VAR` expanded; `None` if it uses anything else of the
    /// shell's (other variables, `~user`, command substitution).
    fn expand(&self, arg: &str) -> Option<PathBuf> {
        let (base, rest) = if let Some(rest) = arg.strip_prefix('~') {
            if !(rest.is_empty() || rest.starts_with('/')) {
                return None; // ~user
            }
            (self.var("HOME")?, rest)
        } else if let Some(after) = arg.strip_prefix('$') {
            let (name, rest) = match after.strip_prefix('{') {
                Some(braced) => braced.split_once('}')?,
                None => {
                    let end = after
                        .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                        .unwrap_or(after.len());
                    after.split_at(end)
                }
            };
            if !(rest.is_empty() || rest.starts_with('/')) {
                return None;
            }
            (self.var(name)?, rest)
        } else {
            return Some(PathBuf::from(arg));
        };
        if rest.contains('$') {
            return None;
        }
        Some(base.join(rest.trim_start_matches('/')))
    }

    fn target(&self, arg: &str) -> Target {
        let Some(path) = self.expand(arg) else {
            return Target::Unknown;
        };
        if path.as_os_str().to_string_lossy().contains('$') {
            return Target::Unknown;
        }
        let joined = if path.is_absolute() {
            path
        } else {
            self.cwd.join(path)
        };
        let normalized = normalize(&joined);
        if self.inside.iter().any(|root| normalized.starts_with(root)) {
            Target::Inside
        } else if arg.contains(['*', '?', '[', '{']) {
            Target::Unknown
        } else {
            Target::Outside(normalized)
        }
    }

    /// Whether a path argument stays inside.
    #[cfg(test)]
    fn contains(&self, arg: &str) -> bool {
        self.target(arg) == Target::Inside
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
        return CommandRisk::DESTRUCTIVE.unknown_outside();
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

/// Special files writing to which changes nothing on disk.
fn is_harmless_sink(path: &str) -> bool {
    matches!(
        path,
        "/dev/null" | "/dev/stdout" | "/dev/stderr" | "/dev/tty"
    )
}

/// An output redirect to a file is opaque (it can truncate anything, e.g. a bare `> file`);
/// one leaving the scope is destructive.
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
    if is_harmless_sink(target) {
        return CommandRisk::READ;
    }
    match scope.target(target) {
        Target::Inside => CommandRisk::OPAQUE_INLINE,
        outside => CommandRisk {
            unrulable: true,
            ..CommandRisk::DESTRUCTIVE
        }
        .writing(outside),
    }
}

/// Whether `name` runs a program text: language interpreters and AppleScript.
fn is_interpreter(name: &str) -> bool {
    const INTERPRETERS: &[&str] = &[
        "perl",
        "ruby",
        "irb",
        "node",
        "nodejs",
        "deno",
        "php",
        "lua",
        "luajit",
        "tclsh",
        "wish",
        "osascript",
        "Rscript",
        "pwsh",
        "powershell",
        "jshell",
        "groovy",
        "scala",
        "julia",
    ];
    INTERPRETERS.contains(&name)
        || ["python", "pypy", "perl", "ruby", "lua", "php"]
            .iter()
            .any(|base| {
                name.strip_prefix(base).is_some_and(|v| {
                    !v.is_empty() && v.chars().all(|c| c.is_ascii_digit() || c == '.')
                })
            })
        || name == "python"
}

/// `<interpreter> <script file>` or `python -m <module>`: opaque, but stable enough for an
/// "always allow" rule naming that script. Everything else (`-c`/`-e` code, flags first, code on
/// stdin) is inline code.
fn runs_named_script(name: &str, args: &[String]) -> bool {
    match args.first().map(String::as_str) {
        Some("-m") if name.starts_with("python") || name.starts_with("pypy") => {
            args.get(1).is_some_and(|m| !m.starts_with('-'))
        }
        Some(first) => !first.starts_with('-') && !first.starts_with('+'),
        None => false,
    }
}

/// `<interpreter> <script file>` (`runs_named_script` holds). One rule for every interpreter
/// that runs a script like `make` runs a Makefile — shells, `python`, `perl`, `ruby`, `node`: a
/// script file inside the scope is sandboxed workspace code ([`Risk::WriteWorkspace`]), so
/// `sh test.sh` and `python test.py` are treated alike. Anything else is opaque (a rule may
/// cover it): a script outside the scope or behind a `$VAR`, `python -m <module>`, and other
/// interpreters (`osascript` can drive other apps outside the sandbox).
fn named_script_risk(name: &str, args: &[String], scope: &PathScope) -> CommandRisk {
    let script_host = SHELLS.contains(&name)
        || matches!(name, "node" | "nodejs")
        || ["python", "pypy", "perl", "ruby"].iter().any(|base| {
            name.strip_prefix(base)
                .is_some_and(|v| v.chars().all(|c| c.is_ascii_digit() || c == '.'))
        });
    match args.first() {
        Some(script) if script_host && script != "-m" && scope.target(script) == Target::Inside => {
            CommandRisk::WRITE
        }
        _ => CommandRisk::OPAQUE,
    }
}

const SHELLS: &[&str] = &["sh", "bash", "zsh", "dash", "ksh", "fish", "csh", "tcsh"];

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
    // Skip `FOO=bar`, shell keywords and transparent wrappers. `exec` replaces the shell with
    // whatever follows, so it makes the segment opaque too.
    let mut i = 0;
    let mut prefix = CommandRisk::READ;
    while i < words.len() {
        let w = words[i].as_str();
        if w == "exec" {
            prefix = CommandRisk::OPAQUE_INLINE;
            i += 1;
        } else if is_assignment(w)
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
    prefix.max(classify_program(&words[i..], scope, depth))
}

/// Classifies `words` once wrappers and keywords are skipped: the program and its arguments.
fn classify_program(words: &[String], scope: &PathScope, depth: usize) -> CommandRisk {
    let Some(first) = words.first() else {
        return CommandRisk::READ;
    };
    let args = &words[1..];
    let name = program_name(first);

    if ALWAYS_DESTRUCTIVE.contains(&name) || name.starts_with("mkfs") {
        return CommandRisk::DESTRUCTIVE.unknown_outside();
    }
    if NETWORK_COMMANDS.contains(&name) {
        return CommandRisk::NETWORK;
    }
    if is_interpreter(name) {
        if (name.starts_with("python") || name.starts_with("pypy"))
            && args.first().is_some_and(|a| a == "-m")
            && args.get(1).is_some_and(|m| m == "pip" || m == "pip3")
        {
            return pip_risk(&args[2..]).max(CommandRisk::OPAQUE);
        }
        return if runs_named_script(name, args) {
            named_script_risk(name, args, scope)
        } else {
            CommandRisk::OPAQUE_INLINE
        };
    }
    match name {
        name if SHELLS.contains(&name) => {
            match args
                .iter()
                .position(|a| a.starts_with('-') && !a.starts_with("--") && a.contains('c'))
            {
                // The script is classified too, so `sh -c 'rm -rf x'` stays destructive.
                Some(pos) => match args.get(pos + 1) {
                    Some(script) => {
                        classify_depth(script, scope, depth + 1).max(CommandRisk::OPAQUE_INLINE)
                    }
                    None => CommandRisk::OPAQUE_INLINE,
                },
                // A script file is classified like any interpreter's; a script on stdin
                // (`… | sh`) is inline code.
                None if runs_named_script(name, args) => named_script_risk(name, args, scope),
                None => CommandRisk::OPAQUE_INLINE,
            }
        }
        "eval" => classify_depth(&args.join(" "), scope, depth + 1).max(CommandRisk::OPAQUE_INLINE),
        "source" | "." => CommandRisk::OPAQUE_INLINE,
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
            let base = if recursive {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            };
            writes_to(base, operands(args), scope, CommandRisk::DESTRUCTIVE)
        }
        "chmod" | "chown" | "chgrp" => {
            let base = if has_short_flag(args, 'R') || has_arg(args, &["--recursive"]) {
                CommandRisk::DESTRUCTIVE
            } else {
                CommandRisk::WRITE
            };
            // The first operand is the mode / owner.
            let paths = operands(args).into_iter().skip(1).collect();
            writes_to(base, paths, scope, CommandRisk::DESTRUCTIVE)
        }
        "tee" => {
            let paths: Vec<&str> = operands(args)
                .into_iter()
                .filter(|p| !is_harmless_sink(p))
                .collect();
            if paths.is_empty() {
                CommandRisk::READ
            } else {
                // Like a redirect: opaque inside the scope, destructive outside.
                writes_to(
                    CommandRisk::OPAQUE_INLINE,
                    paths,
                    scope,
                    CommandRisk::DESTRUCTIVE,
                )
            }
        }
        "kill" | "killall" | "pkill" => CommandRisk::OUTSIDE,
        "launchctl" | "systemctl" | "crontab" => CommandRisk::OUTSIDE.unknown_outside(),
        "rsync" => {
            let ops = operands(args);
            if ops
                .iter()
                .any(|p| p.contains("::") || p.starts_with("rsync://") || is_remote(p))
            {
                CommandRisk::NETWORK
            } else {
                let dest = ops.last().copied().into_iter().collect();
                writes_to(CommandRisk::WRITE, dest, scope, CommandRisk::OUTSIDE)
            }
        }
        // These write only their destination (the last operand), unless `-t <dir>` names it.
        "cp" | "ln" | "install" if !has_arg(args, &["-t", "--target-directory"]) => {
            let dest = operands(args).last().copied().into_iter().collect();
            writes_to(CommandRisk::WRITE, dest, scope, CommandRisk::OUTSIDE)
        }
        name if PATH_WRITERS.contains(&name) => writes_to(
            CommandRisk::WRITE,
            operands(args),
            scope,
            CommandRisk::OUTSIDE,
        ),
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

/// `base`, raised to `outside` (recording where) for every path that leaves the scope.
fn writes_to(
    base: CommandRisk,
    paths: Vec<&str>,
    scope: &PathScope,
    outside: CommandRisk,
) -> CommandRisk {
    paths
        .into_iter()
        .fold(base, |acc, path| match scope.target(path) {
            Target::Inside => acc,
            target => acc.max(outside.clone().writing(target)),
        })
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
                CommandRisk::OUTSIDE.unknown_outside()
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
            ("cp /etc/hosts hosts.bak", WriteWorkspace),
            ("cargo test", WriteWorkspace),
            ("make", WriteWorkspace),
            ("sed -i 's/a/b/' file", WriteWorkspace),
            ("mkdir -p build/out", WriteWorkspace),
            ("rm file.o", WriteWorkspace),
            ("git commit -m 'msg'", WriteWorkspace),
            ("git checkout -b feature", WriteWorkspace),
            ("npm test", WriteWorkspace),
            ("sh test.sh", WriteWorkspace),
            ("bash scripts/ci.sh --fast", WriteWorkspace),
            ("zsh ./setup.zsh", WriteWorkspace),
            ("python3 script.py", WriteWorkspace),
            ("python tools/gen.py --all", WriteWorkspace),
            ("python3.12 script.py", WriteWorkspace),
            ("perl gen.pl", WriteWorkspace),
            ("ruby task.rb", WriteWorkspace),
            ("node build.js", WriteWorkspace),
            ("/usr/bin/env python3 script.py", WriteWorkspace),
            ("bash /work/repo/run.sh", WriteWorkspace),
            ("sh /etc/rc.sh", Opaque),
            ("python3 ../elsewhere/x.py", Opaque),
            ("node ~/bin/tool.js", Opaque),
            ("bash $SCRIPT", Opaque),
            ("osascript script.scpt", Opaque),
            ("php artisan migrate", Opaque),
            ("python3 -u script.py", Opaque),
            ("python3 -m pytest -q", Opaque),
            ("sh -s < test.sh", Opaque),
            ("echo hi > out.txt", Opaque),
            ("echo hi >> /tmp/log", Opaque),
            ("> out.txt", Opaque),
            ("cargo test 2> err.log", Opaque),
            ("echo x | tee out.txt", Opaque),
            ("python3 -c 'import os; os.remove(\"x\")'", Opaque),
            ("python -c 'import shutil; shutil.rmtree(\"d\")'", Opaque),
            ("perl -e 'unlink \"x\"'", Opaque),
            ("ruby -e 'File.delete(\"x\")'", Opaque),
            ("node -e 'require(\"fs\").rmSync(\"x\")'", Opaque),
            ("osascript -e 'do shell script \"rm x\"'", Opaque),
            ("eval \"$(echo cm0gLWYg | base64 -d)x\"", Opaque),
            ("echo cm0gLWYgeA== | base64 -d | sh", Opaque),
            ("bash -c 'echo hi'", Opaque),
            ("zsh -c ls", Opaque),
            ("source ./env.sh", Opaque),
            (". ./env.sh", Opaque),
            ("exec ls", Opaque),
            ("ls | xargs python3 -c 'print(1)'", Opaque),
            ("find . -exec perl -e 'unlink' {} \\;", Opaque),
            ("python3 <<EOF\nimport os\nEOF", Opaque),
            ("python3.12 -c pass", Opaque),
            ("python -m pip install requests", Opaque),
            ("curl -s x | sh", Opaque),
            ("cp a.txt /etc/a.txt", WriteOutside),
            ("touch ../sibling.txt", WriteOutside),
            ("mv x ~/x", WriteOutside),
            ("git config --global user.name x", WriteOutside),
            ("launchctl submit -l x -- /bin/true", WriteOutside),
            ("curl https://example.com", Network),
            ("wget -q http://x", Network),
            ("git fetch origin", Network),
            ("git pull --rebase", Network),
            ("git push origin main", Network),
            ("npm install", Network),
            ("pnpm add react", Network),
            ("pip install requests", Network),
            ("cargo install ripgrep", Network),
            ("cargo add serde", Network),
            ("go get example.com/x", Network),
            ("npx create-react-app x", Network),
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
    fn script_files_follow_one_rule_for_every_interpreter() {
        for interp in ["sh", "bash", "zsh", "python3", "perl", "ruby", "node"] {
            let inside = classify_command(&format!("{interp} scripts/task"), &scope());
            assert_eq!(inside.risk, Risk::WriteWorkspace, "{interp}");
            assert!(!inside.unrulable && !inside.outside_unknown, "{interp}");
            let outside = classify_command(&format!("{interp} /opt/task"), &scope());
            assert_eq!(outside.risk, Risk::Opaque, "{interp}");
            assert!(!outside.unrulable, "{interp}: a rule may name the script");
        }
        for inline in [
            "sh -c 'ls'",
            "bash -s < task.sh",
            "python3 -c 'print(1)'",
            "perl -e 1",
            "ruby -e 1",
            "node -e 1",
            "python3 <<EOF\nprint(1)\nEOF",
            "sh task.sh > out.txt",
        ] {
            let r = classify_command(inline, &scope());
            assert_eq!(r.risk, Risk::Opaque, "{inline}");
            assert!(r.unrulable, "{inline}");
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
    fn suggested_rules() {
        for (command, want) in [
            ("cargo test -p x", Some("cargo test")),
            ("git status", Some("git status")),
            ("ls -la", Some("ls")),
            ("git --no-pager log", Some("git")),
            ("cargo test && curl x", None),
            ("FOO=1 make", None),
            ("env make", None),
            ("", None),
            // Interpreters: the rule names the script or module, never inline code.
            ("python3 -m pytest -q", Some("python3 -m pytest")),
            ("python3 tools/gen.py --all", Some("python3 tools/gen.py")),
            ("sh test.sh", Some("sh test.sh")),
            ("python3 -c 'print(1)'", None),
            ("perl -e 'unlink \"x\"'", None),
            ("eval ls", None),
            ("sh", None),
            // Output redirects to files are never covered.
            ("cargo test > log.txt", None),
            ("ls 2>/dev/null", Some("ls")),
        ] {
            assert_eq!(
                suggest_rule(command),
                want.map(AllowRule::prefix),
                "{command:?}"
            );
        }
    }

    #[test]
    fn network_rules_are_scoped_to_one_host() {
        for (command, want) in [
            (
                "curl -s https://Example.com/a?b=1",
                Some(("curl", "example.com")),
            ),
            (
                "wget https://user:pw@dl.example.org:8443/x.tgz",
                Some(("wget", "dl.example.org")),
            ),
            (
                "scp build.tgz deploy@build.example.net:/srv",
                Some(("scp", "build.example.net")),
            ),
            ("nc -z localhost 8080", Some(("nc", "localhost"))),
            // No blanket rule without exactly one host.
            ("curl x", None),
            ("curl https://a.com https://b.com", None),
            ("ssh -i key.pem myhost", None),
        ] {
            assert_eq!(
                suggest_rule(command),
                want.map(|(cmd, host)| AllowRule::with_host(cmd, host)),
                "{command:?}"
            );
        }

        let rules = [AllowRule::with_host("curl", "example.com")];
        for (command, allowed) in [
            ("curl -fsSL https://example.com/install", true),
            ("curl https://evil.example/x", false),
            ("curl https://example.com -x http://proxy.evil", false),
            ("curl x", false),
        ] {
            assert_eq!(rules_allow(command, &rules), allowed, "{command:?}");
        }
        // A legacy blanket rule no longer covers a host-scoped client.
        assert!(!rules_allow(
            "curl https://example.com",
            &[AllowRule::prefix("curl")]
        ));
    }

    #[test]
    fn rules_must_cover_every_segment() {
        let rules = [
            AllowRule::prefix("cargo test"),
            AllowRule::prefix("jq"),
            AllowRule::prefix("python3 -m pytest"),
            AllowRule::with_host("curl", "x.dev"),
        ];
        for (command, allowed) in [
            ("cargo test --all", true),
            ("curl -s https://x.dev | jq .", true),
            ("python3 -m pytest -k slow", true),
            ("cargo test && rm x", false),
            ("cargo testing", false),
            ("cargo", false),
            ("", false),
            // A matching prefix doesn't cover a redirect or inline code.
            ("cargo test > out.txt", false),
        ] {
            assert_eq!(rules_allow(command, &rules), allowed, "{command:?}");
        }
        assert!(!rules_allow("cargo test", &[]));
        assert!(!rules_allow(
            "python3 -c 'x'",
            &[AllowRule::prefix("python3")]
        ));
    }

    #[test]
    fn outside_writes_are_collected() {
        let scope = scope().with_var("HOME", "/Users/me");
        let classify = |c: &str| classify_command(c, &scope);

        let r = classify("echo approved > ~/scratch/canary.txt");
        assert_eq!(r.risk, Risk::Destructive);
        assert_eq!(
            r.outside_writes,
            [PathBuf::from("/Users/me/scratch/canary.txt")]
        );
        assert!(!r.outside_unknown);

        let r = classify("cp a.txt /etc/a.txt && mv b $HOME/b");
        assert_eq!(
            r.outside_writes,
            [PathBuf::from("/etc/a.txt"), PathBuf::from("/Users/me/b")]
        );
        let r = classify("cp /etc/hosts hosts.bak");
        assert!(r.outside_writes.is_empty() && !r.outside_unknown);
        assert_eq!(
            classify("touch ../x").outside_writes,
            [PathBuf::from("/work/x")]
        );
        // What can't be read off the command line is unknown.
        for command in [
            "echo x > $SOMEWHERE/f",
            "cp a ~other/b",
            "rm -f ../*.log",
            "sudo touch /etc/x",
            "git config --global user.name x",
        ] {
            let r = classify(command);
            assert!(r.outside_unknown, "{command}");
            assert!(r.outside_writes.is_empty(), "{command}");
        }
        // Interpreter code doesn't name its targets; the sandbox confines it.
        let r = classify("python3 -c 'import os; os.remove(\"/etc/x\")'");
        assert_eq!(r.risk, Risk::Opaque);
        assert!(r.outside_writes.is_empty() && r.unrulable);
    }

    #[test]
    fn variables_in_the_scope_are_inside() {
        let scope = PathScope::new(
            "/work/repo",
            vec![PathBuf::from("/work/repo"), PathBuf::from("/s/tmp")],
        )
        .with_var("TMPDIR", "/s/tmp");
        assert!(scope.contains("$TMPDIR/x"));
        assert!(scope.contains("${TMPDIR}/x"));
        assert!(!scope.contains("$TMPDIR/../x"));
        assert!(!scope.contains("$HOME/x"));
        assert!(!scope.contains("/tmp/x"));
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
            let mut with = segments;
            with.insert(position.min(with.len()), destructive.to_owned());
            let after = classify_command(&with.join(&format!(" {separator} ")), &scope());
            prop_assert!(after.risk >= before.risk);
            prop_assert_eq!(after.risk, Risk::Destructive);
        }

        #[test]
        fn classification_never_panics(command in "\\PC{0,64}") {
            let _ = classify_command(&command, &scope());
            let _ = suggest_rule(&command);
            let _ = rules_allow(&command, &[AllowRule::with_host("curl", "x.dev")]);
        }
    }
}
