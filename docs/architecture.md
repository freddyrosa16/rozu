# Architecture and development

## Start with one chat

A Cargo workspace is a group of Rust packages, called crates, built together. Each directory under
`crates/` has its own `Cargo.toml`; the root manifest lists shared dependency versions. A crate can use
another crate only when its manifest declares that dependency. The CLI and desktop app are two frontends
for the same agent, so shared behavior belongs below those frontends.

To learn the code, follow `rozu chat "summarize README.md"` through these files in order:

1. [`rozu-cli/src/main.rs`](../crates/rozu-cli/src/main.rs) parses the command and dispatches to
   [`chat.rs`](../crates/rozu-cli/src/chat.rs). Chat opens the workspace, creates or resumes a session,
   and prepares terminal input and approval prompts.
2. [`rozu-runtime/src/factory.rs`](../crates/rozu-runtime/src/factory.rs) builds the agent, its provider
   and its tools. [`context.rs`](../crates/rozu-runtime/src/context.rs) adds the system prompt and project
   memory to the session. Both the CLI and app use this runtime.
3. [`rozu-core/src/agent.rs`](../crates/rozu-core/src/agent.rs) runs the turn: send conversation messages
   to the provider, receive text or tool calls, execute tools, and send their results back until the model
   finishes or the turn is cancelled. [`provider.rs`](../crates/rozu-core/src/provider.rs) defines the
   provider interface; implementations live in `rozu-providers` and `rozu-cli-providers`.
4. [`rozu-core/src/exec.rs`](../crates/rozu-core/src/exec.rs) checks tool requests against the approval
   policy before executing them. [`rozu-tools/src/lib.rs`](../crates/rozu-tools/src/lib.rs) lists the actual
   file and shell tools. For this example, reading the README goes through the workspace path checks.
5. [`rozu-cli/src/chat/render.rs`](../crates/rozu-cli/src/chat/render.rs) prints agent events while the
   turn runs. Chat then saves the session through
   [`rozu-store/src/session.rs`](../crates/rozu-store/src/session.rs), so a later command can resume it.

Start by reading the functions reached by that example; you do not need to read every provider first.
For a desktop chat, replace step 1 with [`rozu-app/src/backend.rs`](../crates/rozu-app/src/backend.rs) and
step 5 with the app's event feed. Subscription providers run their own agent loop, so their requests enter
rozu's approval/tool path through the vendor adapter and MCP server instead of the HTTP turn loop.

## Where a change belongs

| Change | Start here |
|---|---|
| Terminal flags, input or output | `rozu-cli` |
| Desktop views and event handling | `rozu-app` |
| Behavior the CLI and app must share | `rozu-runtime` |
| Turn control, approval decisions or conversation state | `rozu-core` |
| File access, shell execution or command classification | `rozu-tools` |
| A provider's wire protocol | `rozu-providers` or `rozu-cli-providers` |
| Saving, resuming or searching chats | `rozu-store` |

Keep dependencies pointing from frontends toward shared crates. Core defines interfaces that provider
crates implement; it does not import the CLI or app. Prefer a small function in the existing responsible
module before adding another crate or abstraction.

## Shared runtime

The CLI and desktop app construct agents through `rozu-runtime::AgentFactory`. The factory owns the
workspace tools, approval policy, audit identity, private session directory, and vendor provider sessions.
Model selection lives in `rozu-runtime/src/models.rs`; prompt and project-memory handling live in
`rozu-runtime/src/context.rs`. Approval choices and sandbox labels live in `rozu-runtime/src/approval.rs`
so both frontends offer the same actions. A frontend supplies its approval gate and host description.

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
Command classification and remembered command-prefix rules are separate modules. Remembered approvals
live under the user's config directory, keyed by the canonical workspace path; repository-local approval
files are ignored. Rules cannot authorize destructive, opaque or network requests; destructive and
opaque requests always prompt. On macOS, Seatbelt restricts shell writes and network access in every
approval mode when available; reads are not confined to the workspace. Linux currently uses `NoSandbox`, so an approved shell process has no OS confinement.
File tools still validate workspace paths. Approval prompts are policy decisions, not an OS sandbox.
Changes must preserve destructive-command prompts and default denial of unknown vendor requests.

File writes and patch reads use [`workspace_file.rs`](../crates/rozu-tools/src/workspace_file.rs). It keeps
opened directories attached to the selected workspace, so a later path or symlink change cannot redirect
an operation. The tool implementation handles edits; this helper handles confined access and atomic replacement.

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

Use Rust 1.99.0, selected automatically by rustup from `rust-toolchain.toml`. Rust 1.99 is the declared
minimum supported version; older versions are not supported. Keep that file, the root `rust-version`,
and the CI toolchain versions together when upgrading. Cargo's version 3 resolver considers the declared
Rust version when resolving dependencies; `--locked` keeps checks on the reviewed `Cargo.lock` instead
of silently selecting a new graph.

Run these from the repository root (or run `./scripts/check.sh` for all three):

```sh
./scripts/check.sh quick
./scripts/check.sh rust
./scripts/check.sh security
```

The [script](../scripts/check.sh) is deliberately small: `quick` runs rustfmt and Node's landing-page
tests; `rust` runs Clippy with warnings denied, workspace tests, and the bundled task, replay and memory
evals; `security` runs cargo-deny. Install Node.js 22+ for `quick` and
[cargo-deny](https://embarkstudios.github.io/cargo-deny/) for `security`. For a focused Rust change, run
`cargo test --locked -p <crate-name>` first, then the full Rust check before merging.

Mocked evals do not contact a model provider, but task and replay fixtures can execute shell commands and
checks. The script passes `--trusted` for the bundled task and replay fixtures. Review imported fixtures
before granting that flag. Memory evals do not need it. Generated memory results use a temporary directory
that the script removes on exit.

[CI](../.github/workflows/ci.yml) checks formatting and the landing page on Linux before starting Rust
checks and evals on both macOS and Linux. A separate Linux job checks dependency advisories, licenses and
allowed sources. Runs have time limits; newer pushes cancel superseded runs. The workflow has read-only
repository permissions, disables persisted checkout credentials, and pins third-party actions to full
commit IDs. [Dependabot](../.github/dependabot.yml) opens weekly dependency/action updates for review,
with bounded version-update PR counts. It does not auto-merge them.

GPUI rendering is macOS-only; the backend and feed tests also run on Linux. Fixture tests do not establish
live vendor CLI compatibility or visual correctness of the native app. Test those manually when changing
the relevant integration.

## Build and release boundary

CI verifies changes; it does not publish packages, deploy the landing page, or sign a desktop release.
After checks pass, build the CLI with:

```sh
cargo build --locked --release -p rozu-cli
```

The binary is `target/release/rozu`. On macOS, add `-p rozu-app` to build `target/release/rozu-app` alongside
it; the app needs the CLI binary for tool calls. This produces local executables, not a signed `.app`
bundle. Versioned distribution, signing and notarization remain separate release work.
