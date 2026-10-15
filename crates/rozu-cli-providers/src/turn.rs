//! What a CLI-backed provider sends for one user turn.

use rozu_core::{ChatRequest, Role};

/// The text to send for this turn: the newest user message, preceded by the earlier
/// conversation when the CLI hasn't seen it (a fresh session after talking to another model, or
/// after the CLI was restarted).
pub fn turn_text(req: &ChatRequest, cli_has_history: bool) -> String {
    turn_text_with(
        req,
        cli_has_history,
        "Earlier in this conversation (with another model):",
    )
}

/// [`turn_text`] with the earlier conversation introduced by `header`.
pub fn turn_text_with(req: &ChatRequest, cli_has_history: bool, header: &str) -> String {
    let mut turns = req
        .messages
        .iter()
        .filter(|m| matches!(m.role, Role::User | Role::Assistant))
        .map(|m| (m.role, m.text_content()))
        .filter(|(_, text)| !text.is_empty());
    if cli_has_history {
        return match turns.next_back() {
            Some((Role::User, text)) => text,
            _ => String::new(),
        };
    }
    let mut turns: Vec<_> = turns.collect();
    let latest = match turns.pop() {
        Some((Role::User, text)) => text,
        Some(other) => {
            turns.push(other);
            String::new()
        }
        None => String::new(),
    };
    if turns.is_empty() {
        return latest;
    }
    let mut text = format!("{header}\n");
    for (role, said) in turns {
        let who = if role == Role::User {
            "User"
        } else {
            "Assistant"
        };
        text.push_str(&format!("\n{who}: {said}\n"));
    }
    text.push_str(&format!("\nThe user's new message:\n{latest}"));
    text
}

/// The session's system messages followed by `note` (how to reach rozu's tools).
pub fn system_prompt(req: &ChatRequest, note: &str) -> String {
    let mut prompt: Vec<String> = req
        .messages
        .iter()
        .filter(|m| m.role == Role::System)
        .map(|m| m.text_content())
        .collect();
    prompt.push(note.to_owned());
    prompt.join("\n\n")
}

#[cfg(test)]
mod tests {
    use rozu_core::{ContentPart, Message};

    use super::*;

    fn request(messages: Vec<Message>) -> ChatRequest {
        ChatRequest {
            model: "sonnet".into(),
            messages,
            tools: Vec::new(),
            max_output_tokens: None,
        }
    }

    #[test]
    fn turn_text_replays_history_only_for_a_fresh_session() {
        let req = request(vec![
            Message::system("be brief"),
            Message::user("first"),
            Message::assistant("answer"),
            Message::user("second"),
        ]);
        assert_eq!(turn_text(&req, true), "second");
        let fresh = turn_text(&req, false);
        assert!(fresh.contains("User: first") && fresh.contains("Assistant: answer"));
        assert!(
            fresh.ends_with("The user's new message:\nsecond"),
            "{fresh}"
        );
        let single = request(vec![Message::system("s"), Message::user("only")]);
        assert_eq!(turn_text(&single, false), "only");
    }

    #[test]
    fn turn_text_handles_empty_and_non_user_endings() {
        let cases = [
            (Vec::new(), "", ""),
            (
                vec![
                    Message::system("system"),
                    Message::tool_result("t", "output", false),
                ],
                "",
                "",
            ),
            (vec![Message::user(" \n")], " \n", " \n"),
            (
                vec![Message::user("first"), Message::assistant("answer")],
                "",
                "History:\n\nUser: first\n\nAssistant: answer\n\nThe user's new message:\n",
            ),
            (
                vec![
                    Message::user("latest"),
                    Message::assistant(""),
                    Message::user(""),
                    Message::system("ignored"),
                    Message::tool_result("t", "ignored", false),
                ],
                "latest",
                "latest",
            ),
            (
                vec![Message::user("first"), Message::assistant(" \n")],
                "",
                "History:\n\nUser: first\n\nAssistant:  \n\n\nThe user's new message:\n",
            ),
        ];
        for (messages, continuing, fresh) in cases {
            let req = request(messages);
            assert_eq!(
                turn_text_with(&req, true, "History:"),
                continuing,
                "{req:?}"
            );
            assert_eq!(turn_text_with(&req, false, "History:"), fresh, "{req:?}");
        }
    }

    #[test]
    fn turn_text_concatenates_text_parts_and_preserves_replay_format() {
        let call = ContentPart::ToolCall {
            id: "t".into(),
            name: "read_file".into(),
            args_json: "{}".into(),
        };
        let req = request(vec![
            Message::system("ignored"),
            Message::user("first"),
            Message::assistant("answer\ncontinued"),
            Message::tool_result("t", "ignored", false),
            Message::new(
                Role::User,
                vec![
                    ContentPart::Text { text: "new".into() },
                    call.clone(),
                    ContentPart::Text {
                        text: " message".into(),
                    },
                ],
            ),
            Message::new(Role::Assistant, vec![call]),
        ]);
        assert_eq!(turn_text_with(&req, true, "History:"), "new message");
        assert_eq!(
            turn_text_with(&req, false, "History:"),
            "History:\n\nUser: first\n\nAssistant: answer\ncontinued\n\nThe user's new message:\nnew message"
        );
    }

    #[test]
    fn system_prompt_appends_the_note() {
        let prompt = system_prompt(&request(vec![Message::system("You are rozu.")]), "note");
        assert_eq!(prompt, "You are rozu.\n\nnote");
    }
}
