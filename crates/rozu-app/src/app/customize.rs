//! Defaults for new chats, separate from a chat's current model selection.

use super::*;

impl Rozu {
    pub(super) fn customize_page(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self
            .choice
            .as_ref()
            .map_or("Choose a model".into(), |choice| {
                catalog::label(choice, self.effort.as_deref(), &self.live)
            });
        let efforts = self
            .choice
            .as_ref()
            .map_or(&[][..], |c| catalog::efforts(c.provider.id));
        let picker = div()
            .relative()
            .child(
                div()
                    .id("default-model")
                    .w_full()
                    .flex()
                    .items_center()
                    .gap_2()
                    .p_3()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(rgb(LINE))
                    .bg(rgb(BOX))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(RAISE)))
                    .child(div().flex_1().min_w_0().truncate().child(model))
                    .child(icon("down", INK2))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.providers = catalog::providers();
                        this.menu = true;
                        this.search.update(cx, |s, cx| s.set_text("", cx));
                        window.focus(&this.search.focus_handle(cx));
                        cx.notify();
                    })),
            )
            .when(self.menu, |d| {
                d.child(
                    div().absolute().top(px(46.)).left_0().child(
                        deferred(
                            anchored()
                                .anchor(Corner::TopLeft)
                                .snap_to_window_with_margin(px(8.))
                                .child(self.model_menu(cx)),
                        )
                        .with_priority(2),
                    ),
                )
            });
        div().flex_1().min_w_0().flex().flex_col()
            .child(drag_bar())
            .child(div().id("customize-scroll").flex_1().min_h_0().overflow_y_scroll()
                .child(column().w(px(self.text_w + 56.)).py_6().gap_4()
                    .child(div().text_size(px(28.)).child("Customize"))
                    .child(div().w(px(self.text_w)).text_color(rgb(INK2))
                        .child("Set the defaults for your next chat. Changes are saved automatically."))
                    .child(setup::label_text("Default model"))
                    .child(picker)
                    .when(!efforts.is_empty(), |d| d
                        .child(setup::label_text("Default effort"))
                        .child(self.effort_chips(efforts, cx)))
                    .child(setup::label_text("Approvals"))
                    .child(self.approval_chips(cx))
                    .child(div().w(px(self.text_w)).text_size(px(14.)).text_color(rgb(INK2))
                        .child(setup::approval_text(self.approval)))
                    .child(div().w(px(self.text_w)).text_size(px(14.)).text_color(rgb(INK3))
                        .child("Applies when a chat starts or reopens. Active chats keep their current approval mode."))
                    .child(setup::label_text("Connections"))
                    .child(div().flex().flex_wrap().gap_2().child(
                        button("manage-connections", "Manage AI connections", false)
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.setup(cx);
                                window.focus(&this.page_focus);
                            }))))
                    .child(setup::label_text("Chat history"))
                    .child(div().w(px(self.text_w)).text_size(px(14.))
                        .text_color(rgb(if self.backend.saves() { INK2 } else { WARN }))
                        .child(if self.backend.saves() {
                            "Chats are saved on this Mac. Use Search to find chats in your added folders."
                        } else {
                            "Chats are not being saved because rozu could not open its sessions folder."
                        }))
                ))
    }
}
