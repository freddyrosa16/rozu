# Providers

Two kinds of provider sit behind the same agent interface:

| Kind | Auth | Ids |
|---|---|---|
| HTTP adapters | API key from env or keychain | `openai`, `anthropic`, `xai` |
| CLI-backed (subscriptions) | Your plan, logged in inside the official vendor CLI | `claude-cli`, `codex`, `grok` |

A CLI-backed provider spawns the official vendor binary as a subprocess and speaks its machine protocol over
stdio. The CLI does its own login and keeps its own credentials. **rozu never reads, copies or extracts tokens
or credential files** (no `~/.claude*`, `~/.codex/auth.json`, `~/.grok/auth.json`, keychain items of other
apps) and implements no OAuth of its own. If the CLI isn't installed or not logged in, rozu says so and tells
you the vendor's own login command.

## API keys (`openai`, `anthropic`, `xai`)
Keys live in the OS keychain (service `dev.rozu`, account = provider id); `rozu auth set <provider>` stores
one, and the `*_API_KEY` environment variables override it. Keys are wrapped so they never reach logs. Token
usage and an estimated USD cost are shown after each turn for models in `rozu models`.

## Claude on your subscription (`claude-cli`)
With a Claude Pro/Max subscription and no API key, rozu drives the official `claude` CLI (Claude Code). The CLI
does its own login; rozu never reads or stores its credentials.

```sh
# One-time: install Claude Code and log in inside it (`claude`, then /login).
./target/release/rozu auth status          # last line: claude-cli: /path/to/claude (2.x.y), logged in (claude.ai)

./target/release/rozu chat --provider claude-cli "list the files and summarize README"
./target/release/rozu chat --provider claude-cli --model opus   # sonnet (default), opus, haiku or any id claude accepts
./target/release/rozu chat --model haiku "…"                    # the aliases imply claude-cli
```

Without `--provider`/`--model` and with no API key configured, rozu chooses the first installed subscription
CLI in Claude, Codex, Grok order (falling back to Claude if none is installed). Claude runs its own
agent loop, but its Bash/Edit/Write/web tools are disabled: it uses rozu's tools (`read_file`, `list_dir`, `grep`,
`write_file`, `apply_patch`, `shell`) through rozu's MCP server, so the same approval modes, workspace checks and
sandbox apply, and anything else it wants to do is decided by rozu (prompting you as needed, denied if rozu can't
classify it). Token usage, including cache reads/writes, is shown after each turn without a cost (it's covered by the
subscription). `ROZU_CLAUDE_BIN` points rozu at a specific `claude` binary; otherwise it looks on `PATH` and in
`~/.local/bin`. `ANTHROPIC_API_KEY` is removed from the CLI's environment so it uses your subscription login.

## ChatGPT on your subscription (`codex`)
With a ChatGPT plan and no API key, rozu drives the official Codex CLI's `codex app-server`. Sign in with ChatGPT
is done by Codex itself; rozu never sees or refreshes the tokens.

```sh
./target/release/rozu auth login codex     # runs the official `codex login` (opens the browser)
./target/release/rozu auth status          # codex: /opt/homebrew/bin/codex (0.147.0), logged in (ChatGPT)

./target/release/rozu chat --provider codex "list the files and summarize README"
./target/release/rozu chat --provider codex --model gpt-6.1-sol  # `default` = the model Codex is configured with
./target/release/rozu models                                     # codex models come from the installed CLI
```

Codex runs its own agent loop with thread policy `approvalPolicy="untrusted"`, approval reviewer `user`, and its
`workspace-write` sandbox (network off, no extra writable roots; `read-only` in rozu's read-only mode). rozu verifies those settings before
starting any turn and refuses the session if Codex does not confirm them. Codex 0.162 no longer accepts
`untrusted` in startup configuration, so the app-server starts with `on-request` and `read-only`; the stricter
thread policy is then applied through its supported API. Its own shell commands and patches stay enabled under
that verified policy, and **every approval request Codex sends is answered by rozu**: commands go through rozu's risk classifier
(Codex's `zsh -lc '…'` wrapper is unwrapped first), file changes through rozu's path checks (`.rozu/` is denied,
deletes are destructive), and escalations (a retry outside the sandbox, network access, more permissions) always
ask you. Approvals are one-time: rozu never answers "for this session". On a Codex card `u` means "approve once";
Codex's sandbox stays on. Anything rozu can't map (MCP elicitations, dynamic tools, user-input requests, unknown
methods) is denied, and a ChatGPT token-refresh request is refused. rozu's own tools are also available to Codex
through rozu's MCP server. Decisions land in the audit log with `"provider": "codex"`. Without `--provider`/`--model`
and no API key, rozu uses `claude-cli`, or `codex` if only the Codex CLI is installed. `ROZU_CODEX_BIN` points rozu at
a specific `codex` binary; otherwise it looks on `PATH` and in `/opt/homebrew/bin`. `OPENAI_API_KEY` and
`CODEX_API_KEY` are removed from Codex's environment so it uses your ChatGPT login.

## Grok on your subscription (`grok`)
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
Grok's environment.

## Residual risk with vendor CLIs
For a vendor CLI's *built-in* tools (today only Codex keeps any), the vendor's own sandbox is the enforcement
layer; rozu only decides yes/no. Wrapping the whole CLI subprocess in rozu's Seatbelt profile is an open
investigation item in [PLAN.md](../PLAN.md).
