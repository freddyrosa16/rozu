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
1. **UI design:** blocked until Designer's spec and tokens arrive through Chief (M5).
2. **GUI stack:** spike done. The recommendation is **GPUI + gpui-component**, with Tauri 2 as the fallback; see [docs/gui-spike.md](docs/gui-spike.md). Needs Fred and Chief's sign-off before M5.
3. **Sign in with ChatGPT:** resolved by using the official Codex CLI (`codex app-server`), which does the
   ChatGPT login itself; rozu implements no OAuth of its own (M3.5).
4. Distribution: is personal-use unsigned enough, or do we need a Developer ID for notarized releases? (M6)
5. Sandbox default: should network be off by default for shell tools (Codex-style)? Proposed: yes (implemented that way in M3; revisit if it's too strict in practice).
6. **Grok subscription (HELD, Fred to decide):** xAI's official Grok Build CLI (`grok -p`, ACP via
   `grok agent stdio`) supports SuperGrok login, so a CLI-backed Grok provider is possible in principle. Hermes'
   SuperGrok path uses xAI's shared OAuth client, which isn't licensed for other apps. Until Fred decides, rozu
   implements **no Grok subscription/OAuth login of any kind**; the xAI API-key adapter remains the optional
   later path. Grok Build is not installed on the Mac.

## Requirements
**Must**
- Agent loop with streaming output, tool calling, cancel/interrupt, and turn/token caps.
- CLI-backed providers on the user's own subscriptions: Claude through the official `claude` CLI, OpenAI
  through the official Codex CLI's `codex app-server` (Sign in with ChatGPT).
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
  SuperGrok/X Premium, Hermes). Subscriptions are used only by spawning the **official vendor CLI**, which
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
│  ├─ rozu-cli-providers/  # CLI-backed providers: CliProcess, claude stream-json [M3.5], codex app-server
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
- **Tools & sandbox:** `read_file`, `list_dir`, `grep`, `apply_patch`/`write_file`, `shell` (one-shot, `tokio::process`), `terminal` (PTY). On macOS, shell commands run under a `sandbox-exec` Seatbelt profile: writes only in the workspace and tmp, network denied unless approved. On timeout or cancel the whole process group is killed.
- **Approvals:** three modes: read-only, auto-in-workspace (default) and full access. Destructive commands (`rm -rf`, `git push --force`, `git reset --hard`, branch deletion) always prompt. The approval card shows the exact command, and "always allow this prefix" is saved per project. Every decision goes to the session log.
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

**Grok:** HELD (open question 6). No Grok subscription or OAuth login is implemented. The xAI API-key HTTP
adapter remains as the optional later path.

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
- **Codex:** start app-server with config overrides for approval policy `on-request` (or `untrusted`) and
  sandbox `workspace-write` (or `read-only` when rozu's mode is read-only), cwd = the workspace. Answer each
  exec/file-change approval request from `ApprovalPolicy` (commands go through `classify_command`; file changes
  through the write-path checks); network requests follow rozu's network rule.
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
- Codex app-server reports token usage (incl. cached input) through token-count events; **verify the exact
  fields** before using it for eval runs (no cache-write accounting is expected → `cache_write_tokens: null`).
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
- [x] Sandbox tests (macOS only): writes outside the workspace and into `.rozu/` are blocked, `$TMPDIR` stays writable, `curl` to 1.1.1.1 fails without network approval
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
  `allow default` + `deny file-write*` + writes re-allowed under the workspace, `$TMPDIR`, `/tmp` and `/dev`, with
  `<workspace>/.rozu` denied again and `(deny network*)` unless the call was approved with network. `NoSandbox`
  elsewhere. **Linux Landlock is deferred**; without a sandbox, auto mode asks before shell commands that write.
  Known Seatbelt limits: `allow default` still permits mach/XPC services; tools that write to `~/.cargo` or
  similar caches outside the workspace fail under the sandbox (approve with network, or use full access).
- **Risk classifier** (`classify_command`): splits on `&&`, `||`, `;`, `|`, `&`, newlines, parentheses and
  backticks, recurses into `sh -c`/`bash -c`/`eval`/`xargs`/`find -exec`, takes the max. Unknown commands are
  `WriteWorkspace`; redirects or `tee` to paths outside the workspace/temp are `Destructive`.
- **Approvals** (`rozu-core::approval`): `ApprovalMode::{ReadOnly, AutoWorkspace (default), FullAccess}`.
  Full access runs shell unsandboxed; an approved `WriteOutside` shell call also runs unsandboxed (it couldn't work
  otherwise). "Always allow" stores `{tool, prefix}` in `.rozu/approvals.json` (shell prefix = program plus
  subcommand, e.g. `cargo test`; every segment of a compound command must match a rule; rules never apply to
  destructive calls or in read-only mode). Every decision is an `AgentEvent::Approval` and a `tracing` event
  (target `rozu::approval`); M4 writes them to the session log.
- **PTY:** `rozu_pty::PtySession` (spawn, write, resize, raw byte subscription, `snapshot()`/`screen_text()` of the
  visible grid, exit status). `rozu term` proxies `$SHELL` through it in raw mode (crossterm). The agent-facing
  `terminal_read`/`terminal_write` tools come with the GUI terminal pane.
- Open question 5 (network off by default for shell) is implemented as proposed: off unless approved.
- QA fixes (2026-10-13): a fast double Ctrl-C no longer hangs (signals are counted, never coalesced, and the
  exit path doesn't wait on locks or the turn); tracing logs go to `~/Library/Logs/rozu/rozu.log` (data dir
  elsewhere) instead of the terminal, `ROZU_LOG_STDERR=1` brings stderr logging back; `--max-steps` must be
  1..=200; the model table was refreshed from the official pricing pages and models gained long-context price
  tiers.

### M3.5: CLI-backed providers (≈2–3 weeks) — Claude CLI first, then Codex app-server
See [Providers](#providers-http-adapters-and-cli-backed-providers).
- [x] `CliProcess` plumbing: spawn the official binary (path lookup; `--version` shown by `rozu auth status`),
      process-group kill on cancel/drop, stderr to the log file, typed errors (not installed, not logged in,
      rate/usage limit, protocol)
- [x] Claude: stream-json transport and event mapping (deltas, tool_use/tool_result, result usage); fixture tests
      with a fake `claude` binary; `rozu chat --provider claude-cli`
- [x] rozu stdio MCP server (`rozu mcp-serve`): rozu's tools + the `approve` permission-prompt tool, all through
      `ApprovalPolicy`; Claude launched with `--mcp-config --strict-mcp-config --permission-prompt-tool` and its
      built-in mutating tools disallowed
- [ ] Codex: `codex app-server` JSON-RPC client (initialize, thread/turn, item events, token usage), approval
      requests answered by `ApprovalPolicy` (default-deny for unmapped requests); sandbox/approval config overrides
- [ ] Sign in with ChatGPT via `codex app-server` (account status, prompt the user to run `codex login`); no
      custom OAuth, no reading of Codex's credential files
- [x] Usage (Claude): `result.usage` → `Usage`; `cost_usd` is `None` for subscription models (`Billing::Subscription`);
      the CLI's `total_cost_usd` is only logged (target `rozu::usage`) with the raw usage
- [ ] Usage (Codex): token-count events → `Usage` (verify the fields first)
- [ ] Investigation: Seatbelt-wrap the whole CLI subprocess (workspace + CLI state dir writes, vendor-host network)
- [ ] Live smoke test on the Mac (Fred): `rozu auth status`, then `rozu chat --provider claude-cli "list the files and
      summarize README"`; a write and a `git push --force` requested by Claude show rozu's approval prompt

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
  Grep/LS → read (outside the workspace always asks); Bash → rozu's shell classifier, unsandboxed (destructive
  always asks); Write/Edit/… → write in/outside workspace, `.rozu/` denied; WebFetch/WebSearch → network;
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
| Seatbelt (`sandbox-exec`) is deprecated by Apple | L | M | It's what Codex uses today; isolated behind the `Sandbox` trait (M3) |
| No shell sandbox on Linux yet (Landlock deferred) | M | M | Auto mode asks before unsandboxed shell writes; add a Landlock `Sandbox` impl before Linux builds |
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
| Claude CLI / Codex app-server (subscriptions) | M3.5 |
| Tools, sandbox, approvals | M1 (read), M3 (done), M3.5 (gate for CLI tools) |
| Terminal/PTY | M3 (core), M5 (UI) |
| Keychain API keys | M1 |
| Sign in with ChatGPT | M3.5 (via codex app-server) |
| Sessions & memory | M4 |
| Eval hooks | M1 (fixtures), M4 (runner) |
| GUI | M5 (blocked on spec) |
| Release | M6 |
| Web search | Later |
