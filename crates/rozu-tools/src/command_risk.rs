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

use std::path::{Path, PathBuf};

use crate::Risk;
use crate::workspace::normalize;

mod rules;
pub use rules::{AllowRule, rules_allow, suggest_rule};

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

#[cfg(test)]
mod tests;
