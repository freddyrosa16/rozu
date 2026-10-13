# rozu: implementation plan

Native Rust desktop coding agent: agent loop + tool calling, embedded terminal, multiple providers
(OpenAI, Anthropic, xAI), and web search later. Reference points: OpenAI Codex (Rust), Grok Bot, Zed.

Pace: Fred is putting in about 4–5 h/week (SetupViz comes first), so milestones are sized for that.
Fred approved AI-assisted implementation. He reviews every milestone, and Chief reviews before merge.

## Open questions (resolve before the milestone that needs them)
1. **UI design:** blocked until Designer's spec and tokens arrive through Chief (M5).
2. **GUI stack:** spike done. The recommendation is **GPUI + gpui-component**, with Tauri 2 as the fallback; see [docs/gui-spike.md](docs/gui-spike.md). Needs Fred and Chief's sign-off before M5.
3. **Sign in with ChatGPT:** it's allowed for locally run personal and open-source projects. Re-check OpenAI's current terms and whether a client-id registration is needed before M4.
4. Distribution: is personal-use unsigned enough, or do we need a Developer ID for notarized releases? (M6)
5. Sandbox default: should network be off by default for shell tools (Codex-style)? Proposed: yes.

## Requirements
**Must**
- Agent loop with streaming output, tool calling, cancel/interrupt, and turn/token caps.
- Provider abstraction with OpenAI (Responses API), Anthropic (Messages API) and xAI (OpenAI-compatible).
- Tools: read/list/search files, write/patch files, run shell commands, plus an interactive PTY terminal.
- Approval gates for side effects, with risk classes and a per-project allowlist. Every decision is logged.
- macOS sandbox for shell tools (Seatbelt). Writes are limited to the workspace.
- API keys live in the OS keychain only, never in files or logs.
- Sessions saved to disk and resumable.

**Should**
- Sign in with ChatGPT (official PKCE flow) as an alternative to an OpenAI API key.
- Simple project memory (pinned notes / AGENTS.md-style file) and context compaction.
- Eval hooks: replay recorded sessions against mocked providers, plus scripted task evals.

**Could (later)**
- Web search tool, MCP client, Linux/Windows builds, auto-update.

**Non-goals / prohibited**
- Using Claude Pro/Max or SuperGrok/X Premium subscription tokens, or reusing Codex CLI / Claude Code tokens.
  Those accounts are used only through official API keys (or by the user running the official CLI themselves).
- A hosted/server version. This is a local app only.

## Architecture

```
rozu/
├─ Cargo.toml              # workspace
├─ crates/
│  ├─ rozu-core/           # agent loop, session model, events, policy (no UI, no HTTP)
│  ├─ rozu-providers/      # Provider trait + openai / anthropic / xai adapters, SSE, retries
│  ├─ rozu-tools/          # Tool trait, registry, fs tools, shell, approvals, sandbox
│  ├─ rozu-pty/            # PTY sessions (portable-pty) + vt parser (alacritty_terminal / vt100)
│  ├─ rozu-secrets/        # keychain (keyring crate) + ChatGPT sign-in (OAuth PKCE, loopback)
│  ├─ rozu-store/          # sessions & memory on disk (JSONL per session + SQLite index)
│  ├─ rozu-cli/            # headless CLI/TUI driver: dev loop + evals before the GUI exists
│  └─ rozu-app/            # GUI binary (BLOCKED on design spec)
├─ evals/                  # recorded sessions, scripted tasks, expected outcomes
├─ docs/                   # gui-spike.md, ADRs
└─ spikes/                 # throwaway GUI spikes (not workspace members)
```

### Data flow
UI/CLI → `Command` (mpsc) → **core** runtime on a tokio multi-thread runtime → provider stream →
`Event`s (broadcast) → UI. The UI thread never blocks. Streamed tokens are batched per frame (~16 ms).
Cancellation uses `tokio_util::sync::CancellationToken`, which is passed through providers and tools.

### Key interfaces (sketch, not final)
```rust
// rozu-providers
#[async_trait] pub trait Provider: Send + Sync {
    fn id(&self) -> ProviderId;
    async fn stream(&self, req: ChatRequest, cancel: CancellationToken)
        -> Result<BoxStream<'static, Result<StreamEvent>>>;
}
pub enum StreamEvent { TextDelta(String), ToolCallStart{id, name}, ToolCallArgsDelta{id, json},
                       ToolCallEnd{id}, Usage(Usage), Done(StopReason) }

// rozu-tools
#[async_trait] pub trait Tool: Send + Sync {
    fn name(&self) -> &'static str;
    fn schema(&self) -> serde_json::Value;          // JSON Schema via schemars
    fn risk(&self, args: &Value) -> Risk;            // Read | WriteWorkspace | WriteOutside | Network | Destructive
    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput;   // capped head+tail ~30 KB
}
pub trait ApprovalGate { async fn decide(&self, req: ApprovalRequest) -> Decision; } // UI or CLI implements

// rozu-secrets
pub enum Credential { ApiKey(String), ChatGpt(OAuthTokens) }
pub trait SecretStore { fn get(&self, p: ProviderId) -> Result<Option<Credential>>; fn set(..); fn delete(..); }
```

### Components
- **Core agent loop:** `loop { stream; render deltas; if no tool calls break; for each call: policy → approval → execute (timeout, cancel) → append truncated tool_result }`. Independent read-only calls run in parallel. There are turn and token caps. Compaction kicks in at about 80% of the context window (summarize older turns, keep recent turns and pinned items).
- **Providers:** one internal message and tool-schema model with an adapter per provider. OpenAI uses `/v1/responses` SSE. Anthropic uses Messages SSE (`content_block_*`, `input_json_delta`, with `cache_control` for prompt caching). xAI reuses the OpenAI adapter with base URL `https://api.x.ai/v1`. Built on `reqwest` + `eventsource-stream`. Partial tool JSON is accumulated until the block ends. 429/5xx get exponential backoff with jitter and respect `retry-after`, and errors are typed.
- **Tools & sandbox:** `read_file`, `list_dir`, `grep`, `apply_patch`/`write_file`, `shell` (one-shot, `tokio::process`), `terminal` (PTY). On macOS, shell commands run under a `sandbox-exec` Seatbelt profile: writes only in the workspace and tmp, network denied unless approved. On timeout or cancel the whole process group is killed.
- **Approvals:** three modes: read-only, auto-in-workspace (default) and full access. Destructive commands (`rm -rf`, `git push --force`, `git reset --hard`, branch deletion) always prompt. The approval card shows the exact command, and "always allow this prefix" is saved per project. Every decision goes to the session log.
- **Terminal/PTY:** `portable-pty` spawns the user's shell, and the `alacritty_terminal` parser keeps the grid model. The GUI renders the grid (GPUI) or forwards bytes to xterm.js (Tauri). The agent can read the terminal buffer and, with approval, write to it.
- **Secrets:** the `keyring` crate (macOS Keychain, service `dev.rozu`). Keys are wrapped in `secrecy::SecretString`, so they never reach Debug or logs. A redaction filter on `tracing` output adds a second layer of protection.
- **Sign in with ChatGPT:** OpenAI's official OAuth 2.0 PKCE public-client flow. The system browser opens with a loopback redirect, tokens are stored in the keychain, and refresh is handled. No reuse of Codex CLI token files.
- **Sessions & memory:** one append-only JSONL file per session (messages, tool calls, approvals, usage) plus a SQLite index for listing and search. Memory is a per-project `ROZU.md` or AGENTS.md (pinned into context) plus user-pinned notes.
- **Eval/test hooks:** a `Provider` mock that replays recorded SSE fixtures, plus an `evals/` runner (`rozu-cli eval`) that runs scripted tasks in temp repos and checks the outcomes (files changed, tests pass, approvals requested). Trace events are emitted for every loop step.

## Milestones
Each one is a vertical slice that can be demoed, reviewed by Fred, and then reviewed by Chief.

### M0: Skeleton & CI (≈1 week)
- [ ] Cargo workspace with empty crates, `rust-toolchain.toml` (stable), MIT license, `deny.toml`
- [ ] GitHub Actions: fmt, clippy `-D warnings`, test on macOS + Ubuntu, `rust-cache`
- [ ] `tracing` setup with a redaction layer

**Done when:** CI is green on main, `cargo test --workspace` passes locally, and README links PLAN.

### M1: Core loop end-to-end in the CLI, one provider (≈2–3 weeks)
- [ ] Internal message/tool model, `Provider` trait, OpenAI Responses adapter with streaming + tool calls
- [ ] Tool registry plus `read_file`, `list_dir`, `grep` (read-only)
- [ ] `rozu-cli chat`: streams text, executes tool calls, Ctrl-C cancels mid-stream
- [ ] API key read from the keychain (`rozu-cli auth set openai`)

**Done when:** "list the files and summarize README" works against the real API, and wiremock SSE fixture tests cover text, a tool call, an interrupted stream, and a 429 retry.

### M2: All providers + retries (≈2 weeks)
- [ ] Anthropic adapter (tool_use/tool_result, input_json_delta, prompt caching)
- [ ] xAI via the OpenAI adapter, plus a provider/model switch in the CLI
- [ ] Shared retry/backoff and typed errors; usage accounting

**Done when:** the same scripted task passes on all 3 providers, and fixture tests pass for each adapter.

### M3: Write tools, shell, sandbox, approvals (≈3 weeks)
- [ ] `apply_patch`/`write_file`, `shell` with timeout + process-group kill
- [ ] Seatbelt profile on macOS; risk classification; approval modes; per-project allowlist; decision log
- [ ] PTY session crate (`portable-pty` + `alacritty_terminal`) driven from the CLI

**Done when:** writing outside the workspace is blocked by the sandbox (test), network access triggers an approval prompt, a destructive command always prompts, and an eval task "fix a failing test in a temp repo" passes.

### M4: Sessions, memory, ChatGPT sign-in, evals (≈2–3 weeks)
- [ ] JSONL sessions + SQLite index, resume; ROZU.md memory; compaction
- [ ] Sign in with ChatGPT (PKCE loopback) → keychain; token refresh
- [ ] `rozu-cli eval` runner with ≥5 scripted tasks; replay tests from recorded sessions

**Done when:** a session survives restart and resumes, a long session compacts without losing pinned memory, ChatGPT login works end-to-end on the Mac, and the eval suite runs in CI with mocked providers.

### M5: GUI (BLOCKED until Designer's spec arrives through Chief)
- [ ] Stack chosen per the spike (pending sign-off), with tokens imported into the theme (frontend-from-design-system)
- [ ] Primitives + component gallery screen → chat view (virtualized, batched streaming), tool/approval cards, terminal pane, settings (keys, providers, sign-in)

**Done when:** every screen matches the spec (visual review), streaming 2k lines stays smooth, approval cards work, and the terminal is usable.

### M6: Packaging & release (≈1–2 weeks)
- [ ] `.app` bundle (cargo-bundle or `tauri build`), arm64 first
- [ ] Optional: Developer ID signing + notarization in a tag-triggered workflow; cargo-dist or tauri-action

**Done when:** a tagged build produces a downloadable `.app` that launches on a clean Mac.

### Later
Web search tool, MCP client, Linux/Windows, auto-update.

## Testing, CI & release
- **Local gates:** `cargo fmt --all --check`, `cargo clippy --all-targets --all-features -- -D warnings`, `cargo test --workspace` (nextest optional), `cargo deny check`, `cargo audit`.
- **Unit** tests in-module. **Integration** tests under `tests/`. **Snapshots** with `insta` (normalized transcripts). **HTTP** mocked with `wiremock` using recorded SSE fixtures per provider. **Property** tests with `proptest` (patch application, output truncation, risk classifier).
- **Sandbox tests** run on macOS CI only. **Evals** run with mocked providers in CI and with live providers manually (needs keys).
- **CI:** GitHub Actions matrix (macos-latest, ubuntu-latest; Windows later), `dtolnay/rust-toolchain@stable`, `Swatinem/rust-cache@v2`.
- **Release:** tag-triggered workflow. Unsigned for personal use (right-click → Open). Signed and notarized (`codesign --options runtime`, `notarytool`, `stapler`) once a Developer ID exists.

## Risks
| Risk | L | I | Mitigation |
|---|---|---|---|
| GPUI API churn / sparse docs | M | M | Pin version, keep core UI-agnostic, use gpui-component, read Zed source |
| Provider SSE format drift | M | M | Adapter per provider, fixture tests, typed errors |
| Seatbelt (`sandbox-exec`) is deprecated by Apple | L | M | It's what Codex uses today; isolate it behind a `Sandbox` trait |
| Subscription-auth terms change | M | H | API keys are the default; re-check terms before each release |
| Limited weekly time | H | M | Small slices, CLI-first so value lands before the GUI |
| Xcode license not accepted on the Mac (blocks the Metal toolchain) | H | L | Fred runs `sudo xcodebuild -license` once |

## Traceability
| Requirement | Milestone |
|---|---|
| Agent loop, streaming, cancel | M1 |
| OpenAI / Anthropic / xAI | M1 / M2 |
| Tools, sandbox, approvals | M1 (read), M3 |
| Terminal/PTY | M3 (core), M5 (UI) |
| Keychain API keys | M1 |
| Sign in with ChatGPT | M4 |
| Sessions & memory | M4 |
| Eval hooks | M1 (fixtures), M4 (runner) |
| GUI | M5 (blocked on spec) |
| Release | M6 |
| Web search | Later |
