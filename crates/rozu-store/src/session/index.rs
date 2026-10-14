//! SQLite projection of the session log, rebuilt when needed.

use rozu_core::{Role, Session};
use rusqlite::{Connection, params};

use super::{SessionMeta, StoreError};

const TITLE_CHARS: usize = 80;

pub(super) fn upsert(
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
