//! A chat: the user's messages, the agent's Markdown replies, a row per tool step and a card per
//! approval prompt.

use super::*;

impl Rozu {
    pub(super) fn thread_page(
        &mut self,
        id: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let Some(thread) = self.thread_ref(id) else {
            return div().flex_1();
        };
        let steps: Vec<bool> = thread
            .feed
            .items
            .iter()
            .map(|item| matches!(item, Item::Step(_)))
            .collect();
        let items: Vec<AnyElement> = (0..steps.len())
            .map(|i| {
                let gap = match i {
                    0 => 0.,
                    _ if steps[i] && steps[i - 1] => 4.,
                    _ => 16.,
                };
                div()
                    .w_full()
                    .min_w_0()
                    .mt(px(gap))
                    .child(self.item(id, i, window, cx))
                    .into_any_element()
            })
            .collect();
        let title = self
            .thread_ref(id)
            .map(|t| t.title.clone())
            .unwrap_or_default();
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                drag_bar().px(px(20.)).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_color(rgb(INK3))
                        .child(title),
                ),
            )
            .child(
                div()
                    .id("feed")
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .map(|mut feed| {
                        feed.style().restrict_scroll_to_axis = Some(true);
                        feed
                    })
                    .track_scroll(&self.scroll)
                    .child(
                        column()
                            .w(px(self.text_w + 56.))
                            .pt(px(8.))
                            .pb(px(24.))
                            .children(items),
                    ),
            )
            .child(
                column()
                    .flex_none()
                    .pb(px(16.))
                    .child(self.composer_box(false, cx)),
            )
    }

    pub(super) fn item(
        &self,
        id: usize,
        i: usize,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(thread) = self.thread_ref(id) else {
            return div().into_any_element();
        };
        match &thread.feed.items[i] {
            Item::User(text) => div()
                .border_1()
                .border_color(rgb(LINE))
                .bg(rgb(BOX))
                .rounded(px(14.))
                .px(px(18.))
                .py(px(12.))
                .w(px(self.text_w))
                .child(div().w(px((self.text_w - 38.).max(1.))).child(text.clone()))
                .into_any_element(),
            Item::Text(text) => div()
                .id(("reply", i))
                .child(rich(text, self.text_w, window))
                .into_any_element(),
            Item::Note(text) => div()
                .w(px(self.text_w))
                .text_color(rgb(INK3))
                .text_size(px(14.))
                .child(text.clone())
                .into_any_element(),
            Item::Step(step) => step_row(i, step, self.text_w).into_any_element(),
            Item::Ask(ask) => self.ask_card(id, i, ask, cx).into_any_element(),
        }
    }

    pub(super) fn ask_card(
        &self,
        id: usize,
        i: usize,
        ask: &Ask,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let req = &ask.request;
        let title = match req.tool.as_str() {
            "shell" => "Run a command?".to_owned(),
            t if t.contains("patch") || t.contains("write") => "Edit a file?".to_owned(),
            t => format!(
                "Use {}?",
                rozu_runtime::approval_text(t).replace('\n', "\\n")
            ),
        };
        let preview = rozu_runtime::approval_text(&req.preview);
        let options = rozu_runtime::ApprovalOptions::for_request(req);
        let mut choices = Vec::new();
        if options.approve {
            choices.push(("Allow once", Decision::Approve, true));
        }
        if options.always_allow {
            choices.push(("Always allow", Decision::AlwaysAllowPrefix, false));
        }
        if options.unsandboxed {
            choices.push((
                rozu_runtime::unsandboxed_action_label(req),
                Decision::ApproveUnsandboxed,
                false,
            ));
        }
        choices.push((
            "Deny",
            Decision::Deny {
                reason: "denied by the user".into(),
            },
            false,
        ));
        // Same fixed width as the other messages: inside the scrolling feed GPUI offers text a
        // width wider than the column, so a long command or the button row runs off the window.
        let text_w = self.text_w.max(1.);
        let inner = (text_w - 34.).max(1.);
        div()
            .w(px(text_w))
            .max_w_full()
            .min_w_0()
            .overflow_hidden()
            .border_1()
            .border_color(rgb(LINE))
            .bg(rgb(BOX))
            .rounded(px(12.))
            .px(px(16.))
            .py(px(12.))
            .flex()
            .flex_col()
            .gap(px(10.))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .min_w_0()
                    .child(icon("ask", if ask.answer.is_none() { WARN } else { INK3 }))
                    .child(div().min_w_0().w(px((inner - 28.).max(1.))).child(title)),
            )
            .child(
                div()
                    .id(("ask-preview", i))
                    .w(px(inner))
                    .max_h(px((self.viewport_h * 0.3).clamp(80., 168.)))
                    .overflow_y_scroll()
                    .overflow_x_hidden()
                    .bg(rgb(BG))
                    .rounded(px(8.))
                    .child(
                        div()
                            .w(px(inner))
                            .px(px(10.))
                            .py(px(8.))
                            .font_family("Menlo")
                            .text_size(px(13.))
                            .child(div().w(px((inner - 20.).max(1.))).child(preview)),
                    ),
            )
            .child(
                div()
                    .w(px(inner))
                    .text_color(rgb(INK3))
                    .text_size(px(14.))
                    .child(confinement(req)),
            )
            .child(match ask.answer {
                Some(answer) => div()
                    .w(px(inner))
                    .text_color(rgb(INK3))
                    .text_size(px(14.))
                    .child(answer),
                None => div().w(px(inner)).flex().flex_wrap().gap_2().children(
                    choices
                        .into_iter()
                        .enumerate()
                        .map(|(n, (label, decision, primary))| {
                            button(("ask", n), label, primary)
                                .max_w(px(inner))
                                .flex_none()
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.answer(id, i, decision.clone(), cx)
                                }))
                        }),
                ),
            })
    }
}

pub(super) fn step_row(i: usize, step: &Step, w: f32) -> Div {
    let (name, color) = match (&step.done, step.failed) {
        (Some(_), true) => ("fail", BAD),
        (Some(_), false) => ("check", INK3),
        (None, _) => match step.state {
            ActivityState::Reading => ("read", INK2),
            ActivityState::Searching => ("search", INK2),
            ActivityState::Editing => ("edit", INK2),
            ActivityState::Running => ("run", INK2),
            ActivityState::Waiting => ("ask", WARN),
            _ => ("think", INK2),
        },
    };
    let mark = icon(name, color).size(px(15.));
    let line = div()
        .w(px(w))
        .flex()
        .items_start()
        .gap_2()
        .text_color(rgb(INK2))
        .child(match step.done {
            Some(_) => mark.into_any_element(),
            None => mark
                .with_animation(
                    ("step", i),
                    Animation::new(Duration::from_millis(1600))
                        .repeat()
                        .with_easing(pulsating_between(0.3, 1.)),
                    |s, a| s.opacity(a),
                )
                .into_any_element(),
        });
    let text = div().w(px((w - 23.).max(1.))).min_w_0();
    match &step.done {
        None => line.child(text.child(step.label.clone())),
        Some(done) if step.failed => line.child(text.text_color(rgb(BAD)).child(done.clone())),
        Some(done) => {
            let verb = done.split_once(' ').map_or(done.as_str(), |(verb, _)| verb);
            line.child(text.text_color(rgb(INK3)).child(
                StyledText::new(done.clone()).with_highlights([(
                    0..verb.len(),
                    HighlightStyle {
                        color: Some(rgb(INK2).into()),
                        ..Default::default()
                    },
                )]),
            ))
        }
    }
}

/// The agent's reply, Markdown drawn as paragraphs, headings, lists and code blocks.
pub(super) fn rich(text: &str, w: f32, window: &Window) -> Div {
    let blocks = markdown::parse(text);
    let marker_w = blocks
        .iter()
        .filter_map(|block| match block {
            Block::Item(marker, _) => Some(marker.chars().count() as f32 * 9.),
            _ => None,
        })
        .fold(14., f32::max);
    div()
        .w(px(w))
        .flex()
        .flex_col()
        .gap(px(10.))
        .line_height(relative(1.6))
        .children(blocks.into_iter().enumerate().map(|(i, block)| {
            match block {
                Block::Para(t) => div().w(px(w)).child(spans(t)),
                Block::Heading(t) => div()
                    .w(px(w))
                    .pt(px(6.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(spans(t)),
                Block::Item(marker, t) => div()
                    .w(px(w))
                    .flex()
                    .gap(px(10.))
                    .child(
                        div()
                            .flex_none()
                            .w(px(marker_w))
                            .whitespace_nowrap()
                            .text_right()
                            .text_color(rgb(INK3))
                            .child(marker),
                    )
                    .child(div().w(px((w - marker_w - 10.).max(0.))).child(spans(t))),
                Block::Code(code) => div().w(px(w)).child(
                    div()
                        .id(("code", i))
                        .w(px(w))
                        .overflow_x_scroll()
                        .map(|mut code| {
                            code.style().restrict_scroll_to_axis = Some(true);
                            code
                        })
                        .bg(rgb(CODE_BG))
                        .border_1()
                        .border_color(rgb(LINE))
                        .rounded(px(10.))
                        .child(
                            div()
                                .w(px(code_content_width(&code, w, window)))
                                .px(px(14.))
                                .py(px(10.))
                                .font_family("Menlo")
                                .text_size(px(13.))
                                .line_height(relative(1.5))
                                .text_color(rgb(INK2))
                                .whitespace_nowrap()
                                .child(code),
                        ),
                ),
            }
        }))
}

fn code_content_width(code: &str, viewport_w: f32, window: &Window) -> f32 {
    let longest = code
        .lines()
        .map(|line| {
            let run = gpui::TextRun {
                len: line.len(),
                font: gpui::font("Menlo"),
                color: rgb(INK2).into(),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            f32::from(
                window
                    .text_system()
                    .shape_line(line.to_owned().into(), px(13.), &[run], None)
                    .width,
            )
        })
        .fold(0., f32::max);
    (viewport_w - 2.).max(longest.ceil() + 28.)
}

pub(super) fn spans(t: markdown::Inline) -> StyledText {
    let styles: Vec<_> = t
        .spans
        .into_iter()
        .map(|(r, s)| {
            let mut h = HighlightStyle::default();
            if s.bold {
                h.font_weight = Some(FontWeight::SEMIBOLD);
            }
            if s.code {
                h.color = Some(rgb(INK).into());
                h.background_color = Some(rgb(RAISE).into());
            }
            (r, h)
        })
        .collect();
    StyledText::new(t.text).with_highlights(styles)
}

/// How an approved call would run.
pub(super) fn confinement(req: &ApprovalRequest) -> String {
    let codex = req.sandbox == rozu_core::SandboxMode::Enforced(rozu_core::CODEX_SANDBOX);
    let network = if req.network { "on" } else { "off" };
    let mut text = if codex {
        format!("Sandbox: Codex's own (rozu cannot remove it). Network requested: {network}.")
    } else if req.sandbox == rozu_core::SandboxMode::Off {
        "No OS sandbox: this call is not confined to workspace files or blocked from the network."
            .into()
    } else {
        format!(
            "Sandbox: {}. Network {network}.",
            rozu_runtime::approval_text(&req.sandbox.to_string())
        )
    };
    if req.blocked_retry {
        text.push_str("\nThe sandbox blocked the previous attempt.");
    }
    for path in &req.extra_writes {
        let path = rozu_runtime::approval_text(&path.to_string_lossy()).replace('\n', "\\n");
        if codex {
            text.push_str(&format!(
                "\nRequested write path (Codex's permissions still apply): {path}"
            ));
        } else {
            text.push_str(&format!("\nAlso writable for this call: {path}"));
        }
    }
    if req.outside_unknown {
        text.push_str(
            "\nThis call may write outside the workspace at paths rozu cannot determine.",
        );
        if req.can_run_unsandboxed() {
            text.push_str(" Those writes may be blocked by the sandbox.");
        }
    }
    if req.can_run_unsandboxed() && !codex {
        text.push_str("\nRun without the sandbox gives this command full access to your files and network, for this call only.");
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use rozu_core::{CODEX_SANDBOX, SandboxMode};
    use rozu_tools::Risk;

    fn request(sandbox: SandboxMode) -> ApprovalRequest {
        ApprovalRequest {
            call_id: "call".into(),
            tool: "shell".into(),
            args: Default::default(),
            risk: Risk::WriteOutside,
            preview: String::new(),
            sandbox,
            network: false,
            extra_writes: Vec::new(),
            outside_unknown: false,
            blocked_retry: false,
            allow_rule: None,
        }
    }

    #[test]
    fn confinement_includes_every_path_and_unknown_write_scope() {
        let mut req = request(SandboxMode::Enforced("seatbelt"));
        req.extra_writes = vec!["/one".into(), "/two\nnetwork on\u{1b}[2J".into()];
        req.outside_unknown = true;
        let text = confinement(&req);
        assert!(text.contains("Also writable for this call: /one"));
        assert!(text.contains("Also writable for this call: /two\\nnetwork on"));
        assert!(!text.contains('\u{1b}'));
        assert!(!text.contains("/two\nnetwork on"));
        assert!(text.contains("paths rozu cannot determine"));
        assert!(text.contains("full access to your files and network, for this call only"));
    }

    #[test]
    fn codex_retry_does_not_offer_to_remove_its_sandbox() {
        let mut req = request(SandboxMode::Enforced(CODEX_SANDBOX));
        req.blocked_retry = true;
        req.network = true;
        req.outside_unknown = true;
        let text = confinement(&req);
        assert!(text.contains("rozu cannot remove it"));
        assert!(text.contains("Network requested: on"));
        assert!(text.contains("blocked the previous attempt"));
        assert!(!text.contains("full access"));
        assert!(!text.contains("Run without the sandbox"));
    }

    #[test]
    fn no_sandbox_does_not_claim_to_block_network() {
        let text = confinement(&request(SandboxMode::Off));
        assert!(text.contains("No OS sandbox"));
        assert!(!text.contains("Network off"));
        assert!(!text.contains("Run without the sandbox"));
    }
}
