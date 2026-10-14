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
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(drag_bar())
            .child(
                column()
                    .flex_1()
                    .justify_center()
                    .gap(px(14.))
                    .pb(px(44.))
                    .child(
                        div()
                            .id("folder")
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(14.))
                            .text_color(rgb(INK2))
                            .cursor_pointer()
                            .child(icon("folder", INK2))
                            .child(folder)
                            .child(icon("down", INK3))
                            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                    )
                    .child(self.composer_box(true, cx))
                    .when(!self.backend.saves(), |d| {
                        d.child(div().text_size(px(14.)).text_color(rgb(WARN)).child(
                            "Chats aren't being saved: rozu couldn't open its sessions folder.",
                        ))
                    })
                    .child(div().mt_2().flex().flex_col().children(
                        ideas.into_iter().enumerate().map(|(n, (i, title, text))| {
                            row(("idea", n), i, title)
                                .rounded_none()
                                .py(px(12.))
                                .when(n < 3, |r| r.border_b_1().border_color(rgb(LINE)))
                                .child(div().text_color(rgb(INK3)).child(text))
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.composer
                                        .update(cx, |c, cx| c.set_text(&format!("{title}: "), cx));
                                    window.focus(&this.composer.focus_handle(cx));
                                }))
                        }),
                    )),
            )
    }
}
