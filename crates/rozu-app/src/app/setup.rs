//! The setup page: pick the AI you pay for, sign in through its CLI, then the model, effort,
//! approval mode and folder.

use super::*;

impl Rozu {
    /// Pick the AI you pay for, sign in through its CLI, pick a model, effort and folder.
    pub(super) fn setup_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let first_run = self.settings.provider.is_none();
        // The setup column is capped at 620 px with 28 px of padding on each side.
        let text_w = self.text_w.clamp(80., 564.);
        let plans: Vec<AnyElement> = PLANS
            .iter()
            .map(|plan| {
                let access = self
                    .access
                    .get(plan.id)
                    .copied()
                    .unwrap_or(Access::Checking);
                let on = self.plan == plan.id;
                div()
                    .id(plan.id)
                    .flex()
                    .items_center()
                    .gap_3()
                    .px(px(16.))
                    .py(px(12.))
                    .rounded(px(12.))
                    .border_1()
                    .border_color(rgb(if on { INK2 } else { LINE }))
                    .bg(rgb(BOX))
                    .cursor_pointer()
                    .child(logo(plan.id).size(px(22.)))
                    .child(
                        div().flex_1().flex().flex_col().child(plan.name).child(
                            div()
                                .text_size(px(14.))
                                .text_color(rgb(INK3))
                                .child(plan.plan),
                        ),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(rgb(INK2))
                            .child(access_text(access)),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.pick_plan(plan.id, cx)))
                    .into_any_element()
            })
            .collect();
        let plan = PLANS
            .iter()
            .find(|p| p.id == self.plan)
            .unwrap_or(&PLANS[0]);
        let access = self
            .access
            .get(plan.id)
            .copied()
            .unwrap_or(Access::Checking);
        let action = match access {
            Access::Missing => Some(("Install and sign in", true)),
            Access::SignedOut => Some(("Sign in", true)),
            Access::Installed => Some(("Sign in", false)),
            Access::Checking | Access::Ready => None,
        };
        let ready = matches!(access, Access::Ready | Access::Installed);
        let models: Vec<ModelChoice> = registry::provider_info(plan.id)
            .map(|p| catalog::models(p, &self.live))
            .unwrap_or_default();
        let efforts = catalog::efforts(plan.id);
        let folder = self.project.as_deref().map_or("None yet".into(), name);
        div()
            .flex_1()
            .min_w_0()
            .flex()
            .flex_col()
            .child(drag_bar())
            .child(
                div()
                    .id("setup")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        column()
                            .w(px(text_w + 56.))
                            .max_w(px(620.))
                            .gap(px(14.))
                            .py(px(24.))
                            .child(div().text_size(px(28.)).child(if first_run {
                                "Welcome to rozu"
                            } else {
                                "Your AI"
                            }))
                            .child(div().w(px(text_w)).text_color(rgb(INK2)).child(
                                "Pick the AI you already pay for. rozu works through its official \
                             app, so your plan covers it and rozu never sees your password.",
                            ))
                            .children(plans)
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .children(action.map(|(label, primary)| {
                                        let cmd = match access {
                                            Access::Missing => {
                                                format!("{} && {}", plan.install, plan.login)
                                            }
                                            _ => plan.login.to_owned(),
                                        };
                                        button("sign-in", label, primary)
                                            .on_click(move |_, _, _| catalog::in_terminal(&cmd))
                                    }))
                                    .child(
                                        button("check", "Check again", false)
                                            .on_click(cx.listener(|this, _, _, cx| this.check(cx))),
                                    ),
                            )
                            .when(action.is_some(), |d| {
                                d.child(
                                    div()
                                        .w(px(text_w))
                                        .text_size(px(14.))
                                        .text_color(rgb(INK3))
                                        .child("This opens Terminal. Finish there, then press Check again."),
                                )
                            })
                            .child(label_text("Model"))
                            .child(div().flex().flex_wrap().gap_1().children(
                                models.into_iter().map(|m| {
                                    let on = self.choice.as_ref() == Some(&m);
                                    chip(
                                        SharedString::from(format!("sm:{}", m.model)),
                                        catalog::model_name(&m.model, &self.live),
                                        on,
                                    )
                                    .on_click(
                                        cx.listener(move |this, _, _, cx| {
                                            this.choose(m.clone(), cx)
                                        }),
                                    )
                                }),
                            ))
                            .when(!efforts.is_empty(), |d| {
                                d.child(label_text("Effort"))
                                    .child(self.effort_chips(efforts, cx))
                            })
                            .child(label_text("Approvals"))
                            .child(self.approval_chips(cx))
                            .child(
                                div()
                                    .w(px(text_w))
                                    .text_size(px(14.))
                                    .text_color(rgb(INK3))
                                    .child(approval_text(self.approval)),
                            )
                            .child(label_text("Folder"))
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .child(icon("folder", INK2))
                                    .child(folder)
                                    .child(button("folder", "Choose folder", false).on_click(
                                        cx.listener(|this, _, _, cx| this.add_project(cx)),
                                    )),
                            )
                            .child(
                                div().pt_3().child(
                                    button("continue", "Continue", ready)
                                        .when(!ready, |b| b.opacity(0.5).cursor_default())
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if ready {
                                                this.finish_setup(cx);
                                            }
                                        })),
                                ),
                            ),
                    ),
            )
    }
}

pub(super) fn approval_text(mode: ApprovalMode) -> &'static str {
    APPROVAL_MODES
        .iter()
        .find(|(m, _, _)| *m == mode)
        .map_or("", |(_, _, text)| text)
}

pub(super) fn access_text(access: Access) -> &'static str {
    match access {
        Access::Checking => "Checking",
        Access::Missing => "Not installed",
        Access::SignedOut => "Not signed in",
        Access::Ready => "Ready",
        Access::Installed => "Installed",
    }
}

pub(super) fn label_text(text: &'static str) -> Div {
    div()
        .pt_2()
        .text_size(px(14.))
        .text_color(rgb(INK3))
        .child(text)
}
