//! Sessions on disk: `<dir>/<id>.jsonl` (append-only) plus `<dir>/index.sqlite`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rozu_core::{Message, Role, Session, Usage};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Overrides where sessions are stored.
pub const SESSIONS_DIR_ENV: &str = "ROZU_SESSIONS_DIR";
const INDEX_FILE: &str = "index.sqlite";
const TITLE_CHARS: usize = 80;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("bad session record: {0}")]
    Json(#[from] serde_json::Error),
    #[error("session index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("no saved session matches `{0}` (see `rozu sessions`)")]
    NotFound(String),
    #[error("`{0}` matches more than one session; use more of the id")]
    Ambiguous(String),
    #[error("{0} has no session header")]
    NoHeader(PathBuf),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionMeta {
    pub id: String,
    pub workspace: PathBuf,
    pub provider: String,
    pub model: String,
    /// Unix seconds.
    pub created: u64,
}

/// One line of a session file. Replaying them in order rebuilds the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Record {
    Meta(SessionMeta),
    Message {
        message: Message,
    },
    /// Running totals after a turn.
    Usage {
        usage: Usage,
        cost_usd: f64,
        #[serde(default)]
        context_tokens: u64,
    },
    /// The model changed (`/model`).
    Model {
        provider: String,
        model: String,
    },
    /// The whole history after a compaction; replaces everything before it.
    Checkpoint {
        messages: Vec<Message>,
    },
}

/// A row of `rozu sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub workspace: PathBuf,
    pub provider: String,
    pub model: String,
    pub title: String,
    pub created: u64,
    pub updated: u64,
    pub messages: u64,
}

pub fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

#[derive(Debug, Clone)]
pub struct SessionStore {
    dir: PathBuf,
}

impl SessionStore {
    /// Opens (creating) `dir`, rebuilding the index from the session files if it is missing.
    pub fn open(dir: impl Into<PathBuf>) -> Result<Self, StoreError> {
        let store = Self { dir: dir.into() };
        create_private_dir(&store.dir)?;
        let fresh = !store.index_path().exists();
        store.index()?;
        if fresh {
            store.reindex()?;
        }
        Ok(store)
    }

    /// `ROZU_SESSIONS_DIR`, else the platform data dir (`~/Library/Application Support/dev.rozu.rozu/sessions`
    /// on macOS).
    pub fn default_dir() -> Option<PathBuf> {
        std::env::var_os(SESSIONS_DIR_ENV)
            .filter(|dir| !dir.is_empty())
            .map(PathBuf::from)
            .or_else(|| {
                let dirs = directories::ProjectDirs::from("dev", "rozu", "rozu")?;
                Some(dirs.data_dir().join("sessions"))
            })
    }

    pub fn open_default() -> Result<Self, StoreError> {
        let dir = Self::default_dir()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no home directory"))?;
        Self::open(dir)
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn path_of(&self, id: &str) -> PathBuf {
        self.dir.join(format!("{id}.jsonl"))
    }

    fn index_path(&self) -> PathBuf {
        self.dir.join(INDEX_FILE)
    }

    fn index(&self) -> Result<Connection, StoreError> {
        let conn = Connection::open(self.index_path())?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS sessions (
                id TEXT PRIMARY KEY, workspace TEXT NOT NULL, provider TEXT NOT NULL,
                model TEXT NOT NULL, title TEXT NOT NULL, created INTEGER NOT NULL,
                updated INTEGER NOT NULL, messages INTEGER NOT NULL);
             CREATE INDEX IF NOT EXISTS sessions_by_workspace ON sessions (workspace, updated);",
        )?;
        Ok(conn)
    }

    /// Starts a new session file.
    pub fn create(&self, meta: SessionMeta) -> Result<SessionWriter, StoreError> {
        let path = self.path_of(&meta.id);
        let file = open_private(&path)?;
        let mut writer = SessionWriter {
            store: self.clone(),
            file,
            meta,
            written: 0,
            compactions: 0,
            usage: (Usage::default(), 0.0, 0),
        };
        writer.append(&Record::Meta(writer.meta.clone()))?;
        writer.update_index(&Session::default())?;
        Ok(writer)
    }

    /// Reads a session back, and a writer that continues it.
    pub fn resume(&self, id: &str) -> Result<(Session, SessionWriter), StoreError> {
        let (meta, session) = self.load(id)?;
        let path = self.path_of(&meta.id);
        let writer = SessionWriter {
            store: self.clone(),
            file: open_private(&path)?,
            written: session.messages.len(),
            compactions: session.compactions,
            usage: (session.usage, session.cost_usd, session.context_tokens),
            meta,
        };
        Ok((session, writer))
    }

    /// Replays a session file. The meta reflects the latest `/model`.
    pub fn load(&self, id: &str) -> Result<(SessionMeta, Session), StoreError> {
        let path = self.path_of(id);
        let file = File::open(&path).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => StoreError::NotFound(id.to_owned()),
            _ => err.into(),
        })?;
        replay(&path, BufReader::new(file))
    }

    /// The session id `query` names: `last` (or nothing) is the newest in `workspace`, anything
    /// else an id or a unique id prefix.
    pub fn resolve(&self, query: Option<&str>, workspace: &Path) -> Result<String, StoreError> {
        let conn = self.index()?;
        match query.filter(|q| !q.is_empty() && *q != "last") {
            None => conn
                .query_row(
                    "SELECT id FROM sessions WHERE workspace = ?1 AND messages > 0
                     ORDER BY updated DESC, id DESC LIMIT 1",
                    [workspace.display().to_string()],
                    |row| row.get(0),
                )
                .optional()?
                .ok_or_else(|| StoreError::NotFound("last".into())),
            Some(prefix) => {
                let mut stmt = conn.prepare("SELECT id FROM sessions WHERE id LIKE ?1 || '%'")?;
                let ids: Vec<String> = stmt
                    .query_map([prefix], |row| row.get(0))?
                    .collect::<Result<_, _>>()?;
                match ids.as_slice() {
                    [id] => Ok(id.clone()),
                    [] => Err(StoreError::NotFound(prefix.to_owned())),
                    _ if ids.iter().any(|id| id == prefix) => Ok(prefix.to_owned()),
                    _ => Err(StoreError::Ambiguous(prefix.to_owned())),
                }
            }
        }
    }

    /// Newest first; `workspace` filters, `query` matches the title or id.
    pub fn list(
        &self,
        workspace: Option<&Path>,
        query: Option<&str>,
        limit: usize,
    ) -> Result<Vec<SessionSummary>, StoreError> {
        let conn = self.index()?;
        let mut stmt = conn.prepare(
            "SELECT id, workspace, provider, model, title, created, updated, messages
             FROM sessions
             WHERE messages > 0 AND (?1 IS NULL OR workspace = ?1)
               AND (?2 IS NULL OR title LIKE '%' || ?2 || '%' OR id LIKE ?2 || '%')
             ORDER BY updated DESC, id DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![
                workspace.map(|w| w.display().to_string()),
                query,
                limit as i64
            ],
            |row| {
                Ok(SessionSummary {
                    id: row.get(0)?,
                    workspace: PathBuf::from(row.get::<_, String>(1)?),
                    provider: row.get(2)?,
                    model: row.get(3)?,
                    title: row.get(4)?,
                    created: row.get::<_, i64>(5)? as u64,
                    updated: row.get::<_, i64>(6)? as u64,
                    messages: row.get::<_, i64>(7)? as u64,
                })
            },
        )?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Rebuilds the index from the session files (skipping unreadable ones).
    pub fn reindex(&self) -> Result<usize, StoreError> {
        let mut count = 0;
        for entry in fs::read_dir(&self.dir)? {
            let path = entry?.path();
            if path.extension().is_none_or(|ext| ext != "jsonl") {
                continue;
            }
            let Some(id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };
            let Ok((meta, session)) = self.load(id) else {
                continue;
            };
            let updated = fs::metadata(&path)?
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map_or(meta.created, |d| d.as_secs());
            upsert(&self.index()?, &meta, &session, updated)?;
            count += 1;
        }
        Ok(count)
    }
}

/// Appends one session's changes to its file and keeps its index row current.
#[derive(Debug)]
pub struct SessionWriter {
    store: SessionStore,
    file: File,
    meta: SessionMeta,
    /// Messages already in the file.
    written: usize,
    compactions: u32,
    usage: (Usage, f64, u64),
}

impl SessionWriter {
    pub fn id(&self) -> &str {
        &self.meta.id
    }

    pub fn meta(&self) -> &SessionMeta {
        &self.meta
    }

    pub fn path(&self) -> PathBuf {
        self.store.path_of(&self.meta.id)
    }

    /// Writes whatever `session` gained since the last sync: new messages, or the whole history
    /// after a compaction, and the usage totals.
    pub fn sync(&mut self, session: &Session) -> Result<(), StoreError> {
        if session.compactions != self.compactions || session.messages.len() < self.written {
            self.append(&Record::Checkpoint {
                messages: session.messages.clone(),
            })?;
            self.compactions = session.compactions;
        } else {
            for message in &session.messages[self.written..] {
                self.append(&Record::Message {
                    message: message.clone(),
                })?;
            }
        }
        self.written = session.messages.len();
        let usage = (session.usage, session.cost_usd, session.context_tokens);
        if usage != self.usage {
            self.append(&Record::Usage {
                usage: usage.0,
                cost_usd: usage.1,
                context_tokens: usage.2,
            })?;
            self.usage = usage;
        }
        self.update_index(session)
    }

    pub fn set_model(&mut self, provider: &str, model: &str) -> Result<(), StoreError> {
        self.meta.provider = provider.to_owned();
        self.meta.model = model.to_owned();
        self.append(&Record::Model {
            provider: provider.to_owned(),
            model: model.to_owned(),
        })
    }

    fn append(&mut self, record: &Record) -> Result<(), StoreError> {
        let mut line = serde_json::to_vec(record)?;
        line.push(b'\n');
        self.file.write_all(&line)?;
        Ok(())
    }

    fn update_index(&self, session: &Session) -> Result<(), StoreError> {
        upsert(&self.store.index()?, &self.meta, session, unix_now())
    }
}

fn upsert(
    conn: &Connection,
    meta: &SessionMeta,
    session: &Session,
    updated: u64,
) -> Result<(), StoreError> {
    conn.execute(
        "INSERT INTO sessions (id, workspace, provider, model, title, created, updated, messages)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET provider = ?3, model = ?4, title = ?5, updated = ?7,
           messages = ?8",
        params![
            meta.id,
            meta.workspace.display().to_string(),
            meta.provider,
            meta.model,
            title(session),
            meta.created as i64,
            updated as i64,
            session
                .messages
                .iter()
                .filter(|m| m.role != Role::System)
                .count() as i64,
        ],
    )?;
    Ok(())
}

/// The first user message, on one line.
fn title(session: &Session) -> String {
    let text = session
        .messages
        .iter()
        .find(|m| m.role == Role::User)
        .map(|m| m.text_content())
        .unwrap_or_default();
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= TITLE_CHARS {
        flat
    } else {
        format!("{}…", flat.chars().take(TITLE_CHARS).collect::<String>())
    }
}

fn replay(path: &Path, reader: impl BufRead) -> Result<(SessionMeta, Session), StoreError> {
    let mut meta = None;
    let mut session = Session::default();
    let mut lines = reader.lines().peekable();
    while let Some(line) = lines.next() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let record = match serde_json::from_str(&line) {
            Ok(record) => record,
            // A crash mid-write leaves a torn last line; everything before it is intact.
            Err(_) if lines.peek().is_none() => break,
            Err(err) => return Err(err.into()),
        };
        match record {
            Record::Meta(m) => meta = Some(m),
            Record::Message { message } => session.messages.push(message),
            Record::Usage {
                usage,
                cost_usd,
                context_tokens,
            } => {
                session.usage = usage;
                session.cost_usd = cost_usd;
                session.context_tokens = context_tokens;
            }
            Record::Model { provider, model } => {
                if let Some(meta) = meta.as_mut() {
                    meta.provider = provider;
                    meta.model = model;
                }
            }
            Record::Checkpoint { messages } => {
                session.messages = messages;
                session.compactions += 1;
            }
        }
    }
    let meta = meta.ok_or_else(|| StoreError::NoHeader(path.to_owned()))?;
    Ok((meta, session))
}

fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_private(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(test)]
mod tests {
    use rozu_core::{ContentPart, Message};

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
}
