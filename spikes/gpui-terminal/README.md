# gpui-terminal (throwaway spike)

A minimal embedded terminal pane built on GPUI 0.2: `portable-pty` spawns the shell,
`alacritty_terminal` 0.26 parses VT output, and GPUI draws the grid. This crate is standalone
(it has an empty `[workspace]` table) and isn't part of the main build.

## Run

```sh
cd spikes/gpui-terminal
cargo run --release      # opens an 820x520 window running $SHELL -l in $HOME
cargo test               # key->bytes mapping + palette tests
```

To quit, press Cmd-Q or exit the shell (`exit` / Ctrl-D).

## How it works

- **PTY**: `native_pty_system().openpty`, `$SHELL -l` (fallback `/bin/zsh`), `cwd=$HOME`,
  `TERM=xterm-256color`, `COLORTERM=truecolor`.
- **Parsing**: a reader thread reads up to 64 KiB at a time and passes it to
  `vte::ansi::Processor::advance` on a `FairMutex<Term>`. Replies the terminal generates
  (`Event::PtyWrite`, e.g. DA/DSR answers) go back to the PTY through the `EventListener`.
- **Repaint batching**: the reader sends `()` on an mpsc channel for each chunk. A `cx.spawn`
  loop wakes every 8 ms, drains the channel, and calls `cx.notify()` at most once per tick.
- **Rendering**: each visible row becomes one `StyledText`, and adjacent cells with the same
  style are merged into one `TextRun`. Each run carries fg, bg, bold, underline and
  strikethrough. Colors cover the 16 ANSI colors, the 256-color cube and greys, truecolor, and
  OSC 4/10/11 overrides. DIM and INVERSE are handled. The block cursor is drawn by swapping
  colors on the cursor cell.
- **Resize**: in each `render`, the viewport size divided by the cell size (Menlo advance
  width for `m`, line height `round(13 × 1.25)`) gives cols/rows. When these change, the code
  calls `MasterPty::resize`, which sends SIGWINCH, and then `Term::resize`.
- **Input**: `keys::key_to_bytes` handles printable characters (via `key_char`), Enter,
  Backspace (DEL), Tab, Shift-Tab, Esc, arrows (with DECCKM app-cursor mode), Home/End,
  PgUp/PgDn, Delete, Ctrl+letter and Ctrl+symbols. Option acts as Meta (ESC prefix).

## What works (expected)

zsh/bash prompts, colored `ls`, `git log --color`, `vim`/`htop`/`less` (alt screen, app
cursor), 256-color and truecolor test scripts, and resizing with reflow.

These were only compile- and unit-tested. The window wasn't launched during the spike, so
check it by hand.

## Known gaps

| Area | State |
|---|---|
| Scrollback | The Term keeps 10k lines of history, but there's no wheel or keyboard scrolling and `display_offset` is ignored when rendering. |
| Selection / copy-paste | None. Cmd-C/Cmd-V aren't wired, and there's no bracketed-paste handling. |
| Mouse | No mouse reporting (SGR 1006 etc.), clicks, or focus-in/out reports. |
| IME / dead keys | Uses `on_key_down` only, with no `InputHandler`, so composition (CJK, accents via dead keys) won't work. |
| Wide chars / emoji | Spacer cells are skipped, but glyphs aren't forced onto the cell grid, so a fallback font with a different advance can drift the rest of the row. |
| Glyph grid alignment | Relies on Menlo's advance matching `cell_w`. A real implementation should place each run at `col × cell_w` (custom `Element`) instead of letting text layout flow. |
| Cursor | Block only. Ignores DECSCUSR shapes (beam/underline), blinking, and the hollow cursor when unfocused. |
| Perf | Every repaint re-lays out all rows, there's no damage tracking (`Term::damage`), and the Term lock is held during the whole row build. It's fine for a shell but will struggle with `cat huge.log` or `yes`. |
| Sync updates (mode 2026) | `Processor` buffers synchronized updates, but its sync timeout is never polled. |
| Other | No title/bell handling, hyperlinks (OSC 8), underline colors, bold-as-bright, configurable font/theme, multiple panes, or child-exit status UI. The app quits when the shell exits. Fixed 8 ms poll instead of a waker. |

## Estimate for a production terminal pane

Assumes one engineer who knows GPUI, building a Zed-terminal-level pane for an embedded
use case.

| Work | Estimate |
|---|---|
| Custom grid `Element` (absolute run placement, bg rects, damage-aware relayout, wide-char/emoji handling, cursor shapes) | 1–1.5 wk |
| Scrollback (wheel, keyboard, scrollbar, alt-screen rules) | 2–3 d |
| Selection, copy/paste, bracketed paste, word/line selection | 3–5 d |
| Mouse reporting + focus events | 2–3 d |
| IME via `InputHandler`, full keyboard (F-keys, modifyOtherKeys / kitty protocol) | 3–5 d |
| Event loop hardening (waker instead of polling, sync-update timeouts, back-pressure, child lifecycle) | 2–3 d |
| Theming, font config, title/bell/hyperlinks, search | 3–5 d |
| Testing (vttest, esctest, perf benchmarks, macOS/Linux) | 1 wk |
| **Total** | **≈ 5–7 weeks** |

Zed's `terminal` and `terminal_view` crates solve all of this on the same stack (GPUI +
alacritty_terminal) and are the best reference. Check their license (GPL) before copying
code.
