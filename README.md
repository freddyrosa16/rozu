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

## Try the CLI (M2)
`rozu chat` runs the agent against OpenAI (Responses API), Anthropic (Messages API) or xAI (Responses API
at `api.x.ai`) with read-only workspace tools (`read_file`, `list_dir`, `grep`), all confined to the
workspace directory.

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
```

Model selection: `--model` takes `provider/model` or a bare model id (the provider is inferred), and
`--provider` alone picks that provider's default model. Defaults are `openai/gpt-5.1`,
`anthropic/claude-sonnet-4-5` and `xai/grok-4`; models missing from `rozu models` still work, just without
a cost estimate. Prices in the model list are copied by hand and need verifying against each provider's
pricing page.

Environment variables:
- `OPENAI_API_KEY`, `ANTHROPIC_API_KEY`, `XAI_API_KEY`: if set and non-empty, they **override** the keychain key.
- `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL`, `XAI_BASE_URL`: API roots (defaults `https://api.openai.com/v1`,
  `https://api.anthropic.com`, `https://api.x.ai/v1`), for proxies and tests.
- `ROZU_PROVIDER`, `ROZU_MODEL`: defaults for `--provider` / `--model`.
- `RUST_LOG`: log filter (default `info`, logs go to stderr). API keys are redacted from logs.
- `NO_COLOR`: disables the dim styling.

## Auth
- API keys for OpenAI, Anthropic and xAI, stored in the OS keychain (never in files or logs).
- OpenAI's official **Sign in with ChatGPT** is the only subscription login (planned, M4).
  Claude and Grok subscription logins are not implemented and their tokens are not used.

## License
MIT, see [LICENSE](LICENSE).
