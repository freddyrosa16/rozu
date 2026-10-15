//! Small building blocks every page uses: rows, chips, buttons, icons, the drag bar.

use super::*;

pub(super) fn name(root: &Path) -> SharedString {
    root.file_name()
        .map_or_else(
            || root.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
        .into()
}

pub(super) fn chip(
    id: impl Into<gpui::ElementId>,
    label: impl Into<SharedString>,
    on: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .max_w_full()
        .flex_none()
        .border_1()
        .border_color(rgb(if on { INK } else { LINE }))
        .when(on, |b| b.bg(rgb(INK)).text_color(rgb(BG)))
        .rounded_full()
        .px(px(12.))
        .py(px(3.))
        .text_size(px(14.))
        .cursor_pointer()
        .child(label.into())
}

/// An icon button shown while the pointer is over its `group`.
pub(super) fn hover_icon(
    id: impl Into<gpui::ElementId>,
    group: &'static str,
    i: &'static str,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex_none()
        .rounded(px(6.))
        .p(px(2.))
        .cursor_pointer()
        .invisible()
        .group_hover(group, |s| s.visible())
        .hover(|s| s.bg(rgb(LINE)))
        .child(icon(i, INK3))
}

pub(super) fn logo(provider: &str) -> Svg {
    match provider {
        "codex" | "openai" => icon("logo-openai", INK),
        "grok" | "xai" => icon("logo-grok", INK),
        _ => icon("logo-claude", CLAUDE),
    }
}

/// A centered column as wide as the mockup's chat.
pub(super) fn column() -> Div {
    div()
        .w_full()
        .min_w_0()
        .max_w(px(760.))
        .mx_auto()
        .px(px(28.))
        .flex()
        .flex_col()
}

/// The top 44px: drags the window (its title bar is hidden).
pub(super) fn drag_bar() -> Div {
    div()
        .h(px(44.))
        .flex_none()
        .flex()
        .items_center()
        .text_size(px(14.))
        .on_mouse_down(MouseButton::Left, |_, window, _| window.start_window_move())
}

pub(super) fn heading(text: &'static str) -> Div {
    div()
        .text_color(rgb(INK3))
        .text_size(px(14.))
        .px_2()
        .pt(px(18.))
        .pb(px(6.))
        .child(text)
}

pub(super) fn row(
    id: impl Into<gpui::ElementId>,
    i: &'static str,
    label: impl Into<SharedString>,
) -> Stateful<Div> {
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_3()
        .w_full()
        .min_w_0()
        .px_2()
        .py(px(6.))
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(RAISE)))
        .child(icon(i, INK2))
        .child(div().min_w_0().truncate().child(label.into()))
}

pub(super) fn button(
    id: impl Into<gpui::ElementId>,
    label: &'static str,
    primary: bool,
) -> Stateful<Div> {
    div()
        .id(id)
        .border_1()
        .border_color(rgb(if primary { INK } else { LINE }))
        .when(primary, |b| b.bg(rgb(INK)).text_color(rgb(BG)))
        .rounded_full()
        .px(px(14.))
        .py(px(4.))
        .text_size(px(14.))
        .cursor_pointer()
        .child(label)
}

pub(super) fn icon(name: &'static str, color: u32) -> Svg {
    svg()
        .path(name)
        .size(px(17.))
        .flex_none()
        .text_color(rgb(color))
}
