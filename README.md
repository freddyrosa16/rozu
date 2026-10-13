# rozu

A native Rust desktop coding agent (in the spirit of Codex / Grok Bot): an agent loop with tool calling,
an embedded terminal, multiple LLM providers (OpenAI, Anthropic, xAI) and, later, web search.

**Status:** fresh start. See [PLAN.md](PLAN.md) for the implementation plan and
[docs/gui-spike.md](docs/gui-spike.md) for the GUI stack decision.

## Previous Python version
The earlier Python agent (Boot.dev-style `backend/` with the calculator, plus the old `desktop-rust` scaffold) is
preserved on the [`archive/python-v0`](https://github.com/freddyrosa16/rozu/tree/archive/python-v0) branch.

The static landing page (`index.html`, `styles.css`, `waves.js`, `assets/`, `tests/waves.test.cjs`) is still on main
for now because GitHub Pages serves it.

## Try the CLI (M3)
`rozu chat` runs the agent against OpenAI (Responses API), Anthropic (Messages API) or xAI (Responses API
at `api.x.ai`) with workspace tools: `read_file`, `list_dir`, `grep`, `write_file`, `apply_patch` and `shell`.
File tools are confined to the workspace directory; shell commands run sandboxed (see below).

```sh
cargo build --release

# Store API keys in the OS keychain (service "dev.rozu", account = provider id).
# Prompts without echo; piping works too: `pbpaste | rozu auth set anthropic`.
./target/release/rozu auth set openai      # or: anthropic, xai
./target/release/rozu auth status          # which providers have a key, and from where
./target/release/rozu auth delete xai

# Known models with context window and USD prices per million tokens (* = provider default).
./target/release/rozu models

# One-shot: the answer streams to stdout; tool calls, token usage and estimated cost go to stderr.
./target/release/rozu chat --workspace . "list the files and summarize README"
./target/release/rozu chat --provider anthropic "list the files and summarize README"
./target/release/rozu chat --model xai/grok-4 "list the files and summarize README"

# Interactive: omit the prompt. Ctrl-C cancels the current turn, a second Ctrl-C exits; Ctrl-D or /exit quits.
# `/model <provider/model>` (or just `/model anthropic`) switches for the next turn and keeps the conversation;
# `/model` alone shows the current one.
./target/release/rozu chat --workspace .

# Approval mode (default: auto). See "Approvals and sandbox" below.
./target/release/rozu chat --approval read-only "explain src/main.rs"
./target/release/rozu chat --approval full "run the tests and fix what fails"

# Your shell inside a rozu PTY session (smoke test for the terminal layer); exit the shell to return.
./target/release/rozu term
```

### Claude on your subscription (`claude-cli`)
With a Claude Pro/Max subscription and no API key, rozu drives the official `claude` CLI (Claude Code). The CLI
does its own login; rozu never reads or stores its credentials.

```sh
# One-time: install Claude Code and log in inside it (`claude`, then /login).
./target/release/rozu auth status          # last line: claude-cli: /path/to/claude (2.x.y), logged in (claude.ai)

./target/release/rozu chat --provider claude-cli "list the files and summarize README"
./target/release/rozu chat --provider claude-cli --model opus   # sonnet (default), opus, haiku or any id claude accepts
./target/release/rozu chat --model haiku "…"                    # the aliases imply claude-cli
```

Without `--provider`/`--model`, `claude-cli` is the default when no API key is configured. Claude runs its own
agent loop, but its Bash/Edit/Write/web tools are disabled: it uses rozu's tools (`read_file`, `list_dir`, `grep`,
`write_file`, `apply_patch`, `shell`) through rozu's MCP server, so the same approval modes, workspace checks and
sandbox apply, and anything else it wants to do is decided by rozu (prompting you as needed, denied if rozu can't
classify it). Token usage, including cache reads/writes, is shown after each turn without a cost (it's covered by the
subscription). `ROZU_CLAUDE_BIN` points rozu at a specific `claude` binary; otherwise it looks on `PATH` and in
`~/.local/bin`. `ANTHROPIC_API_KEY` is removed from the CLI's environment so it uses your subscription login.

### Approvals and sandbox
Every tool call gets a risk class (read, write in workspace, write outside, network, opaque, destructive) and is
either auto-approved or shown as an approval card with the exact command or diff, the sandbox and the network state:

| `--approval` | runs without asking | asks |
|---|---|---|
| `read-only` | reads | everything else |
| `auto` (default) | reads, workspace writes | network, writes outside the workspace, opaque, destructive |
| `full` | everything else | opaque, destructive |

**The sandbox is the boundary, in every mode, `full` included.** On macOS, shell commands run under Seatbelt
(`sandbox-exec`): they can read anywhere but only write inside the workspace, a private per-session temp dir (the
command's `$TMPDIR`; `/tmp` and your own `$TMPDIR` are not writable) and `/dev`, and the network is blocked unless
that call was approved with network access (the agent sets `network: true`, or the command is recognized as
networked, e.g. `curl`, `git fetch`, `npm install`). When you approve a command that writes to named paths outside
the workspace (`cp build.tgz ~/Downloads/`), the card lists them and the sandbox lets that one command write
exactly those paths. If the sandbox blocks something anyway, the output says `blocked by rozu sandbox: writes
outside workspace…` and rozu asks once whether to rerun it unsandboxed.

Destructive commands (`rm -rf`, `git push --force`, `git reset --hard`, `git branch -D`, `git clean -fd`, `sudo`,
redirects to paths outside the workspace, …) **always** ask, in every mode. So do **opaque** commands, whose effect
rozu can't read off the command line: language interpreters (`python`, `perl`, `ruby`, `node`, `osascript`, …),
`sh -c`/`bash -c`/`zsh -c`, `eval`, `source`, `exec`, scripts piped into a shell, and any output redirect or `tee` to
a file (`> out.txt`).

Keys at the prompt:

| key | does |
|---|---|
| `y` | approve; the command still runs in the sandbox |
| `n` | deny; `n <reason>` (or a reason on the next line) is passed to the agent |
| `a` | approve and always allow this rule in this project: the command prefix (`cargo test`), the interpreter and script (`python3 -m pytest`), or for network clients the program **and host** (`curl` to `example.com` only). Not offered for destructive commands, inline interpreter code or redirects |
| `u` | run **unsandboxed, this command only**: no sandbox at all, full access to your files and the network. Never remembered, never chosen by a mode or rule. Only offered where there is a sandbox to lift |

Rules live in `.rozu/approvals.json` in the workspace; tools and sandboxed commands can't write there. Without a
terminal on stdin, every prompt is denied. There is no sandbox on Linux yet, so in auto mode shell commands that
write ask first. Commands time out after 120 s by default (max 600 s), and the whole process group is killed on
timeout or Ctrl-C.

Every approval decision (automatic or yours, allowed or denied) is appended to the audit log
`~/Library/Logs/rozu/approvals.jsonl` (owner-only; `approvals.jsonl` in the data directory on other systems;
`ROZU_AUDIT_LOG` moves it): time, session id, tool, decision and who made it, rule, mode, risk, sandboxed or not,
granted paths, a SHA-256 of the command or diff and a redacted, shortened copy of it.

Ctrl-C during a turn is read straight from the terminal (each press counts, even two in one paste), so a second
press always exits.

Model selection: `--model` takes `provider/model` or a bare model id (the provider is inferred), and
`--provider` alone picks that provider's default model (see `rozu models`, `*` marks the defaults); models missing
from `rozu models` still work, just without a cost estimate. Prices in the model list are copied by hand and need verifying against each provider's
pricing page.

Environment variables:
- `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `XAI_API_KEY`: if set and non-empty, they **override** the keychain key.
- `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, `XAI_BASE_URL`: API roots (defaults `https://api.openai.com/v1`,
  `https://api.anthropic.com`, `https://api.x.ai/v1`), for proxies and tests.
- `ROZU_PROVIDER`, `ROZU_MODEL`: defaults for `--provider` / `--model`.
- `ROZU_CLAUDE_BIN`: the `claude` binary for `claude-cli`.
- `RUST_LOG`: log filter (default `info`). Logs go to `~/Library/Logs/rozu/rozu.log` (`ROZU_LOG_FILE` moves it,
  `ROZU_LOG_STDERR=1` sends them to stderr). API keys are redacted from logs. Approval decisions are logged under
  the `rozu::approval` target and, structured, in the audit log.
- `ROZU_AUDIT_LOG`: path of the approval audit log (default `~/Library/Logs/rozu/approvals.jsonl`).
- `NO_COLOR`: disables the dim styling.

## Auth
- API keys for OpenAI, Anthropic and xAI, stored in the OS keychain (never in files or logs).
- Claude subscription: through the official `claude` CLI, which logs in and keeps its credentials itself
  (`--provider claude-cli`, above). rozu never reads Claude Code's credential files or tokens.
- ChatGPT subscription (Sign in with ChatGPT through the official Codex CLI) is planned (M3.5 part 2).
  No Grok subscription login is implemented.

## License
MIT, see [LICENSE](LICENSE).
