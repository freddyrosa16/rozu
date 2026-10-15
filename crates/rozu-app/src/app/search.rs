//! Saved chat titles and session ids, scoped to the projects added to the app.

use rozu_store::SessionSummary;

use super::*;
use crate::backend::SEARCH_LIMIT;

#[derive(Default)]
pub(super) struct SearchState {
    requested: Option<(String, Vec<PathBuf>)>,
    rows: Vec<SessionSummary>,
    error: Option<String>,
    loading: bool,
    generation: u64,
    task: Option<Task<()>>,
    forgotten: HashSet<String>,
}

impl SearchState {
    pub(super) fn forget(&mut self, session: &str) {
        self.forgotten.insert(session.to_owned());
        self.rows.retain(|row| row.id != session);
    }

    fn set_rows(&mut self, rows: Vec<SessionSummary>) {
        self.rows = rows
            .into_iter()
            .filter(|row| !self.forgotten.contains(&row.id))
            .collect();
    }
}

#[derive(Clone)]
enum Target {
    Open(usize),
    Saved(SessionSummary),
}

struct ResultRow {
    key: String,
    title: SharedString,
    root: PathBuf,
    provider: String,
    touched: u64,
    target: Target,
}

impl Rozu {
    pub(super) fn open_search(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.open(Page::Search, cx);
        self.search_state.requested = None;
        self.search_chats(cx);
        window.focus(&self.chat_search.focus_handle(cx));
    }

    pub(super) fn search_chats(&mut self, cx: &mut Context<Self>) {
        if self.page != Page::Search {
            return;
        }
        let query = self.chat_search.read(cx).text.trim().to_owned();
        let roots = self.settings.projects.clone();
        let requested = (query.clone(), roots.clone());
        if self.search_state.requested.as_ref() == Some(&requested) {
            return;
        }
        self.search_state.requested = Some(requested);
        self.search_state.generation += 1;
        let generation = self.search_state.generation;
        self.search_state.rows.clear();
        self.search_state.error = None;
        self.search_state.loading = !roots.is_empty();
        self.search_state.task = None;
        if roots.is_empty() {
            cx.notify();
            return;
        }
        let delay = cx.background_executor().timer(Duration::from_millis(150));
        self.search_state.task = Some(cx.spawn(async move |this, cx| {
            delay.await;
            let Ok(rx) = this.update(cx, |app, _| app.backend.search_chats(roots.clone(), query))
            else {
                return;
            };
            let result = rx.await.unwrap_or_else(|_| {
                Err(anyhow::anyhow!("The saved chat search stopped. Try again."))
            });
            let _ = this.update(cx, |app, cx| {
                if generation != app.search_state.generation {
                    return;
                }
                if roots != app.settings.projects {
                    app.search_state.requested = None;
                    app.search_chats(cx);
                    return;
                }
                app.search_state.loading = false;
                match result {
                    Ok(rows) => app.search_state.set_rows(rows),
                    Err(err) => app.search_state.error = Some(format!("{err:#}")),
                }
                cx.notify();
            });
        }));
        cx.notify();
    }

    fn search_results(&self, cx: &App) -> Vec<ResultRow> {
        let query = self.chat_search.read(cx).text.trim().to_ascii_lowercase();
        let mut rows = Vec::new();
        let mut seen = HashSet::new();
        for thread in &self.threads {
            if !self.settings.projects.contains(&thread.root)
                || !(thread.title.to_ascii_lowercase().contains(&query)
                    || thread
                        .session
                        .as_deref()
                        .is_some_and(|id| id.to_ascii_lowercase().starts_with(&query)))
            {
                continue;
            }
            let key = thread
                .session
                .clone()
                .unwrap_or_else(|| format!("thread:{}", thread.id));
            seen.insert(key.clone());
            rows.push(ResultRow {
                key,
                title: thread.title.clone(),
                root: thread.root.clone(),
                provider: thread.provider.into(),
                touched: thread.touched,
                target: Target::Open(thread.id),
            });
        }
        for row in &self.search_state.rows {
            if self.search_state.forgotten.contains(&row.id)
                || !self.settings.projects.contains(&row.workspace)
                || !seen.insert(row.id.clone())
            {
                continue;
            }
            let open = self
                .threads
                .iter()
                .find(|thread| thread.session.as_deref() == Some(&row.id));
            rows.push(ResultRow {
                key: row.id.clone(),
                title: open.map_or_else(|| row.title.clone().into(), |t| t.title.clone()),
                root: row.workspace.clone(),
                provider: open.map_or_else(|| row.provider.clone(), |t| t.provider.into()),
                touched: open.map_or(row.updated, |t| t.touched.max(row.updated)),
                target: open.map_or_else(|| Target::Saved(row.clone()), |t| Target::Open(t.id)),
            });
        }
        rows.sort_by(|a, b| b.touched.cmp(&a.touched).then(b.key.cmp(&a.key)));
        rows.truncate(SEARCH_LIMIT);
        rows
    }

    fn open_search_result(&mut self, target: Target, window: &mut Window, cx: &mut Context<Self>) {
        let id = match target {
            Target::Open(id) => id,
            Target::Saved(row) => {
                if self.search_state.forgotten.contains(&row.id)
                    || !self.settings.projects.contains(&row.workspace)
                {
                    return;
                }
                if let Some(thread) = self
                    .threads
                    .iter()
                    .find(|thread| thread.session.as_deref() == Some(&row.id))
                {
                    thread.id
                } else {
                    let id = self.next_id;
                    self.next_id += 1;
                    self.threads.push(Thread {
                        id,
                        root: row.workspace,
                        provider: registry::provider_info(&row.provider).map_or("", |p| p.id),
                        choice: registry::resolve(Some(&row.provider), Some(&row.model))
                            .ok()
                            .map(catalog::pin),
                        effort: None,
                        title: row.title.into(),
                        feed: Feed::default(),
                        session: Some(row.id),
                        loaded: false,
                        chat: None,
                        pending: HashMap::new(),
                        touched: row.updated,
                        _updates: None,
                    });
                    id
                }
            }
        };
        if let Some(thread) = self.thread_ref(id) {
            self.collapsed.remove(&thread.root.clone());
            self.open(Page::Thread(id), cx);
            window.focus(&self.composer.focus_handle(cx));
        }
    }

    pub(super) fn search_page(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let query = self.chat_search.read(cx).text.trim().to_owned();
        let wrap_at = (self.text_w - if query.is_empty() { 64. } else { 128. }).max(80.);
        self.chat_search.update(cx, |input, cx| {
            if (input.wrap_at - wrap_at).abs() > 0.5 {
                input.wrap_at = wrap_at;
                cx.notify();
            }
        });
        let rows = self.search_results(cx);
        let empty = rows.is_empty();
        let loading = self.search_state.loading;
        let error = self.search_state.error.clone();
        let no_projects = self.settings.projects.is_empty();
        div()
            .flex_1()
            .min_w_0()
            .min_h_0()
            .flex()
            .flex_col()
            .child(drag_bar())
            .child(
                column()
                    .w(px(self.text_w + 56.))
                    .gap_3()
                    .pt(px(24.))
                    .pb(px(16.))
                    .child(div().text_size(px(28.)).child("Search chats"))
                    .child(
                        div()
                            .w(px(self.text_w))
                            .text_size(px(14.))
                            .text_color(rgb(INK3))
                            .child("Find a saved title or session ID across your added projects."),
                    )
                    .child(
                        div()
                            .id("chat-search-input")
                            .flex()
                            .items_center()
                            .gap_3()
                            .p(px(14.))
                            .border_1()
                            .border_color(rgb(LINE))
                            .rounded(px(12.))
                            .bg(rgb(BOX))
                            .child(icon("search", INK3))
                            .child(
                                div()
                                    .id("chat-search-text")
                                    .flex_1()
                                    .min_w_0()
                                    .max_h(px(104.))
                                    .overflow_y_scroll()
                                    .overflow_x_hidden()
                                    .child(self.chat_search.clone()),
                            )
                            .when(!query.is_empty(), |d| {
                                d.child(
                                    div()
                                        .id("clear-chat-search")
                                        .flex_none()
                                        .px_2()
                                        .py_1()
                                        .rounded(px(6.))
                                        .text_size(px(14.))
                                        .text_color(rgb(INK2))
                                        .cursor_pointer()
                                        .hover(|s| s.bg(rgb(RAISE)))
                                        .child("Clear")
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.chat_search.update(cx, |input, cx| {
                                                input.set_text("", cx);
                                            });
                                            window.focus(&this.chat_search.focus_handle(cx));
                                        })),
                                )
                            })
                            .on_click(cx.listener(|this, _, window, cx| {
                                window.focus(&this.chat_search.focus_handle(cx));
                            })),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(rgb(INK3))
                            .child(if loading {
                                "Searching…"
                            } else if query.is_empty() {
                                "Recent chats"
                            } else {
                                "Results"
                            }),
                    ),
            )
            .child(
                div()
                    .id("chat-search-results")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(
                        column()
                            .w(px(self.text_w + 56.))
                            .pb(px(28.))
                            .when_some(error.clone(), |d, message| {
                                d.child(
                                    div()
                                        .flex()
                                        .flex_col()
                                        .gap_2()
                                        .mb_3()
                                        .child(
                                            div()
                                                .w(px(self.text_w))
                                                .text_size(px(14.))
                                                .text_color(rgb(WARN))
                                                .child(message),
                                        )
                                        .child(
                                            button("retry-search", "Try again", false).on_click(
                                                cx.listener(|this, _, _, cx| {
                                                    this.search_state.requested = None;
                                                    this.search_chats(cx);
                                                }),
                                            ),
                                        ),
                                )
                            })
                            .when(empty && !loading && error.is_none(), |d| {
                                d.child(
                                    div().w(px(self.text_w)).py_3().text_color(rgb(INK2)).child(
                                        if no_projects {
                                            "Add a project to find its saved chats."
                                        } else if query.is_empty() {
                                            "No chats yet. Start a chat in one of your projects."
                                        } else {
                                            "No matching chats. Try another title or session ID."
                                        },
                                    ),
                                )
                            })
                            .children(rows.into_iter().map(|row| {
                                let detail = format!(
                                    "{} · {}",
                                    row.root.display(),
                                    sidebar::age(row.touched)
                                );
                                div()
                                    .id(SharedString::from(format!("chat-result:{}", row.key)))
                                    .flex()
                                    .items_center()
                                    .gap_3()
                                    .px(px(12.))
                                    .py(px(12.))
                                    .rounded(px(10.))
                                    .cursor_pointer()
                                    .hover(|s| s.bg(rgb(RAISE)))
                                    .child(logo(&row.provider))
                                    .child(
                                        div()
                                            .flex_1()
                                            .min_w_0()
                                            .flex()
                                            .flex_col()
                                            .gap_1()
                                            .child(div().truncate().child(row.title))
                                            .child(
                                                div()
                                                    .truncate()
                                                    .text_size(px(13.))
                                                    .text_color(rgb(INK3))
                                                    .child(detail),
                                            ),
                                    )
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.open_search_result(row.target.clone(), window, cx);
                                    }))
                            }))
                            .when(!empty, |d| {
                                d.child(
                                    div().pt_3().text_size(px(13.)).text_color(rgb(INK3)).child(
                                        format!(
                                            "Showing up to {SEARCH_LIMIT} chats, most recent first."
                                        ),
                                    ),
                                )
                            }),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved(id: &str) -> SessionSummary {
        SessionSummary {
            id: id.into(),
            workspace: "/project".into(),
            provider: "anthropic".into(),
            model: "test".into(),
            title: "Saved title".into(),
            created: 1,
            updated: 2,
            messages: 1,
        }
    }

    #[test]
    fn deleting_a_chat_hides_cached_and_late_search_results() {
        let mut state = SearchState::default();
        state.set_rows(vec![saved("deleted"), saved("kept")]);
        state.forget("deleted");
        assert_eq!(state.rows, vec![saved("kept")]);
        state.set_rows(vec![saved("deleted"), saved("kept"), saved("new")]);
        assert_eq!(state.rows, vec![saved("kept"), saved("new")]);
    }
}
