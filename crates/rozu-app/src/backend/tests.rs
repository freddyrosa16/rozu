use super::*;
use futures::stream;
use rozu_core::{
    AgentConfig, ChatRequest, EventStream, Message, Provider, ProviderError, ProviderResult, Role,
    StopReason, StreamEvent,
};
use rozu_providers::registry;

fn chat() -> (Chat, mpsc::UnboundedReceiver<Prompt>) {
    let (prompts, rx) = mpsc::unbounded_channel();
    (
        Chat {
            session_id: new_session_id(),
            prompts,
            cancel: Mutex::new(CancellationToken::new()),
            worker: None,
        },
        rx,
    )
}

#[test]
fn stopping_a_queued_prompt_cancels_that_turn_only() {
    let (chat, mut prompts) = chat();
    let choice = registry::resolve(Some("anthropic"), None).unwrap();
    assert!(chat.send("first".into(), &choice, None));
    chat.cancel();
    let first = prompts.try_recv().unwrap();
    assert_eq!(first.text, "first");
    assert!(first.cancel.is_cancelled());

    assert!(chat.send("second".into(), &choice, None));
    let second = prompts.try_recv().unwrap();
    assert!(!second.cancel.is_cancelled());
    drop(chat);
    assert!(second.cancel.is_cancelled());
    assert!(matches!(
        prompts.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[test]
fn a_failed_resume_does_not_replace_saved_history() {
    let backend = Backend {
        rt: tokio::runtime::Runtime::new().unwrap(),
        sessions: None,
    };
    let choice = registry::resolve(Some("anthropic"), None).unwrap();
    let (updates, mut rx) = mpsc::unbounded_channel();
    let result = backend.start(
        Path::new("."),
        &choice,
        None,
        ApprovalMode::ReadOnly,
        Some("saved-chat"),
        updates,
    );
    assert!(matches!(result, Err(err) if err.to_string().contains("saved chat store")));
    assert!(matches!(
        rx.try_recv(),
        Err(mpsc::error::TryRecvError::Disconnected)
    ));
}

#[test]
fn forgetting_waits_for_the_final_save() {
    let dir = std::env::temp_dir().join(format!("rozu-app-delete-{}", new_session_id()));
    let store = SessionStore::open(&dir).unwrap();
    let mut writer = store
        .create(SessionMeta {
            id: "saved-chat".into(),
            workspace: dir.clone(),
            provider: "anthropic".into(),
            model: "test".into(),
            created: unix_now(),
        })
        .unwrap();
    let backend = Backend {
        rt: tokio::runtime::Runtime::new().unwrap(),
        sessions: Some(store.clone()),
    };
    let (mut chat, mut prompts) = chat();
    let (started, ready) = oneshot::channel();
    chat.worker = Some(backend.rt.spawn(async move {
        let prompt = prompts.recv().await.unwrap();
        let _ = started.send(());
        prompt.cancel.cancelled().await;
        let mut session = Session::new();
        session.messages.push(Message::user(prompt.text));
        writer.sync(&session).unwrap();
    }));
    let choice = registry::resolve(Some("anthropic"), None).unwrap();
    assert!(chat.send("last turn".into(), &choice, None));
    backend.rt.block_on(ready).unwrap();
    let deleting = backend.forget("saved-chat", Some(chat));
    backend.rt.block_on(deleting).unwrap();
    assert!(!store.path_of("saved-chat").exists());
    assert!(store.list(None, None, 10).unwrap().is_empty());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn search_finds_older_chats_across_only_added_projects() {
    let dir = std::env::temp_dir().join(format!("rozu-app-search-{}", new_session_id()));
    let store = SessionStore::open(dir.join("sessions")).unwrap();
    let roots: Vec<_> = ["one", "two", "outside"]
        .map(|name| {
            let root = dir.join(name);
            std::fs::create_dir_all(&root).unwrap();
            root.canonicalize().unwrap()
        })
        .into();
    let save = |root: &Path, id: &str, title: &str| {
        let mut writer = store
            .create(SessionMeta {
                id: id.into(),
                workspace: root.to_path_buf(),
                provider: "anthropic".into(),
                model: "test".into(),
                created: unix_now(),
            })
            .unwrap();
        let mut session = Session::new();
        session.messages.push(Message::user(title));
        writer.sync(&session).unwrap();
    };
    save(&roots[0], "000-old", "Find the hidden needle");
    for n in 0..205 {
        save(&roots[0], &format!("recent-{n:03}"), "An unrelated title");
    }
    save(&roots[1], "second-project", "Another NEEDLE");
    save(&roots[2], "outside-project", "Private needle");
    let backend = Backend {
        rt: tokio::runtime::Runtime::new().unwrap(),
        sessions: Some(store),
    };
    assert!(
        backend
            .saved_chats(&roots[0])
            .iter()
            .all(|row| row.id != "000-old")
    );
    let added = vec![roots[0].clone(), roots[1].clone(), roots[0].join(".")];
    let search = |query: &str| {
        backend
            .rt
            .block_on(backend.search_chats(added.clone(), query.into()))
            .unwrap()
            .unwrap()
    };
    let matches = search(" needle ");
    assert_eq!(matches.len(), 2);
    assert!(matches.iter().any(|row| row.id == "000-old"));
    assert!(matches.iter().any(|row| row.id == "second-project"));
    assert_eq!(search("000-")[0].id, "000-old");
    assert!(search("nothing matches").is_empty());
    let recent = search("");
    assert_eq!(recent.len(), SEARCH_LIMIT);
    assert_eq!(
        recent
            .iter()
            .map(|row| &row.id)
            .collect::<HashSet<_>>()
            .len(),
        recent.len()
    );
    assert!(recent.iter().all(|row| added.contains(&row.workspace)));
    assert!(
        recent
            .windows(2)
            .all(|rows| { (rows[0].updated, &rows[0].id) >= (rows[1].updated, &rows[1].id) })
    );
    assert!(
        backend
            .rt
            .block_on(backend.search_chats(Vec::new(), String::new()))
            .unwrap()
            .unwrap()
            .is_empty()
    );
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn search_reports_an_unavailable_or_broken_store() {
    let mut backend = Backend {
        rt: tokio::runtime::Runtime::new().unwrap(),
        sessions: None,
    };
    let result = backend
        .rt
        .block_on(backend.search_chats(vec![PathBuf::from(".")], String::new()))
        .unwrap();
    assert!(result.unwrap_err().to_string().contains("unavailable"));
    let dir = std::env::temp_dir().join(format!("rozu-app-search-error-{}", new_session_id()));
    backend.sessions = Some(SessionStore::open(&dir).unwrap());
    std::fs::write(dir.join("index.sqlite"), "broken index").unwrap();
    let result = backend
        .rt
        .block_on(backend.search_chats(vec![dir.clone()], String::new()))
        .unwrap();
    assert!(result.is_err());
    std::fs::remove_dir_all(dir).unwrap();
}

type RecordedRequests = Arc<Mutex<Vec<(ChatRequest, Option<String>)>>>;

struct RecordingProvider {
    effort: Option<String>,
    requests: RecordedRequests,
    fail: bool,
}

#[async_trait]
impl Provider for RecordingProvider {
    fn id(&self) -> &str {
        "test"
    }

    async fn stream(&self, req: ChatRequest, _: CancellationToken) -> ProviderResult<EventStream> {
        self.requests
            .lock()
            .unwrap()
            .push((req, self.effort.clone()));
        let end = if self.fail {
            Err(ProviderError::Network("lost connection".into()))
        } else {
            Ok(StreamEvent::Done(StopReason::EndTurn))
        };
        Ok(Box::pin(stream::iter([
            Ok(StreamEvent::TextDelta("response".into())),
            end,
        ])))
    }
}

#[tokio::test]
async fn model_and_effort_changes_keep_history_and_failed_switches_are_retryable() {
    let dir = std::env::temp_dir().join(format!("rozu-app-switch-{}", new_session_id()));
    let store = SessionStore::open(&dir).unwrap();
    let selection = |model: &str, effort: &str| Selection {
        choice: registry::resolve(Some("codex"), Some(model)).unwrap(),
        effort: Some(effort.into()),
    };
    let first = selection("model-one", "low");
    let requests = Arc::new(Mutex::new(Vec::new()));
    let mut builds = Vec::new();
    let mut build = |selected: &Selection| {
        builds.push((selected.choice.model.clone(), selected.effort.clone()));
        if selected.effort.as_deref() == Some("bad") {
            anyhow::bail!("unsupported effort");
        }
        Ok(Agent::new(
            Arc::new(RecordingProvider {
                effort: selected.effort.clone(),
                requests: requests.clone(),
                fail: false,
            }),
            Arc::new(rozu_tools::ToolRegistry::new()),
            Workspace::new(&dir).unwrap(),
            AgentConfig::new(&selected.choice.model),
        ))
    };
    let agent = build(&first).unwrap();
    let writer = store
        .create(SessionMeta {
            id: "switch-chat".into(),
            workspace: dir.clone(),
            provider: first.choice.provider.id.into(),
            model: first.choice.model.clone(),
            created: unix_now(),
        })
        .unwrap();
    let task = ChatTask {
        agent,
        selection: first.clone(),
        session: Session::new(),
        writer: Some(writer),
    };
    let (prompts, rx) = mpsc::unbounded_channel();
    for (text, selected) in [
        ("first", first.clone()),
        ("same settings", first),
        ("new model", selection("model-two", "low")),
        ("new effort", selection("model-two", "high")),
        ("failed switch", selection("model-two", "bad")),
        ("retry", selection("model-two", "high")),
    ] {
        prompts
            .send(Prompt {
                text: text.into(),
                selection: selected,
                cancel: CancellationToken::new(),
            })
            .unwrap();
    }
    drop(prompts);
    let (updates, mut received) = mpsc::unbounded_channel();
    task.run(rx, updates, &mut build).await;
    assert_eq!(
        builds,
        [
            ("model-one".into(), Some("low".into())),
            ("model-two".into(), Some("low".into())),
            ("model-two".into(), Some("high".into())),
            ("model-two".into(), Some("bad".into())),
        ]
    );
    {
        let requests = requests.lock().unwrap();
        let sent: Vec<_> = requests
            .iter()
            .map(|(req, effort)| (req.model.as_str(), effort.as_deref()))
            .collect();
        assert_eq!(
            sent,
            [
                ("model-one", Some("low")),
                ("model-one", Some("low")),
                ("model-two", Some("low")),
                ("model-two", Some("high")),
                ("model-two", Some("high")),
            ]
        );
        let users: Vec<_> = requests
            .last()
            .unwrap()
            .0
            .messages
            .iter()
            .filter(|message| message.role == Role::User)
            .map(Message::text_content)
            .collect();
        assert_eq!(
            users,
            ["first", "same settings", "new model", "new effort", "retry"]
        );
    }
    let (meta, saved) = store.load("switch-chat").unwrap();
    assert_eq!(meta.model, "model-two");
    assert_eq!(saved.messages.len(), 10);
    let mut failures = Vec::new();
    while let Some(update) = received.recv().await {
        if let Update::Failed { message, .. } = update {
            failures.push(message);
        }
    }
    assert_eq!(failures, ["unsupported effort"]);
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn failure_follows_all_partial_response_events() {
    let selection = Selection {
        choice: registry::resolve(Some("codex"), None).unwrap(),
        effort: None,
    };
    let agent = Agent::new(
        Arc::new(RecordingProvider {
            effort: None,
            requests: Arc::default(),
            fail: true,
        }),
        Arc::new(rozu_tools::ToolRegistry::new()),
        Workspace::new(std::env::current_dir().unwrap()).unwrap(),
        AgentConfig::new("test"),
    );
    let task = ChatTask {
        agent,
        selection: selection.clone(),
        session: Session::new(),
        writer: None,
    };
    let (prompts, rx) = mpsc::unbounded_channel();
    prompts
        .send(Prompt {
            text: "go".into(),
            selection,
            cancel: CancellationToken::new(),
        })
        .unwrap();
    drop(prompts);
    let (updates, mut received) = mpsc::unbounded_channel();
    task.run(rx, updates, |_| panic!("unchanged model must be reused"))
        .await;
    assert!(matches!(
        received.recv().await,
        Some(Update::Event(AgentEvent::TextDelta(text))) if text == "response"
    ));
    assert!(matches!(
        received.recv().await,
        Some(Update::Failed { offline: true, .. })
    ));
    assert!(received.recv().await.is_none());
}
