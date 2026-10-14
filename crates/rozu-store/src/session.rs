//! Sessions on disk: `<dir>/<id>.jsonl` (append-only) plus `<dir>/index.sqlite`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rozu_core::{Session, Usage};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

/// Overrides where sessions are stored.
pub const SESSIONS_DIR_ENV: &str = "ROZU_SESSIONS_DIR";
const INDEX_FILE: &str = "index.sqlite";
mod index;
mod log;

use index::upsert;
pub use log::Record;

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
    #[error("invalid session id `{0}`")]
    InvalidId(String),
    #[error("session header id `{actual}` does not match file id `{expected}`")]
    MismatchedId { expected: String, actual: String },
    #[error("session `{0}` is already open for writing; close its other chat first")]
    InUse(String),
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

    fn checked_path(&self, id: &str) -> Result<PathBuf, StoreError> {
        if id.is_empty() || matches!(id, "." | "..") || id.contains(['/', '\\', '\0']) {
            return Err(StoreError::InvalidId(id.to_owned()));
        }
        Ok(self.path_of(id))
    }

    fn read(&self, id: &str) -> Result<log::LoadedSession, StoreError> {
        let path = self.checked_path(id)?;
        let file = File::open(&path).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => StoreError::NotFound(id.to_owned()),
            _ => err.into(),
        })?;
        self.read_from(id, &file)
    }

    fn read_from(&self, id: &str, file: &File) -> Result<log::LoadedSession, StoreError> {
        let loaded = log::replay(&self.path_of(id), BufReader::new(file.try_clone()?))?;
        if loaded.meta.id != id {
            return Err(StoreError::MismatchedId {
                expected: id.to_owned(),
                actual: loaded.meta.id,
            });
        }
        Ok(loaded)
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
        let path = self.checked_path(&meta.id)?;
        let file = open_private(&path, true)?;
        lock_writer(&file, &meta.id)?;
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
        let path = self.checked_path(id)?;
        let mut file = open_private(&path, false).map_err(|err| match err.kind() {
            io::ErrorKind::NotFound => StoreError::NotFound(id.to_owned()),
            _ => err.into(),
        })?;
        lock_writer(&file, id)?;
        let loaded = self.read_from(id, &file)?;
        if loaded.valid_bytes < file.metadata()?.len() {
            file.set_len(loaded.valid_bytes)?;
        }
        if loaded.needs_newline {
            file.write_all(b"\n")?;
        }
        let (meta, session) = (loaded.meta, loaded.session);
        let writer = SessionWriter {
            store: self.clone(),
            file,
            written: session.messages.len(),
            compactions: session.compactions,
            usage: (session.usage, session.cost_usd, session.context_tokens),
            meta,
        };
        Ok((session, writer))
    }

    /// Replays a session file. The meta reflects the latest `/model`.
    pub fn load(&self, id: &str) -> Result<(SessionMeta, Session), StoreError> {
        self.read(id).map(|loaded| (loaded.meta, loaded.session))
    }

    /// Replays a session file from anywhere (e.g. a recorded eval fixture).
    pub fn load_file(path: &Path) -> Result<(SessionMeta, Session), StoreError> {
        log::replay(path, BufReader::new(File::open(path)?))
            .map(|loaded| (loaded.meta, loaded.session))
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
                let mut stmt =
                    conn.prepare("SELECT id FROM sessions WHERE substr(id, 1, length(?1)) = ?1")?;
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

    /// Removes a saved session: its file and its index row. A missing file is not an error.
    pub fn delete(&self, id: &str) -> Result<(), StoreError> {
        let path = self.checked_path(id)?;
        let file = match open_private(&path, false) {
            Ok(file) => {
                lock_writer(&file, id)?;
                fs::remove_file(&path)?;
                Some(file)
            }
            Err(err) if err.kind() == io::ErrorKind::NotFound => None,
            Err(err) => return Err(err.into()),
        };
        self.index()?
            .execute("DELETE FROM sessions WHERE id = ?1", [id])?;
        drop(file);
        Ok(())
    }

    /// Rebuilds the index from the session files (skipping unreadable ones).
    pub fn reindex(&self) -> Result<usize, StoreError> {
        let mut conn = self.index()?;
        let tx = conn.transaction()?;
        tx.execute("DELETE FROM sessions", [])?;
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
            upsert(&tx, &meta, &session, updated)?;
            count += 1;
        }
        tx.commit()?;
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

fn create_private_dir(dir: &Path) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

fn open_private(path: &Path, create: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(create).read(true).append(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    options.open(path)
}

#[cfg(unix)]
fn lock_writer(file: &File, id: &str) -> Result<(), StoreError> {
    use std::os::fd::AsRawFd;

    // The lock belongs to the open file and is released on close, including process crashes.
    let result = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
    if result == 0 {
        return Ok(());
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::WouldBlock {
        Err(StoreError::InUse(id.to_owned()))
    } else {
        Err(error.into())
    }
}

#[cfg(not(unix))]
fn lock_writer(_file: &File, _id: &str) -> Result<(), StoreError> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "session writing requires Unix file locking",
    )
    .into())
}

#[cfg(test)]
mod tests;
