//! Context compaction. Two steps, each taken when the conversation passes a share of the
//! model's context window:
//!
//! 1. **Mask**: outputs of tool calls older than the recent turns are replaced by a one-line
//!    stub, all at once, so the prompt prefix changes once per batch rather than every turn.
//! 2. **Summarize**: everything between the leading system messages (system prompt and pinned
//!    memory, never touched) and the recent turns is replaced by a model-written summary.

use serde::{Deserialize, Serialize};

use crate::model::{ContentPart, Message, Role};

/// Starts the system message that holds a summary, so it can be told apart from pinned memory.
pub const SUMMARY_HEADER: &str = "Summary of the earlier conversation (written by rozu):";
/// Tool outputs shorter than this are never masked.
const MASK_MIN_BYTES: usize = 256;

pub const SUMMARY_PROMPT: &str = "You compact a coding agent's conversation. Summarize the \
transcript below for the agent that will continue it: the user's goals and constraints, decisions \
made, files read or changed (with paths), commands run and their results, and what is still open. \
Keep exact names, paths and error messages. Use terse bullet points. Don't add anything that \
isn't in the transcript.";

/// How rozu keeps a long conversation inside the context window. Eval conditions are named
/// presets of this, so the eval measures what ships.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MemoryPolicy {
    /// Mask old tool outputs past this share of the context window.
    pub mask_at: Option<f32>,
    /// Summarize older turns past this share.
    pub summarize_at: Option<f32>,
    /// User turns (with everything after them) always kept as they are.
    pub keep_turns: usize,
}

impl Default for MemoryPolicy {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl MemoryPolicy {
    pub const DEFAULT: Self = Self {
        mask_at: Some(0.6),
        summarize_at: Some(0.8),
        keep_turns: 3,
    };
    pub const OFF: Self = Self {
        mask_at: None,
        summarize_at: None,
        keep_turns: 3,
    };
    pub const PRESETS: [(&'static str, Self); 4] = [
        ("off", Self::OFF),
        (
            "mask",
            Self {
                summarize_at: None,
                ..Self::DEFAULT
            },
        ),
        (
            "summarize",
            Self {
                mask_at: None,
                ..Self::DEFAULT
            },
        ),
        ("default", Self::DEFAULT),
    ];

    pub fn preset(name: &str) -> Option<Self> {
        Self::PRESETS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, p)| *p)
    }
}

/// What a compaction did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Compaction {
    pub masked: usize,
    pub summarized: usize,
    pub tokens_before: u64,
    pub tokens_after: u64,
}

pub fn is_summary(message: &Message) -> bool {
    message.role == Role::System && message.text_content().starts_with(SUMMARY_HEADER)
}

/// Rough token count (4 bytes a token) for when no provider usage is known.
pub fn estimate_tokens(messages: &[Message]) -> u64 {
    let bytes: usize = messages
        .iter()
        .flat_map(|m| &m.content)
        .map(|part| match part {
            ContentPart::Text { text } => text.len(),
            ContentPart::ToolCall {
                name, args_json, ..
            } => name.len() + args_json.len(),
            ContentPart::ToolResult { output, .. } => output.len(),
        })
        .sum();
    (bytes / 4) as u64
}

/// Leading system messages that aren't a summary: the part compaction never changes.
pub fn pinned_len(messages: &[Message]) -> usize {
    messages
        .iter()
        .take_while(|m| m.role == Role::System && !is_summary(m))
        .count()
}

/// Index of the first kept message: the `keep` -th last user message (or the end, if there are
/// fewer older messages than that).
pub fn recent_start(messages: &[Message], keep: usize) -> usize {
    let users: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| m.role == Role::User)
        .map(|(i, _)| i)
        .collect();
    match keep {
        0 => messages.len(),
        _ if users.len() < keep => users.first().copied().unwrap_or(messages.len()),
        _ => users[users.len() - keep],
    }
}

/// Replaces long tool outputs before `end` with a stub. Returns how many it replaced.
pub fn mask_tool_outputs(messages: &mut [Message], end: usize) -> usize {
    let mut masked = 0;
    for message in &mut messages[..end] {
        for part in &mut message.content {
            if let ContentPart::ToolResult { output, .. } = part {
                if output.len() >= MASK_MIN_BYTES {
                    *output = format!(
                        "[output elided by rozu to save context: {} bytes; rerun the tool if needed]",
                        output.len()
                    );
                    masked += 1;
                }
            }
        }
    }
    masked
}

/// The messages a summary replaces, as a plain transcript.
pub fn transcript(messages: &[Message]) -> String {
    let mut out = String::new();
    for message in messages {
        let who = match message.role {
            Role::System => "Earlier summary",
            Role::User => "User",
            Role::Assistant => "Assistant",
            Role::Tool => "Tool",
        };
        for part in &message.content {
            let line = match part {
                ContentPart::Text { text } => format!("{who}: {text}"),
                ContentPart::ToolCall {
                    name, args_json, ..
                } => format!("Assistant called {name} {args_json}"),
                ContentPart::ToolResult {
                    output, is_error, ..
                } => format!(
                    "Tool result{}: {output}",
                    if *is_error { " (error)" } else { "" }
                ),
            };
            out.push_str(&line);
            out.push_str("\n\n");
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Vec<Message> {
        let call = Message::new(
            Role::Assistant,
            vec![ContentPart::ToolCall {
                id: "c".into(),
                name: "read_file".into(),
                args_json: "{}".into(),
            }],
        );
        vec![
            Message::system("sys"),
            Message::system("memory"),
            Message::user("one"),
            call.clone(),
            Message::tool_result("c", "x".repeat(1000), false),
            Message::assistant("done one"),
            Message::user("two"),
            call,
            Message::tool_result("c", "y".repeat(1000), false),
            Message::user("three"),
        ]
    }

    #[test]
    fn finds_the_pinned_prefix_and_recent_turns() {
        let mut messages = session();
        assert_eq!(pinned_len(&messages), 2);
        assert_eq!(recent_start(&messages, 2), 6);
        assert_eq!(recent_start(&messages, 3), 2);
        assert_eq!(recent_start(&messages, 9), 2);
        assert_eq!(recent_start(&messages, 0), messages.len());
        messages.insert(2, Message::system(format!("{SUMMARY_HEADER} x")));
        assert_eq!(pinned_len(&messages), 2);
        assert!(is_summary(&messages[2]));
    }

    #[test]
    fn masks_only_old_long_outputs() {
        let mut messages = session();
        let before = estimate_tokens(&messages);
        assert_eq!(mask_tool_outputs(&mut messages, 6), 1);
        assert!(messages[4].text_content().is_empty());
        let ContentPart::ToolResult { output, .. } = &messages[4].content[0] else {
            panic!()
        };
        assert!(output.contains("1000 bytes"));
        assert_eq!(messages[8].content, session()[8].content);
        assert!(estimate_tokens(&messages) < before - 200);
        assert_eq!(mask_tool_outputs(&mut messages, 6), 0);
    }

    #[test]
    fn presets() {
        assert_eq!(MemoryPolicy::preset("default"), Some(MemoryPolicy::DEFAULT));
        assert_eq!(MemoryPolicy::preset("mask").unwrap().summarize_at, None);
        assert!(MemoryPolicy::preset("nope").is_none());
        let text = transcript(&session()[2..5]);
        assert!(text.starts_with("User: one\n\nAssistant called read_file {}"));
    }
}
