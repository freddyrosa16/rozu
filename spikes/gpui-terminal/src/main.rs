//! Throwaway GPUI spike: an embedded terminal pane.
//! portable-pty spawns $SHELL, alacritty_terminal parses the byte stream, GPUI renders the grid.
mod keys;

use std::io::{Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alacritty_terminal::event::{Event, EventListener};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Config, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, NamedColor, Processor, Rgb};
use gpui::{
    App, Application, Bounds, Context, FocusHandle, Font, FontWeight, Hsla, KeyDownEvent, Pixels,
    Rgba, StrikethroughStyle, StyledText, TextRun, UnderlineStyle, Window, WindowBounds,
    WindowOptions, div, font, prelude::*, px, size,
};
use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};

const FONT_FAMILY: &str = "Menlo";
const FONT_SIZE: f32 = 13.0;
const PADDING: f32 = 6.0;

type PtyWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Receives terminal events from inside the parser (e.g. device-attribute replies) and
/// writes the responses back to the PTY.
#[derive(Clone)]
struct Listener {
    writer: PtyWriter,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        if let Event::PtyWrite(text) = event {
            let mut w = self.writer.lock().unwrap();
            let _ = w.write_all(text.as_bytes());
            let _ = w.flush();
        }
    }
}

/// `Dimensions` impl for constructing / resizing the Term.
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }
    fn screen_lines(&self) -> usize {
        self.rows
    }
    fn columns(&self) -> usize {
        self.cols
    }
}

struct TerminalView {
    term: Arc<FairMutex<Term<Listener>>>,
    master: Box<dyn MasterPty + Send>,
    writer: PtyWriter,
    focus: FocusHandle,
    cols: usize,
    rows: usize,
    cell_w: Pixels,
    cell_h: Pixels,
    font: Font,
    bold_font: Font,
}

impl TerminalView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let (cols, rows) = (80usize, 24usize);

        // Cell metrics from the monospace font's advance width.
        let regular = font(FONT_FAMILY);
        let bold_font = Font { weight: FontWeight::BOLD, ..regular.clone() };
        let font_size = px(FONT_SIZE);
        let ts = window.text_system();
        let font_id = ts.resolve_font(&regular);
        let cell_w = ts.advance(font_id, font_size, 'm').map(|s| s.width).unwrap_or(px(7.8));
        let cell_h = px((FONT_SIZE * 1.25).round());

        // PTY + shell.
        let pty = native_pty_system()
            .openpty(PtySize { rows: rows as u16, cols: cols as u16, pixel_width: 0, pixel_height: 0 })
            .expect("openpty");
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let home = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let mut cmd = CommandBuilder::new(&shell);
        cmd.arg("-l");
        cmd.cwd(&home);
        cmd.env("TERM", "xterm-256color");
        cmd.env("COLORTERM", "truecolor");
        let mut child = pty.slave.spawn_command(cmd).expect("spawn shell");
        drop(pty.slave);

        let mut reader = pty.master.try_clone_reader().expect("pty reader");
        let writer: PtyWriter = Arc::new(Mutex::new(pty.master.take_writer().expect("pty writer")));

        let term = Arc::new(FairMutex::new(Term::new(
            Config::default(),
            &GridSize { cols, rows },
            Listener { writer: writer.clone() },
        )));

        // Reader thread: PTY bytes -> vte Processor -> Term. Pings the UI per chunk; the UI
        // drains the channel once per frame so bursts collapse into one repaint.
        let (tx, rx) = mpsc::channel::<()>();
        let exited = Arc::new(AtomicBool::new(false));
        {
            let term = term.clone();
            let exited = exited.clone();
            std::thread::spawn(move || {
                let mut parser: Processor = Processor::new();
                let mut buf = [0u8; 64 * 1024];
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            parser.advance(&mut *term.lock(), &buf[..n]);
                            if tx.send(()).is_err() {
                                break;
                            }
                        }
                    }
                }
                let _ = child.wait();
                exited.store(true, Ordering::SeqCst);
                let _ = tx.send(());
            });
        }

        cx.spawn(async move |this, cx| Self::pump(this, rx, exited, cx).await).detach();

        let focus = cx.focus_handle();
        window.focus(&focus);

        Self {
            term,
            master: pty.master,
            writer,
            focus,
            cols,
            rows,
            cell_w,
            cell_h,
            font: regular,
            bold_font,
        }
    }

    /// UI-side loop: wake ~once per frame, coalesce all pending output notifications.
    async fn pump(
        this: gpui::WeakEntity<Self>,
        rx: Receiver<()>,
        exited: Arc<AtomicBool>,
        cx: &mut gpui::AsyncApp,
    ) {
        loop {
            cx.background_executor().timer(Duration::from_millis(8)).await;
            let mut dirty = false;
            let mut disconnected = false;
            loop {
                match rx.try_recv() {
                    Ok(()) => dirty = true,
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => {
                        disconnected = true;
                        break;
                    }
                }
            }
            if exited.load(Ordering::SeqCst) || disconnected {
                let _ = cx.update(|cx| cx.quit());
                return;
            }
            if dirty && this.update(cx, |_, cx| cx.notify()).is_err() {
                return;
            }
        }
    }

    fn write(&self, bytes: &[u8]) {
        let mut w = self.writer.lock().unwrap();
        let _ = w.write_all(bytes);
        let _ = w.flush();
    }

    fn on_key(&mut self, ev: &KeyDownEvent, _w: &mut Window, cx: &mut Context<Self>) {
        let ks = &ev.keystroke;
        let m = ks.modifiers;
        if m.platform {
            if ks.key == "q" {
                cx.quit();
            }
            return; // Leave Cmd-shortcuts to the app (copy/paste not wired yet).
        }
        let app_cursor = self.term.lock().mode().contains(TermMode::APP_CURSOR);
        if let Some(bytes) = keys::key_to_bytes(
            &ks.key,
            ks.key_char.as_deref(),
            m.control,
            m.alt,
            m.shift,
            app_cursor,
        ) {
            self.write(&bytes);
            cx.stop_propagation();
        }
    }

    /// Fit the grid to the window; resizes PTY (SIGWINCH) and Term when cols/rows change.
    fn fit_to(&mut self, viewport: gpui::Size<Pixels>) {
        let avail_w = (viewport.width - px(PADDING * 2.0)).max(px(0.0));
        let avail_h = (viewport.height - px(PADDING * 2.0)).max(px(0.0));
        let cols = ((avail_w / self.cell_w).floor() as usize).max(2);
        let rows = ((avail_h / self.cell_h).floor() as usize).max(1);
        if (cols, rows) == (self.cols, self.rows) {
            return;
        }
        self.cols = cols;
        self.rows = rows;
        let _ = self.master.resize(PtySize {
            rows: rows as u16,
            cols: cols as u16,
            pixel_width: f32::from(self.cell_w * cols as f32) as u16,
            pixel_height: f32::from(self.cell_h * rows as f32) as u16,
        });
        self.term.lock().resize(GridSize { cols, rows });
    }

    /// Build one StyledText per visible row, merging adjacent same-style cells into a run.
    fn build_lines(&self) -> Vec<StyledText> {
        let term = self.term.lock();
        let grid = term.grid();
        let colors = term.colors();
        let show_cursor = term.mode().contains(TermMode::SHOW_CURSOR);
        let cursor = grid.cursor.point;
        let (rows, cols) = (grid.screen_lines(), grid.columns());

        let mut lines = Vec::with_capacity(rows);
        for row in 0..rows {
            let line = Line(row as i32);
            let cells = &grid[line];
            let mut text = String::with_capacity(cols);
            let mut runs: Vec<TextRun> = Vec::new();
            for col in 0..cols {
                let cell = &cells[Column(col)];
                if cell.flags.contains(Flags::WIDE_CHAR_SPACER) {
                    continue;
                }
                let mut fg = resolve(colors, cell.fg, cell.flags.contains(Flags::DIM));
                let mut bg = resolve(colors, cell.bg, false);
                if cell.flags.contains(Flags::INVERSE) {
                    std::mem::swap(&mut fg, &mut bg);
                }
                if show_cursor && cursor.line == line && cursor.column == Column(col) {
                    bg = resolve(colors, Color::Named(NamedColor::Cursor), false);
                    fg = resolve(colors, Color::Named(NamedColor::Background), false);
                }
                let ch = if cell.flags.contains(Flags::HIDDEN) || cell.c == '\0' { ' ' } else { cell.c };
                let start = text.len();
                text.push(ch);
                if let Some(extra) = cell.zerowidth() {
                    text.extend(extra);
                }
                let len = text.len() - start;

                let bold = cell.flags.contains(Flags::BOLD);
                let underline = cell.flags.intersects(Flags::ALL_UNDERLINES);
                let strike = cell.flags.contains(Flags::STRIKEOUT);
                let default_bg = matches!(cell.bg, Color::Named(NamedColor::Background))
                    && !cell.flags.contains(Flags::INVERSE)
                    && bg == resolve(colors, Color::Named(NamedColor::Background), false);
                let run = TextRun {
                    len,
                    font: if bold { self.bold_font.clone() } else { self.font.clone() },
                    color: hsla(fg),
                    background_color: if default_bg { None } else { Some(hsla(bg)) },
                    underline: underline.then(|| UnderlineStyle {
                        thickness: px(1.0),
                        color: Some(hsla(fg)),
                        wavy: cell.flags.contains(Flags::UNDERCURL),
                    }),
                    strikethrough: strike
                        .then(|| StrikethroughStyle { thickness: px(1.0), color: Some(hsla(fg)) }),
                };
                match runs.last_mut() {
                    Some(prev) if same_style(prev, &run) => prev.len += len,
                    _ => runs.push(run),
                }
            }
            lines.push(StyledText::new(text).with_runs(runs));
        }
        lines
    }
}

fn same_style(a: &TextRun, b: &TextRun) -> bool {
    a.font == b.font
        && a.color == b.color
        && a.background_color == b.background_color
        && a.underline == b.underline
        && a.strikethrough == b.strikethrough
}

impl Render for TerminalView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.fit_to(window.viewport_size());
        let lines = self.build_lines();
        let bg = DEFAULT_BG;
        div()
            .id("terminal")
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key))
            .size_full()
            .p(px(PADDING))
            .overflow_hidden()
            .bg(gpui::rgb(bg))
            .font_family(FONT_FAMILY)
            .text_size(px(FONT_SIZE))
            .line_height(self.cell_h)
            .whitespace_nowrap()
            .children(lines.into_iter().map(|l| div().h(self.cell_h).child(l)))
    }
}

// --- colors -------------------------------------------------------------------------------

const DEFAULT_FG: u32 = 0xd8d8d8;
const DEFAULT_BG: u32 = 0x111318;
const CURSOR: u32 = 0xe6e6e6;

/// xterm-ish 16-color palette.
const ANSI16: [u32; 16] = [
    0x1d1f21, 0xcc6666, 0xb5bd68, 0xf0c674, 0x81a2be, 0xb294bb, 0x8abeb7, 0xc5c8c6, //
    0x666666, 0xd54e53, 0xb9ca4a, 0xe7c547, 0x7aa6da, 0xc397d8, 0x70c0b1, 0xeaeaea,
];

fn rgb_u32(c: u32) -> Rgb {
    Rgb { r: (c >> 16) as u8, g: (c >> 8) as u8, b: c as u8 }
}

/// Palette lookup for indices 0..=255 (16 ANSI, 6x6x6 cube, 24 greys) plus named slots.
pub fn palette(idx: usize) -> Rgb {
    match idx {
        0..=15 => rgb_u32(ANSI16[idx]),
        16..=231 => {
            let i = idx - 16;
            let level = |v: usize| if v == 0 { 0 } else { (55 + v * 40) as u8 };
            Rgb { r: level(i / 36), g: level((i / 6) % 6), b: level(i % 6) }
        }
        232..=255 => {
            let v = (8 + (idx - 232) * 10) as u8;
            Rgb { r: v, g: v, b: v }
        }
        _ if idx == NamedColor::Foreground as usize => rgb_u32(DEFAULT_FG),
        _ if idx == NamedColor::Background as usize => rgb_u32(DEFAULT_BG),
        _ if idx == NamedColor::Cursor as usize => rgb_u32(CURSOR),
        _ if idx == NamedColor::BrightForeground as usize => rgb_u32(0xffffff),
        _ if idx == NamedColor::DimForeground as usize => dim(rgb_u32(DEFAULT_FG)),
        // DimBlack..DimWhite follow Cursor in the enum.
        _ => {
            let base = idx.saturating_sub(NamedColor::DimBlack as usize).min(7);
            dim(rgb_u32(ANSI16[base]))
        }
    }
}

fn dim(c: Rgb) -> Rgb {
    Rgb { r: (c.r as u16 * 2 / 3) as u8, g: (c.g as u16 * 2 / 3) as u8, b: (c.b as u16 * 2 / 3) as u8 }
}

/// Resolve a cell color, honoring OSC 4/10/11 overrides stored in the Term.
fn resolve(colors: &alacritty_terminal::term::color::Colors, c: Color, is_dim: bool) -> Rgb {
    let rgb = match c {
        Color::Spec(rgb) => rgb,
        Color::Indexed(i) => colors[i as usize].unwrap_or_else(|| palette(i as usize)),
        Color::Named(n) => colors[n].unwrap_or_else(|| palette(n as usize)),
    };
    if is_dim { dim(rgb) } else { rgb }
}

fn hsla(c: Rgb) -> Hsla {
    Rgba { r: c.r as f32 / 255.0, g: c.g as f32 / 255.0, b: c.b as f32 / 255.0, a: 1.0 }.into()
}

fn main() {
    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(820.), px(520.)), cx);
        cx.open_window(
            WindowOptions { window_bounds: Some(WindowBounds::Windowed(bounds)), ..Default::default() },
            |window, cx| cx.new(|cx| TerminalView::new(window, cx)),
        )
        .unwrap();
        cx.activate(true);
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn palette_cube_and_greys() {
        assert_eq!(palette(16), Rgb { r: 0, g: 0, b: 0 });
        assert_eq!(palette(231), Rgb { r: 255, g: 255, b: 255 });
        assert_eq!(palette(196), Rgb { r: 255, g: 0, b: 0 });
        assert_eq!(palette(232), Rgb { r: 8, g: 8, b: 8 });
        assert_eq!(palette(255), Rgb { r: 238, g: 238, b: 238 });
        assert_eq!(palette(NamedColor::Background as usize), rgb_u32(DEFAULT_BG));
    }
}
