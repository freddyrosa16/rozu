//! The New Chat page: the folder, the composer and a few starting ideas.

use super::*;

impl Rozu {
    pub(super) fn home_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let ideas = [
            (
                "plan",
                "Start with a plan",
                "Align on implementation before writing code",
            ),
            (
                "bug",
                "Debug an issue",
                "Find root causes and fix tricky bugs",
            ),
            ("test", "Write tests", "Cover the code you just changed"),
            (
                "diff",
                "Review my changes",
                "Check the diff before you push",
            ),
        ];
        let folder = self
            .project
            .as_deref()
            .map_or("Choose a folder".into(), name);
        let compact = self.text_w < 560.;
        let idea_w = (self.text_w - 45.).max(1.);
        let content = column()
            .w(px(self.text_w + 56.))
            .min_h(px((self.viewport_h - 44.).max(0.)))
            .justify_center()
            .gap(px(14.))
            .py(px(28.))
            .child(
                div()
                    .id("folder")
                    .w(px(self.text_w))
                    .flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .text_size(px(14.))
                    .text_color(rgb(INK2))
                    .cursor_pointer()
                    .child(icon("folder", INK2))
                    .child(div().flex_1().min_w_0().truncate().child(folder))
                    .child(icon("down", INK3))
                    .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
            )
            .child(self.composer_box(true, cx))
            .when(!self.backend.saves(), |d| {
                d.child(
                    div()
                        .w(px(self.text_w))
                        .text_size(px(14.))
                        .text_color(rgb(WARN))
                        .child("Chats aren't being saved: rozu couldn't open its sessions folder."),
                )
            })
            .child(
                div()
                    .mt_2()
                    .flex()
                    .flex_col()
                    .children(ideas.into_iter().enumerate().map(|(n, (i, title, text))| {
                        div()
                            .id(("idea", n))
                            .w(px(self.text_w))
                            .flex_none()
                            .flex()
                            .items_start()
                            .gap_3()
                            .px_2()
                            .py(px(12.))
                            .cursor_pointer()
                            .hover(|s| s.bg(rgb(RAISE)))
                            .when(n < 3, |r| r.border_b_1().border_color(rgb(LINE)))
                            .child(icon(i, INK2))
                            .child(
                                div()
                                    .w(px(idea_w))
                                    .flex()
                                    .gap_3()
                                    .when(compact, |d| d.flex_col().gap_1())
                                    .child(
                                        div()
                                            .w(px(if compact { idea_w } else { 160. }))
                                            .child(title),
                                    )
                                    .child(
                                        div()
                                            .w(px(if compact { idea_w } else { idea_w - 172. }))
                                            .text_color(rgb(INK3))
                                            .child(text),
                                    ),
                            )
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.composer
                                    .update(cx, |c, cx| c.set_text(&format!("{title}: "), cx));
                                window.focus(&this.composer.focus_handle(cx));
                            }))
                    })),
            );
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(drag_bar())
            .child(
                div()
                    .id("home")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(content),
            )
    }
}
