//! The sidebar: nav, the project folders with their chats, and the account row.

use super::*;

impl Rozu {
    pub(super) fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let nav = [
            ("chat", "New Chat", Page::Home),
            ("search", "Search", Page::Search),
            ("blocks", "Customize", Page::Customize),
        ];
        let (repos, folders): (Vec<PathBuf>, Vec<PathBuf>) = self
            .settings
            .projects
            .iter()
            .cloned()
            .partition(|p| p.join(".git").exists());
        let folders: Vec<AnyElement> = folders
            .into_iter()
            .map(|p| self.project_group(p, cx))
            .collect();
        let repos: Vec<AnyElement> = repos
            .into_iter()
            .map(|p| self.project_group(p, cx))
            .collect();
        let setup = self.page == Page::Setup;
        div()
            .id("full-sidebar")
            .w(px(270.))
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(SIDE))
            .border_r_1()
            .border_color(rgb(LINE))
            .px_2()
            .pb_2()
            .child(drag_bar().justify_end().pr_2().when(self.compact, |d| {
                d.child(
                    button("hide-projects", "Close", false).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.sidebar_open = false;
                            cx.notify();
                        },
                    )),
                )
            }))
            .children(nav.into_iter().map(|(i, label, page)| {
                let on = self.page == page;
                row(label, i, label)
                    .when(on, |r| r.bg(rgb(RAISE)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.navigate(page, window, cx);
                    }))
            }))
            .child(
                div()
                    .id("projects")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(heading("Projects"))
                    .child(
                        row("new-project", "plus", "New Project")
                            .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
                    )
                    .children(folders)
                    .when(!repos.is_empty(), |d| {
                        d.child(heading("Repositories")).children(repos)
                    }),
            )
            .child(
                account()
                    .id("account")
                    .cursor_pointer()
                    .when(setup, |r| r.bg(rgb(RAISE)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.setup(cx);
                        window.focus(&this.page_focus);
                    })),
            )
    }

    fn navigate(&mut self, page: Page, window: &mut Window, cx: &mut Context<Self>) {
        if page == Page::Search {
            self.open_search(window, cx);
        } else {
            self.open(page, cx);
            window.focus(&self.focus_handle(cx));
        }
    }

    pub(super) fn compact_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("compact-sidebar")
            .w(px(88.))
            .h_full()
            .flex_none()
            .flex()
            .flex_col()
            .bg(rgb(SIDE))
            .border_r_1()
            .border_color(rgb(LINE))
            .px_2()
            .pb_2()
            .child(drag_bar())
            .children(
                [
                    ("chat", "New chat", Page::Home),
                    ("search", "Search", Page::Search),
                    ("blocks", "Customize", Page::Customize),
                ]
                .into_iter()
                .map(|(i, label, page)| {
                    rail_button(label, i, label)
                        .when(self.page == page, |d| d.bg(rgb(RAISE)))
                        .on_click(
                            cx.listener(move |this, _, window, cx| this.navigate(page, window, cx)),
                        )
                }),
            )
            .child(
                rail_button("show-projects", "folder", "Projects").on_click(cx.listener(
                    |this, _, _, cx| {
                        this.sidebar_open = !this.sidebar_open;
                        cx.notify();
                    },
                )),
            )
            .child(
                rail_button("add-project-compact", "plus", "Add folder")
                    .on_click(cx.listener(|this, _, _, cx| this.add_project(cx))),
            )
            .child(div().flex_1())
            .child(
                rail_button("account-compact", "gear", "Your AI")
                    .when(self.page == Page::Setup, |d| d.bg(rgb(RAISE)))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.setup(cx);
                        window.focus(&this.page_focus);
                    })),
            )
    }

    pub(super) fn project_group(&mut self, root: PathBuf, cx: &mut Context<Self>) -> AnyElement {
        let open = !self.collapsed.contains(&root);
        let mut ids: Vec<usize> = self
            .threads
            .iter()
            .filter(|t| t.root == root)
            .map(|t| t.id)
            .collect();
        ids.sort_by_key(|&id| {
            std::cmp::Reverse(
                self.threads
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| (t.touched, id)),
            )
        });
        let rows: Vec<AnyElement> = if open {
            ids.into_iter()
                .map(|id| self.thread_row(id, cx).into_any_element())
                .collect()
        } else {
            Vec::new()
        };
        let key = root.display().to_string();
        let (new_root, del_root, toggle_root) = (root.clone(), root.clone(), root.clone());
        let here = self.page == Page::Home && self.project.as_ref() == Some(&root);
        div()
            .child(
                row(
                    SharedString::from(format!("p:{key}")),
                    "folder",
                    name(&root),
                )
                .group("project")
                .when(here, |r| r.bg(rgb(RAISE)))
                .child(div().flex_1())
                .child(
                    hover_icon(SharedString::from(format!("pnew:{key}")), "project", "plus")
                        .on_click(cx.listener(move |this, _, _, cx| {
                            cx.stop_propagation();
                            this.project = Some(new_root.clone());
                            this.open(Page::Home, cx);
                        })),
                )
                .child(
                    hover_icon(
                        SharedString::from(format!("pdel:{key}")),
                        "project",
                        "trash",
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.remove_project(del_root.clone(), window, cx);
                    })),
                )
                .child(icon(if open { "down" } else { "right" }, INK3))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.collapsed.remove(&toggle_root) {
                        this.collapsed.insert(toggle_root.clone());
                    }
                    cx.notify();
                })),
            )
            .children(rows)
            .into_any_element()
    }

    pub(super) fn thread_row(&mut self, id: usize, cx: &mut Context<Self>) -> Stateful<Div> {
        let Some(thread) = self.thread_ref(id) else {
            return div().id(("thread", id));
        };
        let dot = match thread.feed.state() {
            ActivityState::Error => Some(BAD),
            ActivityState::Waiting => Some(WARN),
            _ if thread.feed.running => Some(WARN),
            _ => None,
        };
        div()
            .id(("thread", id))
            .group("thread")
            .flex()
            .items_center()
            .gap_3()
            .pl(px(14.))
            .pr_2()
            .py(px(6.))
            .rounded(px(8.))
            .cursor_pointer()
            .hover(|s| s.bg(rgb(RAISE)))
            .when(self.page == Page::Thread(id), |r| r.bg(rgb(RAISE)))
            .child(logo(thread.provider))
            .child(div().flex_1().truncate().child(thread.title.clone()))
            .children(dot.map(|c| div().size(px(7.)).flex_none().rounded_full().bg(rgb(c))))
            .child(
                div()
                    .text_size(px(13.5))
                    .text_color(rgb(INK3))
                    .group_hover("thread", |s| s.invisible())
                    .child(age(thread.touched)),
            )
            .child(
                hover_icon(("del", id), "thread", "trash")
                    .absolute()
                    .right(px(8.))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        cx.stop_propagation();
                        this.remove_chat(id, window, cx);
                    })),
            )
            .relative()
            .on_click(cx.listener(move |this, _, _, cx| this.open(Page::Thread(id), cx)))
    }
}

fn rail_button(id: &'static str, i: &'static str, label: &'static str) -> Stateful<Div> {
    div()
        .id(id)
        .w_full()
        .flex_none()
        .flex()
        .flex_col()
        .items_center()
        .gap_1()
        .py_2()
        .rounded(px(8.))
        .cursor_pointer()
        .hover(|s| s.bg(rgb(RAISE)))
        .child(icon(i, INK2))
        .child(div().text_size(px(10.)).text_color(rgb(INK2)).child(label))
}

pub(super) fn account() -> Div {
    let name = std::env::var("USER").unwrap_or_default();
    let initial: String = name
        .chars()
        .next()
        .map(|c| c.to_uppercase().collect())
        .unwrap_or_default();
    div()
        .flex()
        .items_center()
        .gap(px(10.))
        .pt(px(10.))
        .px_1()
        .child(
            div()
                .size(px(30.))
                .rounded_full()
                .bg(rgb(RAISE))
                .flex()
                .items_center()
                .justify_center()
                .text_color(rgb(INK2))
                .child(initial),
        )
        .child(div().flex_1().child(name))
        .child(icon("gear", INK3))
}

pub(super) fn age(at: u64) -> String {
    match unix_now().saturating_sub(at) {
        s if s < 60 => "now".into(),
        s if s < 3600 => format!("{}m", s / 60),
        s if s < 86400 => format!("{}h", s / 3600),
        s => format!("{}d", s / 86400),
    }
}
