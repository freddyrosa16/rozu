# Architecture and development

## Shared runtime

The CLI and desktop app construct agents through `rozu-runtime::AgentFactory`. The factory owns the
workspace tools, approval policy, audit identity, private session directory, and vendor provider sessions.
Model selection lives in `runtime/models.rs`; prompt and project-memory handling live in
`runtime/context.rs`. Approval choices and sandbox labels live in `runtime/approval.rs` so both frontends
offer the same actions. A frontend supplies its approval gate and host description.

`rozu-core` owns conversation state, usage accounting, compaction, and the turn loop. Its `agent/session`,
`agent/compaction`, and `agent/stream` modules separate these responsibilities without changing the public
`Agent` or `Session` interfaces. Model providers return tool requests for core to execute; vendor agent
backends execute through rozu's permission boundary and forward events.

## Provider and tool boundaries

`rozu-providers` adapts HTTP streams and handles retries. `rozu-cli-providers` drives the official vendor
CLIs, with a shared JSON-RPC envelope parser for Codex and Grok and a shared approval runtime for all
three subscription providers. That runtime owns a private directory and approval socket, and removes
them when the provider is dropped. Vendor credentials stay with the vendor CLI.

`rozu-tools` owns workspace path resolution, file tools, process execution, and the platform sandbox.
Command classification and persistent prefix/host rules are separate modules. The sandbox remains the
boundary in every approval mode; extracting modules must not weaken destructive-command prompts or
default denial of unknown vendor requests.

## Desktop and CLI

The desktop app separates project/chat orchestration from sidebar, chat, home, setup, and model-picker
views, with a shared composer component. Provider catalog/setup metadata is separate from the backend chat worker. Each worker
owns its session and writer; queued prompts carry their cancellation token. Deleting a chat waits for
the worker's final save before deleting its file and index entry. Failed resume is surfaced as an error.
Model and effort changes apply to the next turn while retaining the session, writer, approval policy,
and private tool directory. Reopening a saved chat restores its stored provider and model.

The CLI keeps chat orchestration separate from terminal rendering. Scripted task and memory evaluations
share sorted fixture discovery and report unreadable fixtures as errors.

## Session storage

`rozu-store` retains the existing JSONL record format and SQLite search index. Log replay and index
projection have separate modules. Resuming a crash-interrupted log trims only its invalid final record
before appending new turns. Invalid records earlier in the file remain errors. Duplicate session
creation is rejected, ID prefixes are matched literally, and rebuilding the index removes stale rows.
Session operations reject IDs containing path separators and headers whose ID differs from the filename.
On the supported macOS/Linux platforms, a session writer holds an exclusive advisory file lock through
recovery and subsequent saves. Another writer or deletion attempt receives an error until it closes.

## Verification

Run these from the repository root:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo run -q -p rozu-cli -- eval tasks
cargo run -q -p rozu-cli -- eval replay evals/recorded/fix-failing-test.jsonl --workspace evals/recorded/fix-failing-test
cargo run -q -p rozu-cli -- eval memory --out /tmp/rozu-memory-eval.jsonl
node --test tests/waves.test.cjs
cargo deny check
```

GitHub CI runs Rust checks and mocked evaluations on macOS and Linux, plus dependency/license checks
and landing-page tests. GPUI rendering is macOS-only; the backend and feed tests also run on Linux.
Fixture tests do not establish live vendor CLI compatibility or visual correctness of the native app.

The workspace currently declares Rust 1.85, but its existing locked dependencies include packages that
require newer Rust (for example `ignore` and `proptest` require 1.88). Use the stable toolchain used by CI.
Rust 1.85 compatibility needs a separate dependency and language-feature compatibility pass; the refactor
does not claim to restore that older toolchain support.
