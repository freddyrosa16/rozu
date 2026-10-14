mod common;

use std::sync::Arc;
use std::sync::atomic::Ordering;

use common::*;
use rozu_core::compact::{SUMMARY_HEADER, SUMMARY_PROMPT};
use rozu_core::{
    Agent, AgentConfig, ContentPart, MemoryPolicy, Message, ModelInfo, ProviderError, Role,
    Session, TurnOutcome,
};
use rozu_tools::{ToolRegistry, Workspace};

const MEMORY: &str = "Project memory: always run cargo test.";

fn make_agent(provider: Arc<ScriptedProvider>, memory: MemoryPolicy) -> (tempfile::TempDir, Agent) {
    let dir = tempfile::tempdir().unwrap();
    let mut config = AgentConfig::new("m")
        .with_model_info(Some(ModelInfo::subscription("scripted", "m", 2_000)));
    config.memory = memory;
    let agent = Agent::new(
        provider,
        Arc::new(ToolRegistry::new()),
        Workspace::new(dir.path()).unwrap(),
        config,
    );
    (dir, agent)
}

/// System prompt, pinned memory, then `turns` long turns with a big tool result each.
fn long_session(turns: usize) -> Session {
    let mut session = Session::with_system_prompt("You are rozu.");
    session.messages.push(Message::system(MEMORY));
    for i in 0..turns {
        session
            .messages
            .push(Message::user(format!("question {i}")));
        session.messages.push(Message::new(
            Role::Assistant,
            vec![ContentPart::ToolCall {
                id: format!("c{i}"),
                name: "read_file".into(),
                args_json: "{}".into(),
            }],
        ));
        session.messages.push(Message::tool_result(
            format!("c{i}"),
            "x".repeat(2_000),
            false,
        ));
        session.messages.push(Message::assistant(format!(
            "answer {i} {}",
            "y".repeat(500)
        )));
    }
    session
}

#[tokio::test]
async fn a_long_session_compacts_without_losing_pinned_memory() {
    let provider = ScriptedProvider::new([reply("- asked 0 and 1; read files"), reply("ok")]);
    let (_dir, agent) = make_agent(provider.clone(), MemoryPolicy::DEFAULT);
    let mut session = long_session(5);

    let (outcome, _) = run_turn(&agent, &mut session, "next", |_, _| {}).await;
    assert_eq!(outcome.unwrap(), TurnOutcome::Completed);

    let requests = provider.requests();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].messages[0].text_content(), SUMMARY_PROMPT);
    assert!(requests[0].tools.is_empty());
    let transcript = requests[0].messages[1].text_content();
    assert!(transcript.contains("User: question 0") && transcript.contains("output elided"));
    assert!(!transcript.contains("question 3"));

    let sent = &requests[1].messages;
    assert_eq!(sent[0].text_content(), "You are rozu.");
    assert_eq!(sent[1].text_content(), MEMORY);
    assert!(sent[2].text_content().starts_with(SUMMARY_HEADER));
    assert!(
        sent[2]
            .text_content()
            .ends_with("- asked 0 and 1; read files")
    );
    assert_eq!(sent[3].text_content(), "question 3");
    assert_eq!(sent.last().unwrap().text_content(), "next");

    let done = session.last_compaction.unwrap();
    assert_eq!((done.masked, done.summarized), (3, 12));
    assert!(done.tokens_after < done.tokens_before / 2, "{done:?}");
    assert_eq!(session.compactions, 1);
    assert_eq!(session.messages.last().unwrap().text_content(), "ok");

    // Next turn: question 3 leaves the recent turns, so its output is masked; no new summary.
    let provider2 = ScriptedProvider::new([reply("fine")]);
    let (_dir2, agent2) = make_agent(provider2.clone(), MemoryPolicy::DEFAULT);
    run_turn(&agent2, &mut session, "more", |_, _| {})
        .await
        .0
        .unwrap();
    let done = session.last_compaction.unwrap();
    assert_eq!((done.masked, done.summarized), (1, 0));
    assert_eq!(provider2.requests().len(), 1);
    assert_eq!(session.messages[1].text_content(), MEMORY);
}

#[tokio::test]
async fn a_failed_summary_still_masks_and_the_turn_goes_on() {
    let provider = ScriptedProvider::new([
        Script::Events(vec![Err(ProviderError::Network("down".into()))]),
        reply("ok"),
    ]);
    let (_dir, agent) = make_agent(provider.clone(), MemoryPolicy::DEFAULT);
    let mut session = long_session(5);
    let (outcome, _) = run_turn(&agent, &mut session, "next", |_, _| {}).await;
    assert_eq!(outcome.unwrap(), TurnOutcome::Completed);
    let done = session.last_compaction.unwrap();
    assert_eq!((done.masked, done.summarized), (3, 0));
    assert_eq!(session.messages[1].text_content(), MEMORY);
}

#[tokio::test]
async fn presets_off_and_agent_backends_never_compact() {
    for backend in [false, true] {
        let provider = ScriptedProvider::new([reply("ok")]);
        provider.backend.store(backend, Ordering::SeqCst);
        let policy = if backend {
            MemoryPolicy::DEFAULT
        } else {
            MemoryPolicy::OFF
        };
        let (_dir, agent) = make_agent(provider.clone(), policy);
        let mut session = long_session(5);
        let before = session.messages.len();
        run_turn(&agent, &mut session, "next", |_, _| {})
            .await
            .0
            .unwrap();
        assert_eq!(provider.requests().len(), 1);
        assert_eq!(session.last_compaction, None);
        assert_eq!(session.messages.len(), before + 2);
    }
}

#[tokio::test]
async fn manual_compaction_ignores_thresholds() {
    let provider = ScriptedProvider::new([reply("- summary")]);
    let (_dir, agent) = make_agent(provider, MemoryPolicy::OFF);
    let mut session = long_session(4);
    let done = agent
        .compact(&mut session, tokio_util::sync::CancellationToken::new())
        .await;
    assert_eq!((done.masked, done.summarized), (1, 4));
    assert_eq!(session.messages.len(), 2 + 1 + 12);
}
