//! The composer with its model picker: a Cursor-style menu with search, the models grouped by
//! provider, and the effort chips; plus the approval-mode chips the setup page shares.

use super::*;

impl Rozu {
    pub(super) fn composer_box(&self, home: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let running = self.running();
        let model = self.choice.as_ref().map_or("No model".into(), |c| {
            catalog::label(c, self.effort.as_deref(), &self.live)
        });
        let picker = div()
            .relative()
            .flex_none()
            .max_w(px(240.))
            .min_w_0()
            .child(
                div()
                    .id("model")
                    .flex()
                    .items_center()
                    .gap_1()
                    .min_w_0()
                    .text_size(px(14.))
                    .text_color(rgb(INK2))
                    .cursor_pointer()
                    .child(div().min_w_0().truncate().child(model))
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
                    div().absolute().top(px(-8.)).right_0().child(
                        deferred(
                            anchored()
                                .anchor(Corner::BottomRight)
                                .snap_to_window_with_margin(px(8.))
                                .child(self.model_menu(cx)),
                        )
                        .with_priority(2),
                    ),
                )
            });
        let go = div()
            .id("go")
            .size(px(34.))
            .flex_none()
            .rounded_full()
            .bg(rgb(INK))
            .flex()
            .items_center()
            .justify_center()
            .cursor_pointer()
            .child(icon(if running { "stop" } else { "up" }, BG))
            .on_click(cx.listener(move |this, _, window, cx| {
                if running {
                    this.stop(cx);
                } else {
                    let composer = this.composer.clone();
                    window.dispatch_action(Box::new(composer::Submit), cx);
                    window.focus(&composer.focus_handle(cx));
                }
            }));
        // A little under the bar's inner width, so the text wraps before it meets the edge.
        let wrap_at = (self.text_w - 36.).max(80.);
        self.composer.update(cx, |composer, cx| {
            if (composer.wrap_at - wrap_at).abs() > 0.5 {
                composer.wrap_at = wrap_at;
                cx.notify();
            }
        });
        let text = div()
            .id("composer-text")
            .w_full()
            .min_w_0()
            .max_h(px(160.))
            .overflow_y_scroll()
            .overflow_x_hidden()
            .child(self.composer.clone());
        let controls = div()
            .flex()
            .items_center()
            .gap_3()
            .child(div().flex_1())
            .child(picker)
            .child(go);
        let shell = div()
            .w_full()
            .min_w_0()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(BOX));
        if home {
            shell
                .rounded(px(18.))
                .p(px(14.))
                .flex()
                .flex_col()
                .gap_3()
                .child(text.min_h(px(52.)))
                .child(controls)
        } else {
            // Text on its own row, model and send underneath, so a long message wraps inside
            // the bar instead of pushing the model out of it.
            shell
                .rounded(px(22.))
                .pt(px(10.))
                .pb(px(8.))
                .pl(px(16.))
                .pr(px(8.))
                .flex()
                .flex_col()
                .gap_2()
                .child(text.min_h(px(28.)))
                .child(controls)
        }
    }

    /// Every model the menu can offer that matches `query`, grouped by provider.
    pub(super) fn matches(&self, query: &str) -> Vec<ModelChoice> {
        let query = query.trim().to_lowercase();
        self.providers
            .iter()
            .flat_map(|p| catalog::models(p, &self.live))
            .filter(|m| {
                let name = format!(
                    "{} {} {}",
                    catalog::provider_name(m.provider),
                    catalog::model_name(&m.model, &self.live),
                    m.model
                );
                query.is_empty() || name.to_lowercase().contains(&query)
            })
            .collect()
    }

    /// The model menu, like Cursor's: a search box, a scrolling list grouped by provider, and
    /// the effort for the chosen model. Kept inside the window.
    pub(super) fn model_menu(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.search.read(cx).text.clone();
        let models = self.matches(&query);
        let mut list = div()
            .id("model-list")
            .flex_1()
            .min_h_0()
            .overflow_y_scroll()
            .p_1()
            .flex()
            .flex_col();
        let mut last = "";
        for m in models.iter().cloned() {
            if m.provider.id != last {
                last = m.provider.id;
                list = list.child(menu_heading(catalog::provider_name(m.provider)));
            }
            let on = self.choice.as_ref() == Some(&m);
            let key = SharedString::from(format!("m:{}", m.spec()));
            let label = catalog::model_name(&m.model, &self.live);
            list = list.child(
                div()
                    .id(key)
                    .flex()
                    .items_center()
                    .px_2()
                    .py(px(4.))
                    .rounded(px(6.))
                    .text_size(px(13.))
                    .cursor_pointer()
                    .hover(|s| s.bg(rgb(LINE)))
                    .when(on, |r| r.bg(rgb(LINE)))
                    .child(div().flex_1().truncate().child(label))
                    .when(on, |r| r.child(icon("check", INK)))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose(m.clone(), cx))),
            );
        }
        if models.is_empty() {
            list = list.child(div().p_2().text_size(px(13.)).text_color(rgb(INK3)).child(
                if self.providers.is_empty() {
                    "No AI is set up. Click the gear to pick one."
                } else {
                    "No models match."
                },
            ));
        }
        let efforts = self
            .choice
            .as_ref()
            .map_or(&[][..], |c| catalog::efforts(c.provider.id));
        div()
            .id("model-menu")
            .occlude()
            .w(px(300.))
            .max_h(px((self.viewport_h - 120.).clamp(160., 420.)))
            .bg(rgb(RAISE))
            .border_1()
            .border_color(rgb(LINE))
            .rounded(px(12.))
            .shadow_lg()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(rgb(LINE))
                    .text_size(px(13.))
                    .child(icon("search", INK3))
                    .child(self.search.clone()),
            )
            .child(list)
            .when(!efforts.is_empty(), |d| {
                d.child(
                    div()
                        .border_t_1()
                        .border_color(rgb(LINE))
                        .px_2()
                        .py_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_size(px(12.))
                                .text_color(rgb(INK3))
                                .child("Effort"),
                        )
                        .child(self.effort_chips(efforts, cx)),
                )
            })
    }

    pub(super) fn effort_chips(
        &self,
        efforts: &'static [&'static str],
        cx: &mut Context<Self>,
    ) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .child(
                chip("effort-default", "Default", self.effort.is_none())
                    .on_click(cx.listener(|this, _, _, cx| this.set_effort(None, cx))),
            )
            .children(efforts.iter().map(|&e| {
                chip(
                    SharedString::from(format!("effort-{e}")),
                    e,
                    self.effort.as_deref() == Some(e),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_effort(Some(e.into()), cx)))
            }))
    }

    pub(super) fn approval_chips(&self, cx: &mut Context<Self>) -> Div {
        div()
            .flex()
            .flex_wrap()
            .gap_1()
            .children(APPROVAL_MODES.iter().map(|&(mode, label, _)| {
                chip(
                    SharedString::from(format!("approval-{}", catalog::approval_arg(mode))),
                    label,
                    self.approval == mode,
                )
                .on_click(cx.listener(move |this, _, _, cx| this.set_approval(mode, cx)))
            }))
    }
}

pub(super) fn menu_heading(text: &'static str) -> Div {
    div()
        .px_2()
        .pt_2()
        .pb_1()
        .text_size(px(13.))
        .text_color(rgb(INK3))
        .child(text)
}
