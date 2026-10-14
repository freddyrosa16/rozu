use rozu_core::{ContentPart, Message, Role};

use super::*;

fn meta(id: &str, workspace: &Path) -> SessionMeta {
    SessionMeta {
        id: id.into(),
        workspace: workspace.into(),
        provider: "openai".into(),
        model: "gpt-5".into(),
        created: 1,
    }
}

fn chat(turns: &[&str]) -> Session {
    let mut session = Session::with_system_prompt("sys");
    for turn in turns {
        session.messages.push(Message::user(*turn));
        session
            .messages
            .push(Message::assistant(format!("re: {turn}")));
    }
    session
}

#[test]
fn a_session_survives_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let ws = Path::new("/w");
    let mut session = chat(&["hello there"]);
    session.usage.input_tokens = 10;
    session.cost_usd = 0.5;
    {
        let store = SessionStore::open(dir.path()).unwrap();
        let mut writer = store.create(meta("s1", ws)).unwrap();
        writer.sync(&session).unwrap();
        session.messages.push(Message::user("again"));
        session.messages.push(Message::new(
            Role::Assistant,
            vec![ContentPart::ToolCall {
                id: "c".into(),
                name: "read_file".into(),
                args_json: "{}".into(),
            }],
        ));
        session
            .messages
            .push(Message::tool_result("c", "out", false));
        writer.sync(&session).unwrap();
        writer.sync(&session).unwrap();
        writer.set_model("anthropic", "claude").unwrap();
    }
    let store = SessionStore::open(dir.path()).unwrap();
    let id = store.resolve(None, ws).unwrap();
    let (mut resumed, mut writer) = store.resume(&id).unwrap();
    assert_eq!(resumed, session);
    assert_eq!(writer.meta().model, "claude");

    resumed.messages.push(Message::user("third"));
    writer.sync(&resumed).unwrap();
    let (_, reloaded) = store.load("s1").unwrap();
    assert_eq!(reloaded.messages, resumed.messages);

    let list = store.list(Some(ws), None, 10).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "hello there");
    assert_eq!(list[0].messages, 6);
    assert_eq!(list[0].provider, "anthropic");
}

#[test]
fn compaction_writes_a_checkpoint() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    let mut session = chat(&["a", "b", "c"]);
    let mut writer = store.create(meta("s", Path::new("/w"))).unwrap();
    writer.sync(&session).unwrap();
    session.messages.drain(1..5);
    session.compactions += 1;
    writer.sync(&session).unwrap();
    session.messages.push(Message::user("d"));
    writer.sync(&session).unwrap();
    let (_, loaded) = store.load("s").unwrap();
    assert_eq!(loaded, session);
}

#[test]
fn resolve_by_prefix_last_and_workspace() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    for (id, ws) in [
        ("20261014-aa", "/a"),
        ("20261014-ab", "/a"),
        ("20261015-b", "/b"),
    ] {
        store
            .create(meta(id, Path::new(ws)))
            .unwrap()
            .sync(&chat(&[id]))
            .unwrap();
    }
    // Not yet talked to: invisible.
    store
        .create(meta("20261016-empty", Path::new("/a")))
        .unwrap();

    assert_eq!(
        store.resolve(Some("20261015"), Path::new("/x")).unwrap(),
        "20261015-b"
    );
    assert!(matches!(
        store.resolve(Some("20261014-a"), Path::new("/a")),
        Err(StoreError::Ambiguous(_))
    ));
    assert!(matches!(
        store.resolve(Some("nope"), Path::new("/a")),
        Err(StoreError::NotFound(_))
    ));
    assert!(
        store
            .resolve(Some("last"), Path::new("/a"))
            .unwrap()
            .starts_with("20261014-a")
    );
    assert!(matches!(
        store.resolve(None, Path::new("/none")),
        Err(StoreError::NotFound(_))
    ));
    assert_eq!(
        store.list(Some(Path::new("/a")), None, 10).unwrap().len(),
        2
    );
    assert_eq!(store.list(None, Some("20261015"), 10).unwrap().len(), 1);
    assert_eq!(store.list(None, None, 1).unwrap().len(), 1);
}

#[test]
fn delete_removes_the_file_and_the_row() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    store
        .create(meta("gone", Path::new("/w")))
        .unwrap()
        .sync(&chat(&["bye"]))
        .unwrap();
    assert_eq!(store.list(None, None, 10).unwrap().len(), 1);
    store.delete("gone").unwrap();
    assert!(!store.path_of("gone").exists());
    assert!(store.list(None, None, 10).unwrap().is_empty());
    assert!(matches!(store.load("gone"), Err(StoreError::NotFound(_))));
    store.delete("gone").unwrap();
}

#[test]
fn a_lost_index_is_rebuilt_and_a_torn_line_is_skipped() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    let session = chat(&["keep me"]);
    store
        .create(meta("s", Path::new("/w")))
        .unwrap()
        .sync(&session)
        .unwrap();
    let mut file = OpenOptions::new()
        .append(true)
        .open(store.path_of("s"))
        .unwrap();
    file.write_all(b"{\"type\":\"mess").unwrap();
    fs::remove_file(dir.path().join(INDEX_FILE)).unwrap();

    let store = SessionStore::open(dir.path()).unwrap();
    let list = store.list(None, Some("keep"), 10).unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(store.load("s").unwrap().1, session);
}

#[test]
fn resuming_a_torn_log_preserves_subsequent_turns() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    let original = chat(&["keep me"]);
    let mut writer = store.create(meta("s", Path::new("/w"))).unwrap();
    writer.sync(&original).unwrap();
    drop(writer);
    OpenOptions::new()
        .append(true)
        .open(store.path_of("s"))
        .unwrap()
        .write_all(b"{\"type\":\"mess")
        .unwrap();

    let (mut session, mut writer) = store.resume("s").unwrap();
    assert_eq!(session, original);
    session.messages.push(Message::user("after recovery"));
    writer.sync(&session).unwrap();
    assert_eq!(store.load("s").unwrap().1, session);
}

#[test]
fn duplicate_creation_cannot_append_another_header() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    let mut writer = store.create(meta("s", Path::new("/w"))).unwrap();
    writer.sync(&chat(&["original"])).unwrap();
    let before = fs::read(writer.path()).unwrap();
    assert!(store.create(meta("s", Path::new("/other"))).is_err());
    assert_eq!(fs::read(writer.path()).unwrap(), before);
}

#[test]
fn reindex_removes_rows_for_deleted_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    let mut writer = store.create(meta("s", Path::new("/w"))).unwrap();
    writer.sync(&chat(&["gone"])).unwrap();
    drop(writer);
    fs::remove_file(store.path_of("s")).unwrap();
    assert_eq!(store.reindex().unwrap(), 0);
    assert!(store.list(None, None, 10).unwrap().is_empty());
}

#[test]
fn id_prefixes_are_literal() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    store.create(meta("a-b", Path::new("/w"))).unwrap();
    for prefix in ["%", "a_", "A-"] {
        assert!(matches!(
            store.resolve(Some(prefix), Path::new("/w")),
            Err(StoreError::NotFound(_))
        ));
    }
    assert_eq!(store.resolve(Some("a-"), Path::new("/w")).unwrap(), "a-b");
}

#[test]
fn session_operations_reject_ids_that_escape_the_store() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path().join("sessions")).unwrap();
    let outside = dir.path().join("outside.jsonl");
    fs::write(&outside, b"untouched").unwrap();
    for id in ["../outside", "/outside", "..\\outside", "", ".", ".."] {
        assert!(matches!(
            store.create(meta(id, Path::new("/w"))),
            Err(StoreError::InvalidId(_))
        ));
        assert!(matches!(store.load(id), Err(StoreError::InvalidId(_))));
        assert!(matches!(store.resume(id), Err(StoreError::InvalidId(_))));
        assert!(matches!(store.delete(id), Err(StoreError::InvalidId(_))));
    }
    assert_eq!(fs::read(outside).unwrap(), b"untouched");
}

#[test]
fn resume_rejects_a_mismatched_header_without_changing_either_file() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    store.create(meta("original", Path::new("/w"))).unwrap();
    fs::copy(store.path_of("original"), store.path_of("copy")).unwrap();
    let before = fs::read(store.path_of("copy")).unwrap();
    assert!(matches!(
        store.resume("copy"),
        Err(StoreError::MismatchedId { .. })
    ));
    assert_eq!(fs::read(store.path_of("copy")).unwrap(), before);
    assert_eq!(fs::read(store.path_of("original")).unwrap(), before);
}

#[test]
fn resume_separates_a_valid_final_record_without_a_newline() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    store.create(meta("s", Path::new("/w"))).unwrap();
    let file = OpenOptions::new()
        .write(true)
        .open(store.path_of("s"))
        .unwrap();
    file.set_len(file.metadata().unwrap().len() - 1).unwrap();
    let (mut session, mut writer) = store.resume("s").unwrap();
    session.messages.push(Message::user("after recovery"));
    writer.sync(&session).unwrap();
    assert_eq!(store.load("s").unwrap().1, session);
}

#[test]
fn resume_rejects_corruption_before_the_final_record() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    store.create(meta("s", Path::new("/w"))).unwrap();
    OpenOptions::new()
        .append(true)
        .open(store.path_of("s"))
        .unwrap()
        .write_all(b"broken\n{\"type\":\"model\",\"provider\":\"p\",\"model\":\"m\"}\n")
        .unwrap();
    let before = fs::read(store.path_of("s")).unwrap();
    assert!(matches!(store.resume("s"), Err(StoreError::Json(_))));
    assert_eq!(fs::read(store.path_of("s")).unwrap(), before);
}

#[cfg(unix)]
#[test]
fn a_session_has_one_writer_and_cannot_be_deleted_while_open() {
    let dir = tempfile::tempdir().unwrap();
    let store = SessionStore::open(dir.path()).unwrap();
    let mut writer = store.create(meta("s", Path::new("/w"))).unwrap();
    let original = chat(&["first"]);
    writer.sync(&original).unwrap();
    let other_store = SessionStore::open(dir.path()).unwrap();
    assert!(matches!(other_store.resume("s"), Err(StoreError::InUse(_))));
    assert!(matches!(other_store.delete("s"), Err(StoreError::InUse(_))));
    assert_eq!(other_store.load("s").unwrap().1, original);
    drop(writer);

    let (mut session, mut writer) = other_store.resume("s").unwrap();
    assert!(matches!(store.resume("s"), Err(StoreError::InUse(_))));
    session.messages.push(Message::user("next"));
    writer.sync(&session).unwrap();
    drop(writer);
    assert_eq!(store.load("s").unwrap().1, session);
    store.delete("s").unwrap();
    assert!(store.list(None, None, 10).unwrap().is_empty());
}

#[cfg(unix)]
#[test]
fn writer_lock_child() {
    let Some(dir) = std::env::var_os("ROZU_STORE_LOCK_CHILD_DIR") else {
        return;
    };
    let store = SessionStore::open(dir).unwrap();
    let mut writer = store.create(meta("s", Path::new("/w"))).unwrap();
    writer.sync(&chat(&["saved before crash"])).unwrap();
    println!("WRITER_READY");
    io::stdout().flush().unwrap();
    let mut input = [0];
    std::io::Read::read_exact(&mut io::stdin(), &mut input).unwrap();
    drop(writer);
}

#[cfg(unix)]
#[test]
fn writer_lock_is_released_after_a_process_crash() {
    use std::io::BufRead;
    use std::process::{Command, Stdio};

    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let dir = tempfile::tempdir().unwrap();
    let mut child = Child(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "session::tests::writer_lock_child",
                "--nocapture",
            ])
            .env("ROZU_STORE_LOCK_CHILD_DIR", dir.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(
            output.read_line(&mut line).unwrap(),
            0,
            "child exited before acquiring the lock"
        );
        if line.trim() == "WRITER_READY" {
            break;
        }
    }
    let store = SessionStore::open(dir.path()).unwrap();
    assert!(matches!(store.resume("s"), Err(StoreError::InUse(_))));
    assert!(matches!(store.delete("s"), Err(StoreError::InUse(_))));
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    let (session, _writer) = store.resume("s").unwrap();
    assert_eq!(session.messages, chat(&["saved before crash"]).messages);
}
