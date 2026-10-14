//! The common system prompt and project memory for new and resumed chats.

use std::path::Path;

use rozu_core::{ApprovalMode, Message, Session};
use rozu_store::ProjectMemory;

/// Where the agent runs, for the system prompt: `rozu chat`.
pub const TERMINAL_HOST: &str = "the user's terminal";
/// Where the agent runs, for the system prompt: the desktop app.
pub const APP_HOST: &str = "the rozu desktop app";

/// The system prompt: the workspace, the tools, how commands are confined, the approval mode.
/// `host` is where the agent runs ([`TERMINAL_HOST`], [`APP_HOST`]).
pub fn system_prompt(root: &Path, mode: ApprovalMode, host: &str) -> String {
    format!(
        "You are rozu, a coding agent running in {host}.\n\
         The workspace root is {}; tool paths are relative to it.\n\
         Tools: read_file, list_dir and grep to look at files; apply_patch to edit a file \
         (exact search/replace edits or a unified diff) and write_file to create one; shell to \
         run commands (non-interactive, with a timeout). On macOS shell commands use Seatbelt, \
         which allows writes in the workspace, private $TMPDIR and explicitly approved outside \
         paths; network is blocked unless granted. Linux has no OS sandbox. Set network: true \
         when a command needs it.\n\
         Approval mode: {mode}. Calls that need approval are shown to the user, who may deny \
         them with a reason; respect it and adapt. Destructive commands (rm -rf, force push, \
         git reset --hard, ...), interpreter one-liners (python -c, eval, ...) and output \
         redirects always need approval, so prefer safer alternatives such as write_file and \
         apply_patch.\n\
         Read files before editing them, keep changes minimal, and run the relevant tests after \
         changing code. Be concise, and cite file paths when you refer to code.",
        root.display()
    )
}

/// The project memory of `root`; unreadable files are logged and skipped.
pub fn load_memory(root: &Path) -> ProjectMemory {
    ProjectMemory::load(root).unwrap_or_else(|err| {
        tracing::warn!(error = %err, "could not read the project memory");
        ProjectMemory::default()
    })
}

/// Replaces the leading system messages with the current system prompt and project memory, so
/// a resumed chat follows today's ROZU.md.
pub fn set_system(
    session: &mut Session,
    root: &Path,
    mode: ApprovalMode,
    host: &str,
    memory: &ProjectMemory,
) {
    let old = rozu_core::compact::pinned_len(&session.messages);
    let mut system = vec![Message::system(system_prompt(root, mode, host))];
    system.extend(memory.prompt().map(Message::system));
    session.messages.splice(..old, system);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refreshing_empty_memory_removes_the_old_prefix_and_preserves_the_conversation() {
        let conversation = vec![
            Message::user("keep this request"),
            Message::assistant("keep this answer"),
            Message::system("keep this later system message"),
        ];
        let mut session = Session::with_system_prompt("old prompt");
        session.messages.push(Message::system("old memory"));
        session.messages.extend(conversation.clone());

        set_system(
            &mut session,
            Path::new("/workspace"),
            ApprovalMode::AutoWorkspace,
            APP_HOST,
            &ProjectMemory::default(),
        );

        assert_eq!(session.messages.len(), 1 + conversation.len());
        assert_eq!(session.messages[1..], conversation);
        let prompt = session.messages[0].text_content();
        assert!(prompt.contains(APP_HOST));
        assert!(prompt.contains("/workspace"));
        assert!(!prompt.contains("old memory"));
    }

    #[test]
    fn the_system_prompt_is_replaced_and_memory_follows_it() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ROZU.md"), "use tabs").unwrap();
        let memory = load_memory(dir.path());
        let mut session = Session::with_system_prompt("old prompt");
        session.messages.push(Message::user("hi"));
        set_system(
            &mut session,
            dir.path(),
            ApprovalMode::ReadOnly,
            APP_HOST,
            &memory,
        );
        assert_eq!(session.messages.len(), 3);
        let first = session.messages[0].text_content();
        assert!(first.contains("Approval mode: read-only"), "{first}");
        assert!(first.contains(APP_HOST), "{first}");
        assert!(session.messages[1].text_content().contains("use tabs"));
        assert_eq!(session.messages[2].text_content(), "hi");
        // Again: still one prompt and one memory message.
        set_system(
            &mut session,
            dir.path(),
            ApprovalMode::FullAccess,
            TERMINAL_HOST,
            &memory,
        );
        assert_eq!(session.messages.len(), 3);
    }
}
