# rozu: implementation plan

Native Rust desktop coding agent: agent loop + tool calling, embedded terminal, multiple providers
(OpenAI, Anthropic, xAI), and web search later. Reference points: OpenAI Codex (Rust), Grok Bot, Zed.

Pace: Fred is putting in about 4–5 h/week (SetupViz comes first), so milestones are sized for that.
Fred approved AI-assisted implementation. He reviews every milestone, and Chief reviews before merge.

**Direction change (2026-10-13): subscriptions first.** Fred has no API keys for now, only subscriptions
(Claude Max, ChatGPT). rozu therefore gets **CLI-backed providers** (see [Providers](#providers-http-adapters-and-cli-backed-providers))
that drive the official vendor CLIs, next to the existing HTTP adapters, which stay as the optional,
de-prioritized API-key path.

## Open questions (resolve before the milestone that needs them)
1. **UI design:** resolved: Fred's mockup `docs/design/app/index.html` is the spec (M5).
2. **GUI stack:** spike done. The recommendation is **GPUI + gpui-component**, with Tauri 2 as the fallback; see [docs/gui-spike.md](docs/gui-spike.md). Fred approved GPUI on 2026-10-14.
3. **Sign in with ChatGPT:** resolved by using the official Codex CLI (`codex app-server`), which does the
   ChatGPT login itself; rozu implements no OAuth of its own (M3.5).
4. Distribution: is personal-use unsigned enough, or do we need a Developer ID for notarized releases? (M6)
5. Sandbox default: should network be off by default for shell tools (Codex-style)? Proposed: yes (implemented that way in M3; revisit if it's too strict in practice).
6. **Grok subscription: APPROVED (2026-10-13)** as a CLI-backed provider on xAI's official Grok Build CLI over
   ACP (`grok agent stdio`), M3.5c. Grok Build does its own `grok login`; rozu implements **no Grok OAuth** (Hermes'
   path uses xAI's shared OAuth client, which isn't licensed for other apps), never reads `~/.grok/auth.json`,
   never uses `GROK_CLI_CHAT_PROXY_BASE_URL`. The xAI API-key adapter stays as the optional path. Still open: the
   unsent note to xAI asking whether driving a user-installed Grok Build from a desktop app is fine
   (`research/grok-subscription-paths.md` §5).

## Requirements
**Must**
- Agent loop with streaming output, tool calling, cancel/interrupt, and turn/token caps.
- CLI-backed providers on the user's own subscriptions: Claude through the official `claude` CLI, OpenAI
  through the official Codex CLI's `codex app-server` (Sign in with ChatGPT), Grok through xAI's Grok Build CLI
  (`grok agent stdio`, ACP).
- Provider abstraction with OpenAI (Responses API), Anthropic (Messages API) and xAI (OpenAI-compatible)
  HTTP adapters, kept as the optional API-key path.
- Tools: read/list/search files, write/patch files, run shell commands, plus an interactive PTY terminal.
- Approval gates for side effects, with risk classes and a per-project allowlist. Every decision is logged.
- macOS sandbox for shell tools (Seatbelt). Writes are limited to the workspace.
- API keys live in the OS keychain only, never in files or logs.
- Sessions saved to disk and resumable.

**Should**
- Sign in with ChatGPT, via `codex app-server` (no custom OAuth), as an alternative to an OpenAI API key.
- Simple project memory (pinned notes / AGENTS.md-style file) and context compaction.
- Eval hooks: replay recorded sessions against mocked providers, plus scripted task evals.

**Could (later)**
- Web search tool, MCP client, Linux/Windows builds, auto-update.

**Non-goals / prohibited**
- Reading, copying, extracting or reusing subscription tokens or credential files (Claude Code, Codex CLI,
  Grok Build / SuperGrok / X Premium, Hermes), or routing a CLI through a rozu-run proxy
  (`GROK_CLI_CHAT_PROXY_BASE_URL`) that would see its bearer token. Subscriptions are used only by spawning the **official vendor CLI**, which
  does its own login and holds its own credentials; rozu only speaks the CLI's documented machine protocol.
- Any custom OAuth/login flow against a vendor's subscription (including xAI's shared client).
- A hosted/server version. This is a local app only.

## Architecture

```
rozu/
├─ Cargo.toml              # workspace
├─ crates/
│  ├─ rozu-core/           # agent loop, session model, events, policy (no UI, no HTTP)
│  ├─ rozu-providers/      # openai / anthropic / xai HTTP adapters, SSE, retries, provider/model registry
│  ├─ rozu-cli-providers/  # CLI-backed providers: CliProcess, claude stream-json, codex app-server, grok ACP [M3.5]
│  ├─ rozu-mcp/            # stdio MCP server exposing rozu's tools + permission-prompt tool [M3.5]
│  ├─ rozu-tools/          # Tool trait, registry, fs tools, shell, approvals, sandbox
│  ├─ rozu-pty/            # PTY sessions (portable-pty) + vt parser (alacritty_terminal)
│  ├─ rozu-secrets/        # keychain (keyring crate) for the optional API keys
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
pub enum Credential { ApiKey(String) }   // subscriptions: no credential in rozu; the vendor CLI holds it
pub trait SecretStore { fn get(&self, p: ProviderId) -> Result<Option<Credential>>; fn set(..); fn delete(..); }
```

### Components
- **Core agent loop:** `loop { stream; render deltas; if no tool calls break; for each call: policy → approval → execute (timeout, cancel) → append truncated tool_result }`. Independent read-only calls run in parallel. There are turn and token caps. Compaction kicks in at about 80% of the context window (summarize older turns, keep recent turns and pinned items).
- **Providers:** see [Providers](#providers-http-adapters-and-cli-backed-providers) below for the CLI-backed path. HTTP adapters: one internal message and tool-schema model with an adapter per provider. OpenAI uses `/v1/responses` SSE. Anthropic uses Messages SSE (`content_block_*`, `input_json_delta`, with `cache_control` for prompt caching). xAI reuses the OpenAI adapter with base URL `https://api.x.ai/v1`. Built on `reqwest` + `eventsource-stream`. Partial tool JSON is accumulated until the block ends. 429/5xx get exponential backoff with jitter and respect `retry-after`, and errors are typed.
- **Tools & sandbox:** `read_file`, `list_dir`, `grep`, `apply_patch`/`write_file`, `shell` (one-shot, `tokio::process`), `terminal` (PTY). On macOS, shell commands run under a `sandbox-exec` Seatbelt profile **in every approval mode**: writes only in the workspace, a per-session temp dir (the command's `$TMPDIR`) and the outside paths an approved call names, network denied unless approved. The sandbox is the boundary; the classifier only decides when to ask. Running without it takes the user's explicit "unsandboxed, this command only" (`u`) on a prompt. On timeout or cancel the whole process group is killed.
- **Approvals:** three modes: read-only, auto-in-workspace (default) and full access. Destructive commands (`rm -rf`, `git push --force`, `git reset --hard`, branch deletion), interpreter code (`python -c`, `eval`, …) and output redirects always prompt. The approval card shows the exact command, and "always allow this prefix" is saved per project (per host for network clients). Every decision goes to the approval audit log (`approvals.jsonl`).
- **Terminal/PTY:** `portable-pty` (MIT) spawns the user's shell, and the `alacritty_terminal` (Apache-2.0) parser keeps the grid model. The GUI renders the grid (GPUI) or forwards bytes to xterm.js (Tauri). The agent can read the terminal buffer and, with approval, write to it. **Do not copy Zed's terminal code** (`crates/terminal`, `terminal_view`): it is GPL-3.0 and rozu is MIT. Zed may be read for ideas only; the pane is written from scratch on alacritty_terminal + portable-pty.
- **Secrets:** the `keyring` crate (macOS Keychain, service `dev.rozu`) for the optional API keys. Keys are wrapped in `secrecy::SecretString`, so they never reach Debug or logs. A redaction filter on `tracing` output adds a second layer of protection. Logs go to a file (`~/Library/Logs/rozu/rozu.log` on macOS), not the terminal.
- **Sign in with ChatGPT:** done by the official Codex CLI (`codex login` / app-server account APIs), not by rozu. rozu never reads `~/.codex/auth.json` or any other credential file.
- **Sessions & memory:** one append-only JSONL file per session (messages, tool calls, approvals, usage) plus a SQLite index for listing and search. Memory is a per-project `ROZU.md` or AGENTS.md (pinned into context) plus user-pinned notes.
- **Eval/test hooks:** a `Provider` mock that replays recorded SSE fixtures, plus an `evals/` runner (`rozu-cli eval`) that runs scripted tasks in temp repos and checks the outcomes (files changed, tests pass, approvals requested). Trace events are emitted for every loop step.

### Providers: HTTP adapters and CLI-backed providers
Two kinds of provider sit behind the same `Provider`/agent interfaces:

| Kind | Auth | Status |
|---|---|---|
| **CLI-backed** (primary) | The user's subscription, logged in inside the official vendor CLI | M3.5 |
| **HTTP adapters** (OpenAI Responses, Anthropic Messages, xAI) | API key from env or keychain | Done (M1/M2); optional, de-prioritized |

A **CLI-backed provider** spawns the **official vendor binary** as a subprocess (`tokio::process`, own process
group, killed on cancel/drop like `shell`) and speaks its machine protocol over stdio. The CLI does its own login
and keeps its own credentials. **rozu never reads, copies or extracts tokens or credential files** (no
`~/.claude*`, `~/.codex/auth.json`, keychain items of other apps). If the CLI isn't installed or not logged in,
rozu says so and tells the user to run the vendor's own login command.

**Claude (Max subscription):** the official `claude` CLI (Claude Code), already logged in by the user.
- Transport: `claude -p --input-format stream-json --output-format stream-json --include-partial-messages --verbose`
  (one long-lived process per session; user turns written as JSON lines to stdin). The Claude Agent SDK is the
  alternative if the raw CLI protocol proves too thin (it wraps the same CLI).
- Events to map onto `StreamEvent`/`AgentEvent`: `system`/`init` (session id, model, tools, permission mode),
  `stream_event` partial deltas (text and `input_json_delta`), `assistant`/`user` messages incl. `tool_use` and
  `tool_result` blocks, and `result` (stop reason, `usage` with `input_tokens`, `cache_creation_input_tokens`,
  `cache_read_input_tokens`, `output_tokens`; `total_cost_usd` is **notional** on a subscription and is not
  reported as cost).
- Fixture tests: record stream-json transcripts and replay them through a fake `claude` binary (a script on
  `PATH` in tests), like the SSE fixtures.

**OpenAI (Sign in with ChatGPT):** the official Codex CLI's `codex app-server` (JSON-RPC over stdio). Codex
0.147.0 is installed on the Mac and logged in with ChatGPT.
- Thread/turn APIs to start a thread and send turns; item events (agent message deltas, command executions,
  file changes, reasoning) map onto `AgentEvent`s; token-usage events give per-turn usage.
- **Server→client approval requests** for exec commands and file changes are answered by rozu (see below).
- Account/login status comes from app-server's account APIs; if logged out, rozu tells the user to run
  `codex login`. No custom OAuth.

**Grok (SuperGrok subscription):** xAI's official Grok Build CLI over ACP (`grok agent stdio`), M3.5c (open
question 6). Grok Build's built-ins are off (agent profile with `tools: use_tool`); rozu's MCP server is its only
toolset; rozu decides every byte of context it sends, one Grok session per context epoch. See the M3.5c notes.
The xAI API-key HTTP adapter remains the optional path.

#### Tools & approvals with CLI-backed providers
When the vendor CLI runs its own tools, **rozu must still be the gate**: every tool request goes through rozu's
`ApprovalPolicy` and risk classifier, `Destructive` always prompts the user, and decisions are logged like M3's.
**Default-deny:** an approval request rozu can't map to a tool + risk is denied.
- **Claude:** run with a restricted permission mode (no `bypassPermissions`/auto-accept) and
  `--permission-prompt-tool mcp__rozu__approve`, served by a **rozu stdio MCP server** passed with
  `--mcp-config <rozu.json> --strict-mcp-config` (so no user/project MCP servers load). Preferred setup:
  disable Claude's built-in mutating tools (`--disallowedTools Bash,Write,Edit,MultiEdit,NotebookEdit,...`) and
  expose **rozu's own tools** (`read_file`, `list_dir`, `grep`, `apply_patch`, `write_file`, `shell`) over the same
  MCP server, so rozu's workspace checks and Seatbelt sandbox apply exactly as with the HTTP providers. The
  permission-prompt tool still covers anything left enabled.
- **Codex:** start app-server with config overrides for approval policy `untrusted` (chosen over `on-request`,
  which lets Codex run sandboxed commands without asking) and sandbox `workspace-write` (or `read-only` when
  rozu's mode is read-only), cwd = the workspace. Answer each exec/file-change approval request from
  `ApprovalPolicy` (commands go through `classify_command`; file changes through the write-path checks); network
  requests follow rozu's network rule. Done in M3.5 part 2, see the notes there.
- **Grok:** built-ins off via the agent profile, so only `use_tool` → rozu's MCP tools can run. Grok's
  `session/request_permission` is approved (once, never "always") only for `use_tool` → `rozu__<registry tool>`,
  logged `decided_by: delegated-to-rozu-mcp` (rozu mcp-serve then decides the call); everything else is denied
  (default-deny, logged `policy`). `fs/*` and `terminal/*` requests are refused (rozu advertises neither).
- **Residual risk:** for a vendor CLI's *built-in* tools, the vendor's own sandbox is the enforcement layer;
  rozu only decides yes/no. **Investigation item:** wrap the whole CLI subprocess in rozu's Seatbelt profile
  (writes limited to the workspace, temp and the CLI's own state dir, e.g. `~/.claude`/`~/.codex`; network
  limited to the vendor's API host), and check it doesn't break login refresh or the CLI's own sandbox
  (nested `sandbox-exec` may not be allowed).

#### Memory eval with subscription providers
Per the eval spec §1a, live runs use the Claude subscription provider:
- Claude stream-json `result.usage` gives uncached input, cache write and cache read tokens per turn, so the
  **primary metrics are tokens and cache hit rate** (plus latency and recall). `cost_usd` is `null` for
  subscription runs; unit cost from the price table stays as a derived secondary metric. Raw usage (and any
  rate-limit/usage indicators the CLI emits) is stored verbatim.
- Usage → metrics mapping: `input_tokens` (uncached) + `cache_creation_input_tokens` (write) +
  `cache_read_input_tokens` (read) = total input, which is what rozu's `Usage::input_tokens` already holds;
  `cache_read_tokens`/`cache_write_tokens` map 1:1; `cache_hit_rate = Σ read / Σ total input`. `cost_usd` comes from
  `ModelInfo::cost_usd` only when the run used an API-key provider; for CLI-backed providers it is `null`, and
  `total_cost_usd` from the CLI is stored only inside `usage_raw`.
- Pacing: `--max-runs-per-hour N` and `--resume`; on a rate-limit / usage-limit result the runner stops cleanly
  (current run marked `status=rate_limited`, not failed) so `--resume` continues later.
- Codex app-server (0.147.0 schema) reports usage in the `thread/tokenUsage/updated` notification:
  `params.tokenUsage.{total, last}` are `TokenUsageBreakdown`s with `inputTokens`, `cachedInputTokens`,
  `cacheWriteInputTokens` (schema default 0), `outputTokens`, `reasoningOutputTokens`, `totalTokens`, plus
  `modelContextWindow`. `total` is the thread's running sum, `last` the latest model call. rozu reports one turn as
  `total` at `turn/completed` minus `total` at the previous turn's end, mapped as: `inputTokens` → `input_tokens`
  (already includes the cached part), `cachedInputTokens` → `cache_read_tokens`, `cacheWriteInputTokens` →
  `cache_write_tokens` (0 in practice; OpenAI doesn't bill cache writes; record it as `null` in eval rows unless
  non-zero), `outputTokens` (includes reasoning) → `output_tokens`. `reasoningOutputTokens` has no `Usage` field; it
  and the raw breakdown are logged under `rozu::usage` (provider `codex`). Cost is `None` (subscription).
  `cache_hit_rate = Σ cachedInputTokens / Σ inputTokens`. Field names are from the schema; the live values still
  need the smoke test.
- Grok Build (ACP) reports usage in the `session/prompt` result's `_meta`: `inputTokens` (the **whole** prompt,
  cached part included), `cachedReadTokens`, `outputTokens`, `reasoningTokens`; headless `streaming-json` reports
  `input_tokens` as **uncached only**. `grok::protocol::normalize_usage` maps both to `Usage` with `input_tokens` =
  total, `cache_read_tokens` = cached, `cache_write_tokens` = 0 always (never reported; record `null` in eval rows),
  cost `None`. Model capability flags (`ModelInfo::caps`: `prompt_prefix_control: false`, `cache_write_reported:
  false`, `memory_features: Limited`) mark Grok so the UI/eval can tell. Live numbers (Grok Build 1.0.52 pilot): one
  append-only session 82% cache hits, new session per turn 23%, batched masking with a new session per batch 77%.
- **Limit:** rozu doesn't control the CLI's system prompt or prefix layout (Claude Code adds its own system
  prompt and tool definitions), so conditions that depend on exact prompt layout (A's volatile line, byte-exact
  `prefix_sha256`, `cache_control` breakpoint placement) may need the API-key path or Claude Agent SDK options
  (custom system prompt, tool set). Record which conditions were run on which path.

## Milestones
Each one is a vertical slice that can be demoed, reviewed by Fred, and then reviewed by Chief.

### M0: Skeleton & CI (≈1 week)
- [x] Cargo workspace with empty crates, `rust-toolchain.toml` (stable), MIT license, `deny.toml`
- [x] GitHub Actions: fmt, clippy `-D warnings`, test on macOS + Ubuntu, `rust-cache`
- [x] `tracing` setup with a redaction layer

**Done when:** CI is green on main, `cargo test --workspace` passes locally, and README links PLAN.

### M1: Core loop end-to-end in the CLI, one provider (≈2–3 weeks)
- [x] Internal message/tool model, `Provider` trait, OpenAI Responses adapter with streaming + tool calls
- [x] Tool registry plus `read_file`, `list_dir`, `grep` (read-only)
- [x] `rozu-cli chat`: streams text, executes tool calls, Ctrl-C cancels mid-stream
- [x] API key read from the keychain (`rozu-cli auth set openai`)

**Done when:** "list the files and summarize README" works against the real API, and wiremock SSE fixture tests cover text, a tool call, an interrupted stream, and a 429 retry.
- [ ] Live check against the real OpenAI API (needs Fred's key): `rozu chat "list the files and summarize README"`
- [x] Fixture tests: text, tool-call round trip through the agent loop, truncated stream, cancel mid-stream, 429 → success (plus 5xx retry, retry exhaustion, 401)

Notes: `rozu-core` depends on `rozu-tools` (the agent loop uses `ToolRegistry` directly); `rozu-providers`
depends on `rozu-core` for the shared model and `Provider` trait. `ApprovalGate` lives in `rozu-core`; M1 denies
anything that isn't `Risk::Read`.

### M2: All providers + retries (≈2 weeks)
- [x] Anthropic adapter (tool_use/tool_result, input_json_delta, prompt caching)
- [x] xAI via the OpenAI adapter, plus a provider/model switch in the CLI
- [x] Shared retry/backoff and typed errors; usage accounting

**Done when:** the same scripted task passes on all 3 providers, and fixture tests pass for each adapter.
- [x] Scripted cross-provider task (fake `lookup` tool through the agent loop) passes against mocked OpenAI, Anthropic and xAI
- [x] Fixture tests: Anthropic text, tool_use round trip, stream `error` event, 529 overloaded → retry (plus persistent 529, 401, 400); xAI via the OpenAI adapter with its own base URL and key; registry lookup
- [ ] Live check against the real Anthropic API (needs Fred's key): `rozu chat --provider anthropic "list the files and summarize README"`
- [ ] Live check against the real xAI API (needs Fred's key): `rozu chat --provider xai "list the files and summarize README"`
- [x] Verify the model list prices against each provider's pricing page (2026-10-13; see the comments in each adapter's `models()`)

Notes: the registry (`rozu_providers::registry`) owns providers, models and default models; `rozu models`
lists them. Errors are unified as `Auth`, `RateLimited`, `BadRequest`, `Server` (5xx incl. 529), `Network`,
`Cancelled`, plus in-stream `Api`/`Decode`/`UnexpectedEof`. Usage is normalized so `input_tokens` includes
cache reads and writes, which are also carried separately as `cache_read_tokens`/`cache_write_tokens`
(Anthropic reports both; OpenAI/xAI report only reads). Cost prices reads at the cached-input price and
writes at `price_cache_write_per_mtok` (1.25x input for Anthropic, the input price elsewhere). xAI needed no
adapter flags against the fixtures; add one to `OpenAiConfig` if the live check shows a difference.

### M3: Write tools, shell, sandbox, approvals (≈3 weeks)
- [x] `apply_patch`/`write_file`, `shell` with timeout + process-group kill
- [x] Seatbelt profile on macOS; risk classification; approval modes; per-project allowlist; decision log
- [x] PTY session crate (`portable-pty` + `alacritty_terminal`) driven from the CLI

**Done when:** writing outside the workspace is blocked by the sandbox (test), network access triggers an approval prompt, a destructive command always prompts, and an eval task "fix a failing test in a temp repo" passes.
- [x] Sandbox tests (macOS only): writes outside the workspace and into `.rozu/` are blocked, the session `$TMPDIR` stays writable (`/tmp` and the user's temp dir don't), approved outside paths are writable for that call only, `curl` to 1.1.1.1 fails without network approval
- [x] Network commands prompt in auto mode; destructive commands prompt in every mode (also with a matching rule); approval matrix and persisted-rule tests
- [x] Eval: scripted provider fixes a failing shell test in a temp repo via `shell` → `read_file` → `apply_patch` → `shell`
- [x] Timeout/cancel/drop kill the whole process group (background `sleep`s are gone); classifier table + proptest
- [ ] Manual check on the Mac: `rozu chat` approval prompts (y/n/a) and `rozu term`

Notes:
- **Tools:** `write_file` (create/overwrite) and `apply_patch` (exact search/replace `edits`, or a single-file
  unified `diff`; all-or-nothing, with an actionable error on a mismatch) resolve paths with
  `Workspace::resolve_for_write` and reject anything outside the workspace or under `.rozu/` *before* approval.
  `shell` runs `/bin/sh -c` in its own process group (`timeout_secs` default 120, max 600, plus an optional
  `network: true` request), captures stdout/stderr head+tail, and kills the group with `killpg` on timeout,
  cancel, drop and when the command finishes (so background children don't outlive the call).
- **Sandbox:** `Sandbox` trait in `rozu-tools`. `Seatbelt` runs `/usr/bin/sandbox-exec -p <profile>`; the profile is
  `allow default` + `deny file-write*` + writes re-allowed under the workspace, the session temp dir
  (`SessionTemp`: `$TMPDIR/rozu-<session>-<random>`, 0700, removed at exit, exported to commands as
  `TMPDIR`/`TMP`/`TEMP`), `/dev` and the call's approved extra paths, with `<workspace>/.rozu` denied again and
  `(deny network*)` unless the call was approved with network. `/tmp` and the user's `$TMPDIR` are **not**
  writable. `NoSandbox` elsewhere. **Linux Landlock is deferred**; without a sandbox, auto mode asks before shell
  commands that write, and `rozu chat` / `rozu doctor` print a multi-line red WARNING banner on stderr.
  Known Seatbelt limits: `allow default` still permits mach/XPC services; tools that write to caches outside the
  workspace (`~/.cargo` — cargo locks `.package-cache` on every build — `~/.npm`, `~/.cache/pip`, Go's module and
  build caches, …) fail under the sandbox **in every mode, `full` included** (the output says "blocked by rozu
  sandbox: writes outside workspace…" and the user is offered `u`, unsandboxed for that command only, never
  remembered). Documented workarounds (README): start rozu with `CARGO_HOME`, `npm_config_cache`,
  `PIP_CACHE_DIR`, `GOMODCACHE`/`GOCACHE` under the workspace, or point them at the session `$TMPDIR` per command;
  filling a fresh cache needs one approved networked run. A read-write cache allowlist stays an option if this
  is frequent in dogfooding.
- **Risk classifier** (`classify_command`): splits on `&&`, `||`, `;`, `|`, `&`, newlines, parentheses and
  backticks, recurses into `sh -c`/`bash -c`/`eval`/`xargs`/`find -exec`, takes the max. Unknown commands are
  `WriteWorkspace`. One rule for script files: `sh`/`bash`/`zsh` (any shell), `python`, `perl`, `ruby` or `node`
  running a named script **inside the workspace** is `WriteWorkspace` (sandboxed, no prompt in auto/full), like
  `make`; the same script outside the workspace (or behind a `$VAR`), `python -m <module>` and other interpreters
  (`osascript`, …) are `Opaque` but rule-able. `Opaque` and never rule-able: inline code (`-c`/`-e`, code on
  stdin or in a heredoc, `eval`, `source`/`.`, `exec`, those behind `xargs`/`find -exec`) and any output
  redirect or `tee` to a file. Redirects or `tee` to paths outside
  the workspace/session tmp are `Destructive`. It also collects the outside paths a command writes (`~`, `$HOME`,
  `$TMPDIR` expanded; other variables, `~user` and globs are "unknown").
- **Approvals** (`rozu-core::approval`): `ApprovalMode::{ReadOnly, AutoWorkspace (default), FullAccess}`. Every
  mode runs shell commands sandboxed. An approved call may also write the outside paths it names (canonicalized,
  shown on the card; `/`, top-level dirs and `$HOME` itself are too broad and never granted). The prompt's `u`
  (`Decision::ApproveUnsandboxed`) runs one command without the sandbox; it is never automatic and never stored.
  When the sandbox blocks an approved call, the gate is asked once whether to rerun it unsandboxed. "Always
  allow" stores `{tool, prefix, host?}` in `.rozu/approvals.json` (shell prefix = program plus subcommand, e.g.
  `cargo test`, or interpreter plus script, e.g. `python3 -m pytest`; network clients like `curl` get program +
  host, never a blanket rule; every segment of a compound command must match; rules never apply to destructive
  calls, inline interpreter code, output redirects, or in read-only mode). Every decision is an
  `AgentEvent::Approval`, a `tracing` event (target `rozu::approval`, now in `rozu.log` by default) and a line in
  the audit log (below).
- **Audit log** (`rozu-core::audit`): `~/Library/Logs/rozu/approvals.jsonl` on macOS (`approvals.jsonl` in the
  data dir elsewhere; `ROZU_AUDIT_LOG` overrides), 0600, one JSON line per decision: timestamp, session id (shared
  with the `rozu mcp-serve` the chat starts), call id, tool, allow/deny, kind (approve, always allow, approve
  unsandboxed, deny), decided by (mode, rule + which rule, user, policy, cancelled), mode, risk, sandboxed?,
  network, extra writable paths, retry-after-block flag, SHA-256 of the preview and the preview/reason redacted
  and cut to 300 chars.
- **PTY:** `rozu_pty::PtySession` (spawn, write, resize, raw byte subscription, `snapshot()`/`screen_text()` of the
  visible grid, exit status). `rozu term` proxies `$SHELL` through it in raw mode (crossterm). The agent-facing
  `terminal_read`/`terminal_write` tools come with the GUI terminal pane.
- Open question 5 (network off by default for shell) is implemented as proposed: off unless approved.
- QA fixes (2026-10-13): a fast double Ctrl-C no longer hangs (signals are counted, never coalesced, and the
  exit path doesn't wait on locks or the turn); tracing logs go to `~/Library/Logs/rozu/rozu.log` (data dir
  elsewhere) instead of the terminal, `ROZU_LOG_STDERR=1` brings stderr logging back; `--max-steps` must be
  1..=200; the model table was refreshed from the official pricing pages and models gained long-context price
  tiers.
- **Boundary change + QA fixes for f6289f2 (2026-10-13, report `research/qa-f6289f2.md`).** The sandbox, not the
  classifier, is now the boundary in every mode (full access included); see the notes above.
  - BUG-5 (High): in full mode, `python -c os.remove`, `perl -e unlink`, `eval "$(… | base64 -d)…"`,
    `shutil.rmtree` and bare `> file` ran unprompted and unsandboxed. Now they're `Opaque` (prompt in auto/full,
    no rule may cover them) and run under Seatbelt like everything else. Tests: `rozu-core/tests/approvals.rs`
    (each QA variant prompts in full mode; denied → nothing changes; approved → canaries outside the workspace
    survive under real Seatbelt; `u` runs once, isn't persisted, and is never chosen by a mode or rule),
    `classification_table`, `approval_matrix`, CLI `full_mode_prompts_before_interpreter_code`, MCP
    `interpreter_code_prompts_the_parent_even_in_full_access` (REG-4).
  - BUG-6 (Med): an approved outside write in auto mode failed with EPERM. Now the named outside targets are added
    as extra writable paths for that call only; undeterminable targets get a card note, an explicit
    "blocked by rozu sandbox: writes outside workspace" note in the output and a one-time `u` re-prompt. Tests:
    `approved_outside_write_runs_in_auto_mode` (real Seatbelt, REG-5), `approved_outside_paths_are_writable_for_that_call_only`,
    `sandbox_block_*`.
  - BUG-7 (Med): decisions weren't logged (filter dropped `rozu::approval`). Now the default filter is `info`
    and every decision, including `rozu mcp-serve`'s, goes to the audit log. Tests: `every_decision_lands_in_the_audit_log`
    (REG-6), `mcp_decisions_go_to_the_audit_log`, CLI `chat_decisions_go_to_the_audit_log`, audit unit tests.
  - BUG-8 (Low): denial lines printed 2–3 times; now once (test `denial_is_printed_once`).
  - BUG-9 (Low): `gpt-6-astra`/`gpt-6-luna` gained their >272K tiers; `claude-sonnet-4-5` (deprecated) was
    removed from the registry (it still resolves, unpriced).
  - BUG-1 (Low): two `^C` bytes in one write merged into one SIGINT. While a turn runs on a TTY, `ISIG` is off
    and `^C` is a `VEOL` line delimiter, so the stdin reader counts each byte; termios is restored at turn end,
    on exit (incl. the signal path) and on panic. Signals stay the path at the prompt and without a TTY. Test:
    PTY `double_ctrl_c_in_one_write_on_a_terminal_exits` (fails with the turn mode disabled).
  - Tightened auto mode: per-session temp dir instead of `/tmp`/`$TMPDIR` (OBS-2); "always allow" for network
    clients is per host (OBS-3). Claude's built-in `Bash` is denied by policy (it would run outside rozu's
    sandbox); `mcp__rozu__shell` is the way in.

### M3.5: CLI-backed providers (≈2–3 weeks) — Claude CLI first, then Codex app-server, then Grok Build
See [Providers](#providers-http-adapters-and-cli-backed-providers).
- [x] `CliProcess` plumbing: spawn the official binary (path lookup; `--version` shown by `rozu auth status`),
      process-group kill on cancel/drop, stderr to the log file, typed errors (not installed, not logged in,
      rate/usage limit, protocol)
- [x] Claude: stream-json transport and event mapping (deltas, tool_use/tool_result, result usage); fixture tests
      with a fake `claude` binary; `rozu chat --provider claude-cli`
- [x] rozu stdio MCP server (`rozu mcp-serve`): rozu's tools + the `approve` permission-prompt tool, all through
      `ApprovalPolicy`; Claude launched with `--mcp-config --strict-mcp-config --permission-prompt-tool` and its
      built-in mutating tools disallowed
- [x] Codex: `codex app-server` JSON-RPC client (initialize, thread/turn, item events, token usage), approval
      requests answered by `ApprovalPolicy` (default-deny for unmapped requests); sandbox/approval config overrides;
      fixture tests with a fake `codex` app-server; `rozu chat --provider codex`
- [x] Sign in with ChatGPT via the official Codex CLI (`account/read` status; `rozu auth login codex` runs
      `codex login`; `rozu auth status` reads only the auth mode from `codex login status`); no custom OAuth, no
      token handling (a token-refresh request is refused), no reading of Codex's credential files
- [x] Usage (Claude): `result.usage` → `Usage`; `cost_usd` is `None` for subscription models (`Billing::Subscription`);
      the CLI's `total_cost_usd` is only logged (target `rozu::usage`) with the raw usage
- [x] Usage (Codex): `thread/tokenUsage/updated` → `Usage` per turn (fields from the 0.147.0 schema; see
      [Memory eval with subscription providers](#memory-eval-with-subscription-providers))
- [ ] Investigation: Seatbelt-wrap the whole CLI subprocess (workspace + CLI state dir writes, vendor-host network)
- [ ] Live smoke test on the Mac (Fred): `rozu auth status`, then `rozu chat --provider claude-cli "list the files and
      summarize README"`; a write and a `git push --force` requested by Claude show rozu's approval prompt
- [ ] Live smoke test on the Mac (Fred), Codex: `rozu auth status` (codex line), `rozu models` (codex list from the
      CLI), `rozu chat --provider codex "list the files and summarize README"`, then ask for a file write and a
      `git push --force` and check rozu's approval cards, Ctrl-C mid-turn, and `"provider":"codex"` in
      `approvals.jsonl`; check the unverified points in the Codex notes below
- [x] Grok (M3.5c): `grok agent stdio` ACP client on the native binary (`ROZU_GROK_BIN`, else `grok` on `PATH`
      resolved through symlinks to `grok-native`, never via node), `--no-auto-update`; agent profile with only
      `use_tool`; rozu's MCP server as the only toolset (`session/new` `mcpServers`), frozen rozu profile + `rozu__*`
      tool list in `_meta.systemPromptOverride`; `cached_token` auth only; streaming text, `use_tool` tool calls and
      results; permission requests approved only for `rozu__*` (default-deny, audited `provider: grok`);
      `session/cancel`; one session per context epoch with `GrokSession::rotate_epoch`; fixture tests with a fake
      `grok`; `rozu chat --provider grok [--model …]`
- [x] Grok usage: ACP `_meta` (total input) and headless (uncached input) normalized by one function (tests for
      both); cost `None`; capability flags on `ModelInfo` (`caps`) mark Grok's memory features as limited
- [x] Grok CLI: `rozu auth status` grok line (binary, version, login unknown), `rozu auth login grok` (official
      `grok login`, inherited stdio), `rozu models` static unverified grok list, default order claude-cli → codex →
      grok, `rozu grok-hook` (PostToolUse hook, passthrough by default), headless fallback behind `--grok-headless`
- [ ] Live smoke test on the Mac (Fred), Grok: `rozu auth login grok` (if needed), `rozu auth status` (grok line
      shows `grok-native` and its version), `rozu chat --provider grok "list the files and summarize README"`, ask
      for a file write and a `git push --force` (rozu's cards via rozu mcp-serve), Ctrl-C mid-turn then a follow-up
      turn (same process), `"provider":"grok"` in `approvals.jsonl`, usage line; check the unverified points in the
      Grok notes below

**Done when:** on the Mac, `rozu chat` works end-to-end with Claude (Max) and Codex (ChatGPT) without any API key;
a write and a destructive command requested by each CLI go through rozu's approval prompt (and an unmappable
request is denied); fixture tests cover both protocols in CI; no test or code path touches a credential file.

Notes (part 1, Claude, 2026-10-13):
- **Crates:** `rozu-cli-providers` (`CliProcess`: own process group, `kill_on_drop`, `killpg` on cancel/drop,
  JSON-lines stdout capped at 16 MiB per line, stderr only to the log file under `rozu::cli`; `ClaudeCliProvider`)
  and `rozu-mcp` (MCP server, approval bridge, Claude permission mapping). The HTTP adapters are untouched; the
  registry lists `claude-cli` (models `sonnet` (default), `opus`, `haiku`, plus any id the CLI accepts, all
  `Billing::Subscription`).
- **Agent integration:** `Provider::kind()` → `ProviderKind::AgentBackend`. `Agent::run_turn` then makes one
  stream per user turn and forwards tool calls, `StreamEvent::ToolResult` and `StreamEvent::Approval` as the same
  `AgentEvent`s the HTTP path emits; it executes nothing. Only the assistant text goes into the session (Claude
  keeps its own tool history). Tool execution was factored out of the agent into `rozu_core::ToolExecutor`, which
  both the agent and `rozu mcp-serve` use, so policy, sandbox and decision logging are identical.
- **Process:** one long-lived `claude -p` per chat (stream-json in and out, user turns as `{"type":"user",…}` lines).
  Cancel, a dropped stream or a model switch kill the process group; the next turn restarts it with
  `--resume <session_id>`. A fresh session after talking to another model gets the earlier text turns as a
  transcript. `ANTHROPIC_API_KEY`/`ANTHROPIC_AUTH_TOKEN` are removed from its environment so the subscription
  login is used. Command line:
  `claude -p --input-format stream-json --output-format stream-json --include-partial-messages --verbose
  --model <m> --permission-mode default --permission-prompt-tool mcp__rozu__approve --mcp-config <tmp>/mcp.json
  --strict-mcp-config --disallowedTools Bash,BashOutput,KillShell,Edit,MultiEdit,Write,NotebookEdit,WebFetch,WebSearch,Task
  --append-system-prompt <rozu system prompt + tool note> [--resume <id>]`, cwd = workspace.
- **Approvals:** the MCP config starts `rozu mcp-serve --workspace <ws> --approval <mode>` with
  `ROZU_APPROVAL_SOCKET` = a Unix socket in a 0700 dir under `/tmp`. mcp-serve decides with the project's
  `ApprovalPolicy`; prompts go over the socket to the parent's gate (the normal approval card), decision records
  come back for display, and every decision is logged (`rozu::approval`) in both processes' log. Socket
  missing/unreachable → deny. `approve` mapping: `mcp__rozu__*` → allow (gated when the tool runs); Read/Glob/
  Grep/LS → read (outside the workspace always asks); Bash → denied (it would run outside rozu's sandbox; use
  `mcp__rozu__shell`); Write/Edit/… → write in/outside workspace, `.rozu/` denied; WebFetch/WebSearch → network;
  anything else → denied without asking (`DecisionSource::Policy`).
- **Usage:** `input_tokens = input + cache_creation + cache_read` (rozu's convention: all prompt tokens), with
  reads/writes also carried separately; the raw `usage` and the notional `total_cost_usd` are logged under
  `rozu::usage` (M4 stores them as `usage_raw`).
- **Errors:** `CliNotFound` (binary missing; `ROZU_CLAUDE_BIN` overrides the lookup on `PATH` then
  `~/.local/bin/claude`), `NotLoggedIn` ("run `claude` and use /login"; detected in `result` text or the CLI's
  stderr when it exits early), `UsageLimit`, `Api { code: <result subtype> }`, `Decode` for protocol trouble.
- `claude-cli` is the default provider when no API key is configured (env or keychain) and neither `--provider`
  nor `--model` is given. `rozu auth status` reports the binary, `claude --version` and `claude auth status`
  (only `loggedIn`/`authMethod` are read).
- **Unverified against the real CLI** (fixtures are hand-recorded from the documented format): the exact
  stream-json input envelope (`session_id: ""`, `parent_tool_use_id: null`), the not-logged-in and usage-limit
  texts, and that MCP servers stay in Claude's process group. The live smoke test above checks these.

Notes (part 2, Codex, 2026-10-13; protocol = `codex app-server generate-json-schema` from codex 0.147.0):
- **Crate:** `rozu_cli_providers::codex` (`CodexAppServerProvider`, `protocol` (message parsing, notification →
  `StreamEvent` mapping, usage, errors), `approvals` (server requests → policy → responses), `install` (binary
  lookup, `--version`, `codex login status`, `model/list`)). Shared with Claude: `CliProcess`, `which`, `turn`
  (turn text + system prompt). The registry lists `codex` (models: `default` = Codex's configured model, plus a
  static **unverified** list; `rozu models` asks the installed CLI's `model/list` instead when it answers).
- **Process:** one long-lived app-server per chat; one thread (`thread/start`) per session; each rozu turn is a
  `turn/start` (with `model` unless it is `default`, so `/model` switches don't restart anything). Command line
  (cwd = workspace; `OPENAI_API_KEY`/`CODEX_API_KEY` removed from the environment):
  `codex -c approval_policy="untrusted" -c sandbox_mode="workspace-write"
  -c sandbox_workspace_write.network_access=false -c web_search="disabled"
  -c mcp_servers.rozu.command="<rozu>" -c mcp_servers.rozu.args=["mcp-serve", "--workspace", "<ws>", "--approval",
  "<mode>", "--session-id", "<id>", "--provider", "codex"] -c mcp_servers.rozu.env={ ROZU_APPROVAL_SOCKET = "<sock>" }
  app-server` (`sandbox_mode="read-only"` in rozu's read-only mode). Handshake: `initialize` (clientInfo `rozu`,
  no experimental API) → `initialized` → `account/read {refreshToken:false}` (only `account.type` is read; no
  account and `requiresOpenaiAuth` → `NotLoggedIn` "run `rozu auth login codex`") → `thread/start {cwd,
  approvalPolicy, sandbox, developerInstructions = rozu's system prompt + MCP tool note}`.
- **Events:** `item/agentMessage/delta` → text; `item/started`/`item/completed` for `commandExecution`
  (`codex:shell`), `fileChange` (`codex:apply_patch`), `mcpToolCall` (rozu's tools by name), `webSearch`,
  `dynamicToolCall` → tool call + result; a completed `agentMessage` without deltas → text; `turn/completed`
  → usage + done / cancelled (`interrupted`) / typed error (`failed`: `unauthorized` → not logged in,
  `usageLimitExceeded` → usage limit). Reasoning summaries are not shown (logged at debug).
- **Cancel:** `turn/interrupt {threadId, turnId}`, then wait (10 s) for `turn/completed`; approval requests
  meanwhile are answered `cancel`. If the turn doesn't end, or the stream is dropped, the app-server is killed
  and the next turn starts a new thread with the conversation so far as a transcript.
- **Approvals** (see `codex/approvals.rs` for the table): commands → `classify_command` on the command with Codex's
  `sh|bash|zsh -c|-lc '<script>'` wrapper unwrapped (card shows both); escalations (a `reason`, a
  `networkApprovalContext` host, a cwd outside the workspace) always ask; file changes → per-path write checks
  (diff from the `fileChange` item, since `item/fileChange/requestApproval` carries only the item id; `.rozu/`
  denied; delete = destructive; `grantRoot` asks); `item/permissions/requestApproval` always asks and grants only
  the requested profile for `scope: "turn"`; elicitations → `decline`; dynamic tool calls → `success: false`;
  `account/chatgptAuthTokens/refresh`, `item/tool/requestUserInput`, `attestation/generate` and unknown methods →
  JSON-RPC error -32601. Answers are always one-time (`accept`/`approved`), never `acceptForSession`,
  `approved_for_session` or policy amendments; `a` stores rozu's own rule (tool `codex:shell`), `u` is a one-time
  approval (Codex's sandbox can't be lifted by rozu; the card says so). Every decision is in `approvals.jsonl`
  with `"provider": "codex"` (also those of `rozu mcp-serve --provider codex`).
- **Not done by design:** no `chatgptAuthTokens` login, no token refresh, no reading of `~/.codex`.
- **Unverified against the real CLI** (fixtures are hand-written from the schema): that `-c` overrides placed before
  `app-server` apply to it and that the TOML values parse as written (incl. the `mcp_servers.rozu.env` inline
  table); that Codex 0.147 has no switch to disable its built-in shell/apply_patch (none is in the config schema);
  which requests `untrusted` actually produces for patches and MCP tool calls (if Codex routes approval of rozu's
  MCP tools through `mcpServer/elicitation/request`, rozu's default-deny would block them, and the mapping needs
  revisiting); the JSON-RPC envelope without a `"jsonrpc"` field; `codex login status` wording (matched: "Logged in
  using ChatGPT", "…an API key", "Not logged in") and that it prints to stderr; whether `turn/interrupt` always ends
  with `turn/completed` `interrupted`; `model/list` returning `model` ids that `turn/start` accepts; the static
  model ids and the 272K context window.

Notes (part 3, Grok Build, 2026-10-13; protocol from Grok Build 1.0.52 ACP/headless transcripts and xAI's user
guide; research: `grok-rozu-owned-loop.md` LIVE UPDATE, `grok-live-results.md`, `grok-memory-bridge.md`):
- **Crate:** `rozu_cli_providers::grok` (`GrokSession` (ACP provider), `protocol` (messages, update mapping, usage
  normalization, errors), `permissions` (agent requests → answers + audit), `install` (binary lookup, version),
  `hook` (`rozu grok-hook`), `headless` (fallback)). Registry id `grok`; models `default` (no `-m`), `grok-4.7`,
  `grok-4.7-build`, `grok-4.6` (static, **unverified**), 500K context assumed, `Billing::Subscription`,
  `caps = GROK_CAPS`.
- **Process:** one long-lived process per chat, cwd = workspace, `XAI_API_KEY` and `GROK_CLI_CHAT_PROXY_BASE_URL`
  removed from its environment (even from explicit extra env): `<grok-native> --no-auto-update agent [-m <model>]
  --agent-profile /tmp/rozu-grok-XXXX/rozu-agent.md stdio`. A model switch restarts it (`-m` is per process); the
  new process gets a new session with the conversation so far. The profile (frozen) is
  `---\nname: rozu-backend\ndescription: …\ntools: use_tool\n---\nYou are rozu's model backend.\n`.
- **Handshake:** `initialize {protocolVersion: 1, clientCapabilities: {fs: {readTextFile: false, writeTextFile:
  false}, terminal: false}, clientInfo}` → `authenticate {methodId: "cached_token"}` only if offered (no other method
  is ever picked; an error → `NotLoggedIn` "run `rozu auth login grok`") → `session/new {cwd, mcpServers: [{name:
  "rozu", command: <rozu>, args: [mcp-serve, --workspace, …, --provider, grok], env: [{name:
  ROZU_APPROVAL_SOCKET, value}]}], _meta: {systemPromptOverride: <session system messages + rozu__ tool list with
  input schemas>}}` ("Authentication required" → `NotLoggedIn`).
- **Turns:** `session/prompt {sessionId, prompt: [{type: text, text}]}`; `session/update` `agent_message_chunk` →
  text, `tool_call`/`tool_call_update` (`use_tool` `{tool_name: "rozu__x", tool_input}` → tool `x`; other tools →
  `grok:<name>`; results from `rawOutput.output.OkayOutput` or `content`) → tool call/result events; the prompt
  result → usage + done (`end_turn`, `max_turn_requests` → end turn; `max_tokens`; `refusal` → content filter;
  `cancelled`). Decisions of `rozu mcp-serve --provider grok` reach the stream over the approval socket.
- **Context epochs:** `GrokSession::rotate_epoch(EpochReason::{Masking, Compaction, ProfileRefresh, Other})` makes
  the next turn open a new session in the same process, with the profile recomputed and the request's (rewritten)
  history replayed as text ("Earlier in this conversation: …"). Otherwise turns append to the session. Nothing in
  rozu calls it yet; M4's memory layer will (at batch boundaries).
- **Cancel:** `session/cancel {sessionId}` (notification), then wait (10 s) for the prompt result; permission
  requests meanwhile are answered `cancelled`. If it doesn't end, Grok is killed and the next turn restarts it.
- **Hook:** `rozu grok-hook [--max-bytes N | --replace TEXT]` reads Grok's `PostToolUse` event and prints
  `{"hookSpecificOutput": {"hookEventName": "PostToolUse", "updatedToolOutput": …}}` only for `rozu__*` tools (never
  `search_tool`), always exit 0; no flag = passthrough. Off by default; `ROZU_GROK_HOOK_MAX_BYTES` adds it to the
  agent profile frontmatter as `hooks: {"PostToolUse": [{"matcher": "^rozu__", "hooks": [{"type": "command",
  "command": "'<rozu>' grok-hook --max-bytes N"}]}]}`. rozu never writes `~/.grok/hooks`.
- **Headless fallback** (`--grok-headless`): per turn `grok --no-auto-update -p <text> --output-format
  streaming-json [-m] --tools use_tool --disallowed-tools run_terminal_cmd,read_file,grep,list_dir,search_replace,
  web_search,web_fetch,Agent --no-subagents --no-plan --disable-web-search --allow "MCPTool(rozu__*)"
  --system-prompt-override <profile> [-r <session>]`; usage from `end` (uncached input, normalized); cancel kills the
  run. Headless has no MCP-server flag, so rozu's tools need a user-registered `rozu mcp-serve`.
- **Not done by design:** no Grok OAuth, no `x.ai/auth/*` calls, no API-key auth method, no chat proxy, no reading of
  `~/.grok`, no writes to `~/.grok` (hooks/MCP config), no bundling of the CLI.
- **Unverified against the real CLI** (fixtures are adapted from 1.0.52 transcripts; the real `grok` was not run for
  this milestone): that `authMethods` offers `cached_token` (1.0.52 signed out showed only `grok.com`; rozu then
  skips `authenticate` and relies on `session/new`); the `session/request_permission` params/options shape
  (`toolCall`, `options[].kind` = `allow_once`/`reject_once`) and whether Grok asks at all for MCP tools over ACP;
  `tool_call` field names over ACP (`title`/`kind`/`rawInput` assumed like headless); that `-m` before
  `--agent-profile` works for `grok agent`; that `session/cancel` ends the prompt with `stopReason: cancelled`;
  `hooks:` in agent-profile frontmatter (documented to exist, shape not); the static model ids and 500K window; the
  headless built-in tool names; that Grok's MCP subprocess (rozu mcp-serve) stays in Grok's process group.

**Approval-pending events + activity orb wiring (2026-10-13).**
- `AgentEvent::ApprovalPending(PendingApproval { request_id, tool, summary, provider })` is sent the moment a
  prompt waits for the user, and `AgentEvent::ApprovalResolved { request_id, resolution: Approved | Denied |
  Expired }` when it closes (before that call's `Approval` record). Never for auto-decisions: mode, rule, policy,
  delegated, or a gate that answers on its own (`ApprovalGate::asks_user()` is false for `ReadOnlyGate`, the
  non-interactive `CliGate` and `SocketGate`, which is a relay).
- Sources: `ToolExecutor::ask_gate` (native loop, Codex's own requests, blocked-retry prompts; `Expired` on
  cancel), and `ApprovalBridge` in the parent for prompts forwarded by `rozu mcp-serve` (Claude, Codex and Grok;
  `Expired` when the asker hangs up). CLI providers carry them as `StreamEvent::ApprovalPending/Resolved`; the
  bridge channel now carries `StreamEvent`s and is bound with the provider id. Codex's in-process prompts are
  yielded while the answer is awaited. Tests: core `approval_prompts_are_reported_while_open`,
  `auto_decisions_open_no_prompt`, `a_prompt_open_at_cancel_expires`, `agent_backend_prompts_are_forwarded`; MCP
  `rozu_write_tool_goes_through_the_parent_in_read_only_mode`, `withdrawn_prompts_expire`; fixtures for Claude,
  Codex (`codex_prompts_are_reported_before_the_user_answers`) and Grok.
- `rozu-activity` (no GUI deps; `rozu-core` stays GPUI-free) maps them: pending → Waiting (held while any prompt
  is open), resolved → the running tool or the state before Waiting; `TurnFailed` from `AgentError` → Error, or
  Offline for network errors. `Debouncer` paces the pill (Waiting/Done/Error/Offline/idle at once; other states
  stay up ≥300ms, coalescing). `replay::demo_turn` + `cargo run -p rozu-activity --example replay` replay a
  scripted turn; the GPUI demo in `spikes/` is untouched and can replay it later.

### M4: Sessions, memory, evals (≈2–3 weeks)
- [ ] JSONL sessions + SQLite index, resume; ROZU.md memory; compaction
- [ ] `rozu-cli eval` runner with ≥5 scripted tasks; replay tests from recorded sessions
- [ ] `rozu eval memory --condition <ID|all> --seeds N --mode live|dry`: replays scripted sessions per memory condition and logs per run: input/output tokens, cache read/write tokens, cost, latency, recall; `--max-runs-per-hour`, `--resume`, clean stop on rate limit; `cost_usd: null` for subscription providers

**Done when:** a session survives restart and resumes, a long session compacts without losing pinned memory, and the eval suite runs in CI with mocked providers. (ChatGPT login moved to M3.5.)

**Memory eval compatibility:**
- Eval conditions are named presets of the real memory/compaction settings, not separate code paths, so what the eval measures is what ships.
- The memory layer keeps a stable prompt prefix (system prompt + pinned memory + tool schemas) so it stays a cache hit across turns, and hides/elides old tool outputs in batches (not one per turn) so the prefix isn't invalidated on every request.
- Provider usage already carries `cache_read_tokens` and `cache_write_tokens` (M2 follow-up), and `ModelInfo` prices both (plus long-context tiers), so per-run cost needs no new provider work for API-key runs. `dry` mode runs against recorded/mocked providers.
- Subscription (CLI-backed) runs: tokens and cache hit rate are primary, `cost_usd` is `null`, unit cost is derived; see [Memory eval with subscription providers](#memory-eval-with-subscription-providers).

### M5: GUI (in progress: `crates/rozu-app`)
- [x] Stack: GPUI, approved by Fred 2026-10-14, with the mockup `docs/design/app/index.html` as the spec (no terminal pane, no side panel)
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
| Seatbelt (`sandbox-exec`) is deprecated by Apple | L | M | It's what Codex uses today; isolated behind the `Sandbox` trait (M3) |
| No shell sandbox on Linux yet (Landlock deferred) | M | M | Auto mode asks before unsandboxed shell writes; full mode there has no boundary beyond the classifier (interpreters/redirects/destructive still ask). Add a Landlock `Sandbox` impl before Linux builds |
| Always-on sandbox breaks tools that write caches outside the workspace (`~/.cargo`, `~/.npm`) | M | L | Explicit "blocked by rozu sandbox" note and a one-time `u` re-prompt; consider read-write cache allowlists if it's frequent in dogfooding |
| Subscription-auth terms change | M | H | Only official vendor CLIs touch subscription auth; API-key path kept; re-check terms before each release |
| Vendor CLI protocol churn (claude stream-json, codex app-server) or ToS changes for driving the CLIs from another app | H | H | Pin and check CLI versions at startup, fixture tests per protocol version, typed protocol errors with a clear "unsupported CLI version" message; keep the HTTP adapters working as the fallback; re-read each vendor's terms before each release |
| GPL code leaking into the MIT codebase (Zed terminal) | L | H | Terminal pane built only on alacritty_terminal (Apache-2.0) + portable-pty (MIT); no Zed code copied; `cargo deny` license check |
| Limited weekly time | H | M | Small slices, CLI-first so value lands before the GUI |
| Xcode license not accepted on the Mac (blocks the Metal toolchain) | H | L | Fred runs `sudo xcodebuild -license` once |

## Traceability
| Requirement | Milestone |
|---|---|
| Agent loop, streaming, cancel | M1 |
| OpenAI / Anthropic / xAI (HTTP, API key) | M1 / M2 |
| Claude CLI / Codex app-server / Grok Build ACP (subscriptions) | M3.5 |
| Tools, sandbox, approvals | M1 (read), M3 (done), M3.5 (gate for CLI tools) |
| Terminal/PTY | M3 (core), M5 (UI) |
| Keychain API keys | M1 |
| Sign in with ChatGPT | M3.5 (via codex app-server) |
| Sessions & memory | M4 |
| Eval hooks | M1 (fixtures), M4 (runner) |
| GUI | M5 (blocked on spec) |
| Release | M6 |
| Web search | Later |
