//! The message box. Typing goes through the platform input handler, so accents and IME work;
//! the caret stays at the end. Enter sends, Shift-Enter starts a new line.

use std::ops::Range;

use gpui::{
    App, Bounds, Context, ElementInputHandler, EntityInputHandler, EventEmitter, FocusHandle,
    Focusable, KeyBinding, Pixels, Point, SharedString, UTF16Selection, Window, actions, canvas,
    div, prelude::*, px, rgb,
};

use crate::theme::{INK, INK3};

actions!(composer, [Submit, Newline, Backspace, Paste]);

pub fn bind_keys(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("enter", Submit, Some("Composer")),
        KeyBinding::new("shift-enter", Newline, Some("Composer")),
        KeyBinding::new("backspace", Backspace, Some("Composer")),
        KeyBinding::new("cmd-v", Paste, Some("Composer")),
    ]);
}

/// Emitted with the text when the user sends it.
pub struct Sent(pub String);

pub struct Composer {
    focus: FocusHandle,
    pub text: String,
    pub placeholder: SharedString,
    /// Width the message text wraps at, in pixels. `0` sizes to the text (the model search box).
    pub wrap_at: f32,
    /// IME composition in progress (bytes of `text`).
    marked: Option<Range<usize>>,
}

impl EventEmitter<Sent> for Composer {}

impl Focusable for Composer {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Composer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let (text, placeholder, marked) = (String::new(), SharedString::default(), None);
        Self {
            focus: cx.focus_handle(),
            text,
            placeholder,
            wrap_at: 0.,
            marked,
        }
    }

    pub fn set_text(&mut self, text: &str, cx: &mut Context<Self>) {
        self.text = text.to_owned();
        self.marked = None;
        cx.notify();
    }

    fn submit(&mut self, _: &Submit, _: &mut Window, cx: &mut Context<Self>) {
        let text = self.text.trim().to_owned();
        if !text.is_empty() && self.marked.is_none() {
            self.set_text("", cx);
            cx.emit(Sent(text));
        }
    }

    fn newline(&mut self, _: &Newline, _: &mut Window, cx: &mut Context<Self>) {
        self.text.push('\n');
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
        self.text.pop();
        cx.notify();
    }

    fn paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|c| c.text()) {
            self.text.push_str(&text);
            cx.notify();
        }
    }

    fn range(&self, utf16: Option<Range<usize>>) -> Range<usize> {
        utf16
            .map(|r| from16(&self.text, r.start)..from16(&self.text, r.end))
            .or(self.marked.clone())
            .unwrap_or(self.text.len()..self.text.len())
    }
}

impl Render for Composer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let focused = self.focus.is_focused(window);
        let empty = self.text.is_empty();
        let shown: SharedString = match (empty, focused) {
            (true, false) => self.placeholder.clone(),
            (true, true) => "▏".into(),
            (false, true) => format!("{}▏", self.text).into(),
            (false, false) => self.text.clone().into(),
        };
        let entity = cx.entity();
        let focus = self.focus.clone();
        let wrap_at = self.wrap_at;
        // `min_w_0` lets this shrink inside the bar. Without it a long line is the flex
        // item's minimum width, and the model picker is pushed out of the bar. The text
        // itself wraps at `wrap_at`: GPUI only wraps when it is given a width.
        div()
            .id("composer")
            .key_context("Composer")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::submit))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::paste))
            .on_click(cx.listener(|this, _, window, _| window.focus(&this.focus)))
            .relative()
            .w_full()
            .flex_1()
            .min_w_0()
            .min_h_6()
            .overflow_x_hidden()
            .cursor_text()
            .text_color(rgb(if empty && !focused { INK3 } else { INK }))
            .child(div().when(wrap_at > 0., |d| d.w(px(wrap_at))).child(shown))
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, (), window, cx| {
                        window.handle_input(&focus, ElementInputHandler::new(bounds, entity), cx)
                    },
                )
                .absolute()
                .size_full(),
            )
    }
}

impl EntityInputHandler for Composer {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        *adjusted = Some(range.clone());
        self.text.get(self.range(Some(range))).map(str::to_owned)
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        let end = to16(&self.text, self.text.len());
        Some(UTF16Selection {
            range: end..end,
            reversed: false,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked
            .as_ref()
            .map(|r| to16(&self.text, r.start)..to16(&self.text, r.end))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.range(range);
        self.text.replace_range(range, text);
        self.marked = None;
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = self.range(range);
        self.text.replace_range(range.clone(), text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        _: Range<usize>,
        bounds: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        Some(bounds)
    }

    fn character_index_for_point(
        &mut self,
        _: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        None
    }
}

fn to16(s: &str, byte: usize) -> usize {
    s[..byte].encode_utf16().count()
}

fn from16(s: &str, utf16: usize) -> usize {
    let mut n = 0;
    for (i, c) in s.char_indices() {
        if n >= utf16 {
            return i;
        }
        n += c.len_utf16();
    }
    s.len()
}
