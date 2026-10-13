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

## Try the CLI (M1)
`rozu chat` runs the agent against OpenAI's Responses API with read-only workspace tools
(`read_file`, `list_dir`, `grep`), all confined to the workspace directory.

```sh
cargo build --release

# Store your OpenAI API key in the OS keychain (service "dev.rozu", account "openai").
# Prompts without echo; piping works too: `pbpaste | rozu auth set openai`.
./target/release/rozu auth set openai
./target/release/rozu auth status

# One-shot: the answer streams to stdout; tool calls and token usage go to stderr.
./target/release/rozu chat --workspace . "list the files and summarize README"

# Interactive: omit the prompt. Ctrl-C cancels the current turn, a second Ctrl-C exits; Ctrl-D or /exit quits.
./target/release/rozu chat --model gpt-5.1 --workspace .
```

Environment variables:
- `OPENAI_API_KEY`: if set and non-empty, it **overrides** the keychain key.
- `OPENAI_BASE_URL`: API root (default `https://api.openai.com/v1`), for proxies and tests.
- `RUST_LOG`: log filter (default `info`, logs go to stderr). API keys are redacted from logs.
- `NO_COLOR`: disables the dim styling.

## Auth
- API keys for OpenAI (now) and Anthropic and xAI (M2), stored in the OS keychain (never in files or logs).
- OpenAI's official **Sign in with ChatGPT** is the only subscription login (planned, M4).
  Claude and Grok subscription tokens are not used.

## License
MIT, see [LICENSE](LICENSE).
