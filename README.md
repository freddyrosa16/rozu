# rozu

A coding agent written in Rust, with a CLI and a native macOS desktop app. It supports tool calling and
multiple LLM providers: OpenAI, Anthropic and xAI by API key; Claude, ChatGPT and SuperGrok subscriptions
through the vendors' official CLIs. An embedded terminal and web search are planned.

**Status:** early. See [PLAN.md](PLAN.md) for the implementation plan and
[docs/gui-spike.md](docs/gui-spike.md) for the GUI stack decision.

## Docs
- [docs/cli.md](docs/cli.md): every `rozu` command, the interactive chat, saved sessions, project memory,
  compaction, evals and environment variables.
- [docs/providers.md](docs/providers.md): API-key providers and the subscription-backed ones (`claude-cli`,
  `codex`, `grok`): what runs, how it is confined, how sign-in works.
- [docs/approvals.md](docs/approvals.md): approval modes, the Seatbelt sandbox, destructive and opaque
  commands, project rules and the audit log.
- [docs/architecture.md](docs/architecture.md): a beginner's path through one chat, crate boundaries,
  session lifecycle, and development checks.

## Requirements
Install [Rust with rustup](https://www.rust-lang.org/tools/install). This workspace requires Rust 1.99;
`rust-toolchain.toml` selects 1.99.0 with Clippy and rustfmt. The CLI runs on macOS and Linux; the desktop
UI requires macOS and the Xcode command line tools. Building on Linux needs a C compiler, `pkg-config`
and OpenSSL development headers (`build-essential pkg-config libssl-dev` on Ubuntu).

## Try the app (macOS)
The desktop app is a sidebar of projects and their chats plus a centered chat, driven by the same core as
`rozu chat`. On first run it asks which AI you pay for (Claude, ChatGPT or Grok), installs and signs in to its
official CLI in Terminal, and lets you pick the model, effort, approval mode and a folder to work in. The gear
at the bottom left reopens that page.

```sh
cargo build --locked -p rozu-cli -p rozu-app   # the app needs `rozu` next to it for tool calls
./target/debug/rozu-app ~/path/to/project
```

Chats are saved by default (the same store as `rozu chat`, so `rozu sessions` lists them and `rozu chat --resume`
continues them): reopen one from the sidebar after a restart and it carries on where it was. `ROZU.md`,
`AGENTS.md` and pinned notes in the folder are sent with every request, as in the CLI. Hover a chat to delete
it (its saved history goes too); removing a project only hides its chats, and they come back when the folder
is added again. Removing projects leaves their project files intact.

## Try the CLI
```sh
cargo build --locked --release -p rozu-cli

./target/release/rozu auth set openai                # API key into the OS keychain (or: anthropic, xai)
./target/release/rozu chat --workspace . "list the files and summarize README"
./target/release/rozu chat                           # interactive; Ctrl-D or /exit quits
./target/release/rozu chat --provider claude-cli     # your Claude subscription, through Claude Code
./target/release/rozu chat --resume                  # continue this workspace's latest chat
./target/release/rozu doctor                         # versions, log paths, sandbox status
```

The agent gets `read_file`, `list_dir`, `grep`, `write_file`, `apply_patch` and `shell`. File tools are
confined to the workspace; on macOS, shell commands use Seatbelt when it is available. Linux currently
has no OS sandbox: approval checks do not restrict what an approved shell process can access. Check
`rozu doctor` for the active sandbox. See
[docs/cli.md](docs/cli.md) and [docs/approvals.md](docs/approvals.md).

## Check a change
Start with `./scripts/check.sh quick` for formatting and landing-page tests. Run `./scripts/check.sh rust`
for Clippy, Rust tests and the bundled mocked evaluations. The complete check is:

```sh
./scripts/check.sh
```

The complete check also needs Node.js 22+ and [cargo-deny](https://embarkstudios.github.io/cargo-deny/).
Evaluations use scripted model responses and need no API keys, but task and replay fixtures execute real
tools and check commands. The script explicitly trusts the bundled fixtures; review imported evals before
running them. See the [development guide](docs/architecture.md#verification) for the CI and build process.

## Repository layout
| Path | What |
|---|---|
| `crates/rozu-core` | Agent loop, session model, approval policy, compaction, audit log |
| `crates/rozu-runtime` | Builds an agent for a model choice (API key or vendor CLI), system prompt, project memory; shared by the CLI and the app |
| `crates/rozu-tools` | Workspace tools, command risk classifier, Seatbelt sandbox |
| `crates/rozu-providers` | OpenAI / Anthropic / xAI HTTP adapters and the model registry |
| `crates/rozu-cli-providers` | Claude Code, Codex app-server and Grok Build drivers |
| `crates/rozu-mcp` | rozu's stdio MCP server (how vendor CLIs reach rozu's tools) |
| `crates/rozu-store` | Saved sessions (JSONL + SQLite index) and project memory |
| `crates/rozu-secrets` | API keys in the OS keychain |
| `crates/rozu-pty` | Terminal layer (portable-pty + alacritty_terminal) |
| `crates/rozu-activity` | Activity states and the orb shown in the app |
| `crates/rozu-cli` | The `rozu` binary |
| `crates/rozu-app` | The desktop app (GPUI, macOS) |
| `evals/` | Scripted tasks and recorded sessions for `rozu eval` |
| `spikes/` | Throwaway GUI spikes (not workspace members) |

The static landing page (`index.html`, `styles.css`, `waves.js`, `tests/waves.test.cjs`) lives at the root
because GitHub Pages serves it. The earlier Python agent is preserved on the
[`archive/python-v0`](https://github.com/freddyrosa16/rozu/tree/archive/python-v0) branch.

## License
MIT, see [LICENSE](LICENSE).
