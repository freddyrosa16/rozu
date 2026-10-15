# The `rozu` CLI

`rozu chat` runs the agent against OpenAI (Responses API), Anthropic (Messages API) or xAI (Responses API
at `api.x.ai`) with an API key, or against your Claude, ChatGPT or SuperGrok subscription through the vendor's
official CLI (see [providers.md](providers.md)). The agent gets the workspace tools `read_file`, `list_dir`,
`grep`, `write_file`, `apply_patch` and `shell`. File tools are confined to the workspace directory; shell
commands run sandboxed (see [approvals.md](approvals.md)).

```sh
cargo build --release
```

## Keys and models
```sh
# Store API keys in the OS keychain (service "dev.rozu", account = provider id).
# Prompts without echo; piping works too: `pbpaste | rozu auth set anthropic`.
./target/release/rozu auth set openai      # or: anthropic, xai
./target/release/rozu auth status          # which providers have a key, and from where; CLI sign-in status
./target/release/rozu auth delete xai
./target/release/rozu auth login codex     # runs the official `codex login`; also: grok

# Known models with context window and USD prices per million tokens (* = provider default).
./target/release/rozu models
```

Model selection: `--model` takes `provider/model` or a bare model id (the provider is inferred), and
`--provider` alone picks that provider's default model (see `rozu models`, `*` marks the defaults); models
missing from `rozu models` still work, just without a cost estimate. Prices in the model list are copied by
hand and need verifying against each provider's pricing page.

Without `--provider`/`--model` and with no API key configured, rozu uses the first installed of `claude-cli`,
`codex` and `grok`.

## Chatting
```sh
# One-shot: the answer streams to stdout; tool calls, token usage and estimated cost go to stderr.
./target/release/rozu chat --workspace . "list the files and summarize README"
./target/release/rozu chat --provider anthropic "list the files and summarize README"
./target/release/rozu chat --model xai/grok-4 "list the files and summarize README"

# Interactive: omit the prompt. Ctrl-C cancels the current turn, a second Ctrl-C exits; Ctrl-D or /exit quits.
./target/release/rozu chat --workspace .

# Approval mode (default: auto). See approvals.md.
./target/release/rozu chat --approval read-only "explain src/main.rs"
./target/release/rozu chat --approval full "run the tests and fix what fails"
```

Slash commands in the interactive session:

| command | does |
|---|---|
| `/model <provider/model>` (or `/model anthropic`) | switches for the next turn and keeps the conversation; `/model` alone shows the current one |
| `/pin <note>` | adds a note to the project memory (`.rozu/pinned.md`) |
| `/compact` | shrinks the context now (see below) |
| `/exit`, `/quit` | leaves |

Ctrl-C during a turn is read straight from the terminal (each press counts, even two in one paste), so a second
press always exits.

## Saved chats
Every chat is saved: one append-only JSONL file per session plus a SQLite index, in
`~/Library/Application Support/dev.rozu.rozu/sessions` on macOS (`ROZU_SESSIONS_DIR` moves it). The desktop
app uses the same store.

```sh
./target/release/rozu sessions                 # this workspace's chats; --all for every workspace, or a search word
./target/release/rozu chat --resume            # continue this workspace's latest
./target/release/rozu chat --resume 20261014T1302Z -- "and now the tests"
./target/release/rozu chat --no-save           # don't save this one
```

Codex and Claude subscription chats use temporary vendor sessions while rozu keeps the saved conversation.
Grok also keeps its own vendor history, which `--no-save` does not disable. These settings concern local
session history; see [provider details](providers.md) for restart behavior and limitations.

## Project memory and compaction
`ROZU.md` and `AGENTS.md` in the workspace root, plus notes added with `/pin <note>` (kept in
`.rozu/pinned.md`), are sent with every request, after the system prompt. Each file is read with a 32 KB cap.
Memory files and `.rozu` must be regular files/directories, not symlinks; pinned notes cannot grow beyond 32 KB.

Long chats compact themselves: at 60% of the context window old tool outputs are hidden, at 80% older turns
are summarized; the system prompt, project memory and the last 3 turns are never touched. `/compact` does it
now; `--memory off|mask|summarize|default` picks the policy. Vendor CLIs (`claude-cli`, `codex`, `grok`) keep
and compact their own context, so `/compact` does nothing there.

File writes and patches are limited to 1 MiB. Patches also cap edit/hunk counts and matching work; split
larger changes into smaller files or use a reviewed command. Approval details remain complete and scrollable;
requests needing an approval larger than 2 MiB are denied instead of clipped.

## Evals
Dry runs script the model, so they need no keys; add `--mode live --provider ...` for a real one.
Tasks and replays still execute real tools and shell checks. Inspect their fixtures/recordings first, then
pass `--trusted` to authorize execution. Replay copies reject symlinks and special files; fixture paths must
stay inside their temporary workspace. The memory eval does not need `--trusted`.

```sh
./target/release/rozu eval tasks --trusted      # evals/tasks: each task in a temp repo, checked by its outcome
./target/release/rozu eval replay --trusted evals/recorded/fix-failing-test.jsonl --workspace evals/recorded/fix-failing-test
./target/release/rozu eval memory --seeds 3     # rows per memory condition in rozu-eval-memory.jsonl
```

## Other commands
```sh
./target/release/rozu doctor    # crate versions, log and audit paths, sandbox status
./target/release/rozu term      # your shell inside a rozu PTY session (smoke test for the terminal layer)
```

`rozu mcp-serve` and `rozu grok-hook` are internal: vendor CLIs start them.

## Environment variables
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
- `ROZU_SESSIONS_DIR`: where saved chats live.
- `RUST_LOG`: log filter (default `info`). Logs go to `~/Library/Logs/rozu/rozu.log` (`ROZU_LOG_FILE` moves it,
  `ROZU_LOG_STDERR=1` sends them to stderr). API keys are redacted from logs. Approval decisions are logged under
  the `rozu::approval` target and, structured, in the audit log.
- `ROZU_AUDIT_LOG`: path of the approval audit log (default `~/Library/Logs/rozu/approvals.jsonl`).
- `NO_COLOR`: disables the dim styling.
