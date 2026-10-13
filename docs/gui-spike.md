# GUI stack spike: GPUI vs Tauri 2

Date: 2026-10-13. Machine: Fred's MacBook Pro (Apple Silicon, 11 cores, macOS 26.6.2), Rust 1.97.1.
Spike code: `spikes/gpui-spike`, `spikes/tauri-spike`. Both are throwaway and not workspace members.

## What was built
The same app in both stacks: a 640×420 window with a dark monospace list. A background task pushes one
"token" line every 16 ms for 2,000 lines, which stands in for a streaming LLM response.
- **GPUI 0.2.2** (crates.io, `runtime_shaders` feature): an entity with `Vec<SharedString>`, updated from
  `cx.spawn` and a background timer, with `cx.notify()` to re-render; scrollable div.
- **Tauri 2.12.3**: a Rust thread emits a `token` event; a static `dist/index.html` (no npm or bundler) batches
  lines per `requestAnimationFrame` and appends them to the DOM.

Both compiled on the first try, launched, and streamed. An embedded terminal was **not** prototyped (time-box).
The options are covered below.

## Measurements (release, `lto = "thin"`, `strip = true`)
| | GPUI | Tauri 2 |
|---|---|---|
| Clean build (cold deps) | 62.8 s | 43.1 s |
| Incremental rebuild (touch main.rs) | 7.0 s | 16.0 s |
| Binary size | 4.2 MB | 6.9 MB (+ system WebKit) |
| RSS after 5 s streaming | ~75 MB | ~101 MB app + ~40 MB WebContent process |
| Toolchain needs | Rust only | Rust (+ Node/Vite for a real frontend) |

Note: the Xcode license on this Mac hasn't been accepted, so the Xcode toolchain refuses to link. The builds used
`DEVELOPER_DIR=/Library/Developer/CommandLineTools`. Fred should run `sudo xcodebuild -license` once.
GPUI's precompiled Metal shaders need the Xcode `metal` compiler, which is why the spike uses `runtime_shaders`.

## Qualitative comparison
| Criterion | GPUI | Tauri 2 |
|---|---|---|
| Terminal embedding | Render an `alacritty_terminal` grid yourself, as Zed's terminal does (proven, but real work: ~1–2 weeks). PTY via `portable-pty` | xterm.js in the WebView, fed PTY bytes over IPC. Mature and fast to integrate. IPC throughput needs batching |
| Text rendering | GPU glyph atlas, built for editors. Excellent for long transcripts and code | WebKit text: good, and selection/copy/a11y come free. Long lists need virtualization |
| Streaming perf | Native, no IPC boundary. Batching via `notify` is trivial | Every token crosses IPC (JSON). Fine with per-frame batching |
| Maturity / docs | Pre-1.0, sparse docs, API churn, so you learn from Zed source. gpui-component (0.7, 60+ components) fills the widget gap | Stable 2.x, excellent docs, huge web ecosystem, built-in bundler/updater/signing |
| macOS support | First-class (Zed's main platform). Linux is good, Windows newer | First-class. WebView differences across OSes |
| Design-system fit | Tokens → a Rust `Theme` struct. Every primitive is hand-built or from gpui-component | Tokens → CSS variables / Tailwind v4. shadcn/Radix, cmdk, Storybook. Fastest path to pixel-matching a spec |
| Learning value for Fred | All Rust, one language end to end | Rust backend + TypeScript/React frontend (also marketable) |
| "Feels like Codex/Zed" | Yes, fully native | Close, but it's a web UI |

## Recommendation
**GPUI + gpui-component**, with Tauri 2 as the documented fallback.

Why: the product is a text-heavy, streaming, terminal-centric native app, which is exactly what GPUI was built for
(Zed). It was smaller (4.2 vs 6.9 MB), lighter (~75 vs ~140 MB), rebuilt faster incrementally (7 vs 16 s), and
keeps everything in Rust with no IPC boundary. That fits Fred's "native Rust desktop app" goal.

Costs we're accepting: pre-1.0 API churn (pin the version), doing the terminal grid ourselves (follow Zed's
`terminal_view`), and more manual work matching Designer's spec.

**Switch to Tauri 2 if** Designer's spec leans heavily on web-style components and polish has to land fast, or if
GPUI churn costs more than about a week in M5. The core crates stay UI-agnostic (channels in, events out), so
switching only affects `rozu-app`.

Decision status: **proposed**, pending Fred and Chief's review and Designer's spec. No real UI has been built.
