# Working on rozu

rozu is a Rust workspace (edition 2024, MSRV 1.85): a coding agent with a CLI (`crates/rozu-cli`, binary `rozu`)
and a GPUI desktop app (`crates/rozu-app`, macOS only). The crate map is in README.md; the plan and open
questions in PLAN.md; user-facing docs in `docs/`.

## Build and test
```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings   # CI fails on any warning
cargo test --workspace
cargo run -q -p rozu-cli -- eval tasks                   # mocked evals, no keys needed
```
CI runs all of the above on macOS and Linux plus `cargo-deny`. Everything must build on Linux too: GPUI is
macOS-only, so `rozu-app` gates it behind `cfg(target_os = "macos")`.

The provider tests spawn fake vendor CLIs (shell scripts under `crates/*/tests/fixtures`) and write to temp
dirs, so they need an unsandboxed `$TMPDIR` to pass; when running the test suite through rozu itself, approve
the unsandboxed rerun.

## Rules
- Licensing: rozu is MIT. Do not copy code from Zed's `crates/terminal` or `terminal_view` (GPL-3.0); read
  for ideas only. The terminal pane is written from scratch on `alacritty_terminal` + `portable-pty`.
- Credentials: rozu never reads another app's credential files or tokens (`~/.claude*`, `~/.codex/auth.json`,
  `~/.grok/auth.json`). Subscription providers go through the official vendor CLI's own login.
- Safety: the Seatbelt sandbox is the boundary in every approval mode; the classifier only decides when to
  ask. Destructive and opaque commands always prompt. Default-deny anything a vendor CLI asks that rozu can't
  map to a tool and risk.
- Dependencies: `cargo-deny` rejects OpenSSL-style licenses; `reqwest` uses `native-tls` for that reason.
  `keyring` stays on 3.x (4.x needs Rust 1.88).
- Secrets go through `secrecy::SecretString`; never log them. Tracing output is redacted as a second layer.
- Keep the CLI and the app in step: both build the agent through `rozu-runtime::AgentFactory`, use its
  system prompt and memory helpers, and save chats through `rozu-store`. Put anything both need in
  `rozu-runtime`, not in `rozu-cli` or `rozu-app`.
- Tests use fixtures (recorded SSE / CLI transcripts), table-driven cases and few comments. Add a fixture
  rather than a mock when a provider protocol changes.
