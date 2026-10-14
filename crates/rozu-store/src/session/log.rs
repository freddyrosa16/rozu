//! JSONL serialization and crash recovery, independent of the search index.

use std::io::BufRead;
use std::path::Path;

use rozu_core::{Message, Session, Usage};
use serde::{Deserialize, Serialize};

use super::{SessionMeta, StoreError};

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

pub(super) struct LoadedSession {
    pub meta: SessionMeta,
    pub session: Session,
    pub valid_bytes: u64,
    pub needs_newline: bool,
}

pub(super) fn replay(path: &Path, mut reader: impl BufRead) -> Result<LoadedSession, StoreError> {
    let mut meta = None;
    let mut session = Session::default();
    let mut valid_bytes = 0;
    let mut needs_newline = false;
    let mut line = Vec::new();
    loop {
        line.clear();
        let read = reader.read_until(b'\n', &mut line)?;
        if read == 0 {
            break;
        }
        if line.iter().all(u8::is_ascii_whitespace) {
            valid_bytes += read as u64;
            needs_newline = !line.ends_with(b"\n");
            continue;
        }
        let record = match serde_json::from_slice(&line) {
            Ok(record) => record,
            // A crash may leave an incomplete final record. Resume truncates only that tail.
            Err(_) if reader.fill_buf()?.is_empty() => break,
            Err(err) => return Err(err.into()),
        };
        valid_bytes += read as u64;
        needs_newline = !line.ends_with(b"\n");
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
    Ok(LoadedSession {
        meta,
        session,
        valid_bytes,
        needs_newline,
    })
}
