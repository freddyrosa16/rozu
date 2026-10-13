# rozu

A native Rust desktop coding agent (in the spirit of Codex / Grok Bot): an agent loop with tool calling,
an embedded terminal, multiple LLM providers (OpenAI, Anthropic, xAI by API key; Claude, ChatGPT and SuperGrok
subscriptions through the vendors' official CLIs) and, later, web search.

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

### ChatGPT on your subscription (`codex`)
With a ChatGPT plan and no API key, rozu drives the official Codex CLI's `codex app-server`. Sign in with ChatGPT
is done by Codex itself; rozu never sees or refreshes the tokens.

```sh
./target/release/rozu auth login codex     # runs the official `codex login` (opens the browser)
./target/release/rozu auth status          # codex: /opt/homebrew/bin/codex (0.147.0), logged in (ChatGPT)

./target/release/rozu chat --provider codex "list the files and summarize README"
./target/release/rozu chat --provider codex --model gpt-6.1-sol  # `default` = the model Codex is configured with
./target/release/rozu models                                     # codex models come from the installed CLI
```

Codex runs its own agent loop with `approval_policy="untrusted"` and its `workspace-write` sandbox (network off;
`read-only` in rozu's read-only mode). Its own shell commands and patches stay enabled (Codex has no switch to turn
them off), but **every approval request Codex sends is answered by rozu**: commands go through rozu's risk classifier
(Codex's `zsh -lc '…'` wrapper is unwrapped first), file changes through rozu's path checks (`.rozu/` is denied,
deletes are destructive), and escalations (a retry outside the sandbox, network access, more permissions) always
ask you. Approvals are one-time: rozu never answers "for this session". On a Codex card `u` means "approve once";
Codex's sandbox stays on. Anything rozu can't map (MCP elicitations, dynamic tools, user-input requests, unknown
methods) is denied, and a ChatGPT token-refresh request is refused. rozu's own tools are also available to Codex
through rozu's MCP server. Decisions land in the audit log with `"provider": "codex"`. Without `--provider`/`--model`
and no API key, rozu uses `claude-cli`, or `codex` if only the Codex CLI is installed. `ROZU_CODEX_BIN` points rozu at
a specific `codex` binary; otherwise it looks on `PATH` and in `/opt/homebrew/bin`. `OPENAI_API_KEY` and
`CODEX_API_KEY` are removed from Codex's environment so it uses your ChatGPT login.

### Grok on your subscription (`grok`)
With a SuperGrok plan and no API key, rozu drives xAI's official Grok Build CLI over ACP (`grok agent stdio`).
Grok Build does its own login (`grok login`); rozu never reads `~/.grok/auth.json` or any token, never uses an API
key for it, and never points it at a proxy.

```sh
npm install -g @xai-official/grok           # or xAI's standalone installer (~/.grok/bin/grok)
./target/release/rozu auth login grok       # runs the official `grok login`
./target/release/rozu auth status           # grok: /…/grok-native (1.0.x), login status unknown (…)

./target/release/rozu chat --provider grok "list the files and summarize README"
./target/release/rozu chat --provider grok --model grok-4.7   # `default` = Grok Build's own default model
```

rozu runs the **native** binary directly (`grok` on `PATH` resolved through its symlinks to `grok-native`; never
`node grok-bootstrap.js`), as one long-lived `grok --no-auto-update agent [-m <model>] --agent-profile <profile>
stdio`. The agent profile rozu writes allows only Grok's `use_tool` meta-tool, so **Grok's built-in tools are off**;
its only tools are rozu's (`rozu__read_file`, …, `rozu__shell`) through rozu's MCP server, under the same approval
modes, workspace checks and sandbox. Grok's permission requests are approved once only for those tools (the call is
then decided by rozu's MCP server) and denied for anything else; file-system and terminal requests are refused.
Decisions land in the audit log with `"provider": "grok"`. Turns are appended to one Grok session (its prompt cache
is per session); a new session in the same process starts only when rozu rewrites the history it sent. Token usage
(input incl. cached, cached, output) is shown without a cost. Grok Build has no login-status command, so
`rozu auth status` can't tell whether it is signed in; a chat says "not logged in … run `rozu auth login grok`" when
it isn't. **Memory features are limited with grok** (Grok adds its own system-prompt block and tool schemas, so rozu
has no per-turn control of the prompt prefix; cache writes aren't reported); `rozu models` and `rozu chat` say so.
`--grok-headless` (or `ROZU_GROK_HEADLESS=true`) switches to a fallback that runs `grok -p … --output-format
streaming-json` per turn; it has no way to pass rozu's MCP server, so rozu's tools only work there if you registered
`rozu mcp-serve` with Grok yourself. `ROZU_GROK_BIN` points rozu at a specific (native) binary; otherwise it looks on
`PATH`, in `~/.grok/bin` and `/opt/homebrew/bin`. `XAI_API_KEY` and `GROK_CLI_CHAT_PROXY_BASE_URL` are removed from
Grok's environment. Without `--provider`/`--model` and no API key, rozu uses the first installed of `claude-cli`,
`codex` and `grok`.

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
- `ROZU_CODEX_BIN`: the `codex` binary for `codex`.
- `ROZU_GROK_BIN`: the native Grok Build binary for `grok` (used as given).
- `ROZU_GROK_HEADLESS`: `true` = the headless fallback for `grok` (same as `--grok-headless`).
- `ROZU_GROK_HOOK_MAX_BYTES`: if set, Grok gets rozu's PostToolUse hook (`rozu grok-hook`), which keeps the head and
  tail of rozu tool results longer than this; off by default (unverified with Grok Build, see PLAN.md).
- `RUST_LOG`: log filter (default `info`). Logs go to `~/Library/Logs/rozu/rozu.log` (`ROZU_LOG_FILE` moves it,
  `ROZU_LOG_STDERR=1` sends them to stderr). API keys are redacted from logs. Approval decisions are logged under
  the `rozu::approval` target and, structured, in the audit log.
- `ROZU_AUDIT_LOG`: path of the approval audit log (default `~/Library/Logs/rozu/approvals.jsonl`).
- `NO_COLOR`: disables the dim styling.

## Auth
- API keys for OpenAI, Anthropic and xAI, stored in the OS keychain (never in files or logs).
- Claude subscription: through the official `claude` CLI, which logs in and keeps its credentials itself
  (`--provider claude-cli`, above). rozu never reads Claude Code's credential files or tokens.
- ChatGPT subscription: Sign in with ChatGPT through the official Codex CLI (`rozu auth login codex` runs
  `codex login`; `--provider codex`, above). rozu never reads `~/.codex/auth.json` or handles ChatGPT tokens.
- SuperGrok subscription: through xAI's official Grok Build CLI (`rozu auth login grok` runs `grok login`;
  `--provider grok`, above). rozu never reads `~/.grok/auth.json`, never uses an xAI API key or the CLI chat proxy for
  it, and implements no OAuth of its own.

## License
MIT, see [LICENSE](LICENSE).
