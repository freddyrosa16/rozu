//! The app's link to rozu-core, built like `rozu chat` builds it: one agent and session per
//! chat, on a shared tokio runtime. Events and approval prompts come back as [`Update`]s.
//! Every chat is saved through [`rozu_store::SessionStore`], like `rozu chat`, so it can be
//! reopened after a restart.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};

use anyhow::{Context, anyhow};
use async_trait::async_trait;
use rozu_activity::ActivityEvent;
use rozu_cli_providers::codex::install::{find_codex_binary, list_models};
use rozu_core::{
    Agent, AgentEvent, ApprovalGate, ApprovalMode, ApprovalRequest, Decision, Session,
    new_session_id,
};
use rozu_providers::ModelChoice;
use rozu_runtime::{APP_HOST, AgentFactory, load_memory, set_system};
use rozu_store::{SessionMeta, SessionStore, SessionSummary, SessionWriter, unix_now};
use rozu_tools::Workspace;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::catalog::{Access, Live, access};

pub enum Update {
    Event(AgentEvent),
    Ask(ApprovalRequest, oneshot::Sender<Decision>),
    Failed { message: String, offline: bool },
}

pub struct Backend {
    rt: tokio::runtime::Runtime,
    /// Saved chats; `None` when the store can't be opened (chats then live only in memory).
    sessions: Option<SessionStore>,
}

/// A running chat: prompts go in, [`Update`]s come out on the channel given to [`Backend::start`].
pub struct Chat {
    /// The saved session's id (also the audit session id).
    pub session_id: String,
    prompts: mpsc::UnboundedSender<Prompt>,
    cancel: Mutex<CancellationToken>,
    worker: Option<tokio::task::JoinHandle<()>>,
}

struct Prompt {
    text: String,
    cancel: CancellationToken,
    selection: Selection,
}

#[derive(Clone, PartialEq)]
struct Selection {
    choice: ModelChoice,
    effort: Option<String>,
}

impl Chat {
    pub fn send(&self, text: String, choice: &ModelChoice, effort: Option<&str>) -> bool {
        let cancel = CancellationToken::new();
        *self.cancel.lock().expect("cancel lock") = cancel.clone();
        self.prompts
            .send(Prompt {
                text,
                cancel,
                selection: Selection {
                    choice: choice.clone(),
                    effort: effort.map(str::to_owned),
                },
            })
            .is_ok()
    }

    pub fn cancel(&self) {
        self.cancel.lock().expect("cancel lock").cancel();
    }

    async fn close(mut self) {
        let worker = self.worker.take();
        drop(self);
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }
}

impl Drop for Chat {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Sends each prompt to the UI and waits for the answer.
struct Gate(mpsc::UnboundedSender<Update>);

#[async_trait]
impl ApprovalGate for Gate {
    async fn decide(&self, req: ApprovalRequest) -> Decision {
        let (tx, rx) = oneshot::channel();
        let deny = |reason: &str| Decision::Deny {
            reason: reason.into(),
        };
        if self.0.send(Update::Ask(req, tx)).is_err() {
            return deny("the app closed");
        }
        rx.await.unwrap_or_else(|_| deny("the prompt was closed"))
    }
}

impl Backend {
    pub fn new() -> anyhow::Result<Self> {
        let sessions = SessionStore::open_default()
            .inspect_err(|err| eprintln!("rozu-app: chats won't be saved: {err}"))
            .ok();
        Ok(Self {
            rt: tokio::runtime::Runtime::new()?,
            sessions,
        })
    }

    /// Whether chats are saved to disk.
    pub fn saves(&self) -> bool {
        self.sessions.is_some()
    }

    /// The saved chats of a project folder, newest first.
    pub fn saved_chats(&self, root: &Path) -> Vec<SessionSummary> {
        let Some(store) = &self.sessions else {
            return Vec::new();
        };
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        store.list(Some(&root), None, 200).unwrap_or_default()
    }

    /// A saved chat's history, for showing it before it is continued.
    pub fn history(&self, id: &str) -> Option<Session> {
        self.sessions.as_ref()?.load(id).ok().map(|(_, s)| s)
    }

    /// Waits for the chat's final save before deleting its history and index entry.
    pub fn forget(&self, id: &str, chat: Option<Chat>) -> tokio::task::JoinHandle<()> {
        let id = id.to_owned();
        let store = self.sessions.clone();
        self.rt.spawn(async move {
            if let Some(chat) = chat {
                chat.close().await;
            }
            if let Some(store) = store
                && let Err(err) = store.delete(&id)
            {
                eprintln!("rozu-app: could not delete chat {id}: {err}");
            }
        })
    }

    /// The models the installed Codex CLI offers; `None` when it can't say.
    pub fn codex_models(&self) -> oneshot::Receiver<Live> {
        let (tx, rx) = oneshot::channel();
        self.rt.spawn(async move {
            let models = match find_codex_binary().filter(|p| p.exists()) {
                Some(bin) => list_models(&bin).await.ok(),
                None => None,
            };
            let _ = tx.send(
                models.map(|models| models.into_iter().map(|m| (m.id, m.display_name)).collect()),
            );
        });
        rx
    }

    /// Whether a subscription CLI is installed and signed in.
    pub fn check(&self, id: &'static str) -> oneshot::Receiver<Access> {
        let (tx, rx) = oneshot::channel();
        self.rt.spawn(async move {
            let _ = tx.send(access(id).await);
        });
        rx
    }

    /// Starts a chat in `root`, continuing the saved chat `resume` if given (its history is
    /// replayed into the new session) or creating a new saved one.
    pub fn start(
        &self,
        root: &Path,
        choice: &ModelChoice,
        effort: Option<String>,
        mode: ApprovalMode,
        resume: Option<&str>,
        updates: mpsc::UnboundedSender<Update>,
    ) -> anyhow::Result<Chat> {
        let (prompts, rx) = mpsc::unbounded_channel::<Prompt>();
        // Resume first: the agent's audit entries then share the saved chat's id.
        let resumed = resume
            .map(|id| {
                self.sessions
                    .as_ref()
                    .context("the saved chat store is unavailable")?
                    .resume(id)
                    .context("could not reopen the saved chat")
            })
            .transpose()?;
        let session_id = match &resumed {
            Some((_, writer)) => writer.id().to_owned(),
            _ => new_session_id(),
        };
        let selection = Selection {
            choice: choice.clone(),
            effort,
        };
        let gate: Arc<dyn ApprovalGate> = Arc::new(Gate(updates.clone()));
        let workspace = Workspace::new(root)
            .with_context(|| format!("cannot open the folder {}", root.display()))?;
        let mut factory = AgentFactory::new(&workspace, mode, gate, session_id.clone())?;
        let agent = {
            let _rt = self.rt.enter();
            build_agent(&mut factory, &selection)?
        };
        let (mut session, writer) = match resumed {
            Some((session, mut writer)) => {
                writer.set_model(choice.provider.id, &choice.model)?;
                (session, Some(writer))
            }
            None => {
                let writer = self.sessions.as_ref().and_then(|store| {
                    store
                        .create(SessionMeta {
                            id: session_id.clone(),
                            workspace: canonical(root),
                            provider: choice.provider.id.to_owned(),
                            model: choice.model.clone(),
                            created: unix_now(),
                        })
                        .inspect_err(|err| eprintln!("rozu-app: this chat won't be saved: {err}"))
                        .ok()
                });
                (Session::new(), writer)
            }
        };
        set_system(&mut session, root, mode, APP_HOST, &load_memory(root));
        let task = ChatTask {
            agent,
            selection,
            session,
            writer,
        };
        let worker = self.rt.spawn(task.run(rx, updates, move |selection| {
            build_agent(&mut factory, selection)
        }));
        Ok(Chat {
            session_id,
            prompts,
            cancel: Mutex::new(CancellationToken::new()),
            worker: Some(worker),
        })
    }
}

/// Owns a chat's session and writer across model changes and serializes all its turns.
struct ChatTask {
    agent: Agent,
    selection: Selection,
    session: Session,
    writer: Option<SessionWriter>,
}

impl ChatTask {
    async fn run(
        mut self,
        mut rx: mpsc::UnboundedReceiver<Prompt>,
        updates: mpsc::UnboundedSender<Update>,
        mut build: impl FnMut(&Selection) -> anyhow::Result<Agent>,
    ) {
        while let Some(prompt) = rx.recv().await {
            if self.selection != prompt.selection {
                match build(&prompt.selection) {
                    Ok(agent) => {
                        self.agent = agent;
                        self.selection = prompt.selection;
                        if let Some(writer) = self.writer.as_mut()
                            && let Err(err) = writer.set_model(
                                self.selection.choice.provider.id,
                                &self.selection.choice.model,
                            )
                        {
                            eprintln!("rozu-app: could not save the model choice: {err}");
                            self.writer = None;
                        }
                    }
                    Err(err) => {
                        let _ = updates.send(Update::Failed {
                            message: format!("{err:#}"),
                            offline: false,
                        });
                        continue;
                    }
                }
            }
            let (tx, mut events) = mpsc::channel(256);
            let cancel = prompt.cancel.clone();
            let run = async {
                let result = self
                    .agent
                    .run_turn(prompt.text, &mut self.session, &tx, prompt.cancel)
                    .await;
                drop(tx);
                result
            };
            let relay = async {
                while let Some(event) = events.recv().await {
                    if updates.send(Update::Event(event)).is_err() {
                        cancel.cancel();
                    }
                }
            };
            // Drain this turn's events before publishing its failure or taking another prompt.
            let (result, ()) = tokio::join!(run, relay);
            if let Some(w) = self.writer.as_mut()
                && let Err(err) = w.sync(&self.session)
            {
                eprintln!("rozu-app: could not save the chat: {err}");
                self.writer = None;
            }
            if let Err(err) = result {
                let offline = matches!(
                    ActivityEvent::from(&err),
                    ActivityEvent::TurnFailed { offline: true }
                );
                let _ = updates.send(Update::Failed {
                    message: err.to_string(),
                    offline,
                });
            }
        }
    }
}

fn build_agent(factory: &mut AgentFactory, selection: &Selection) -> anyhow::Result<Agent> {
    if selection.choice.provider.is_cli_backed() {
        factory.set_rozu_binary(rozu_binary()?);
    }
    factory.set_effort(selection.effort.clone());
    factory.build(&selection.choice)
}

fn canonical(root: &Path) -> PathBuf {
    root.canonicalize().unwrap_or_else(|_| root.to_path_buf())
}

/// `rozu` (for `rozu mcp-serve`): next to this binary, else on PATH. A `rozu` found on PATH
/// must report this app's version, so a stale install can't serve the tools.
fn rozu_binary() -> anyhow::Result<PathBuf> {
    let sibling = std::env::current_exe()?.with_file_name("rozu");
    if sibling.is_file() {
        return Ok(sibling);
    }
    let on_path = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("rozu"))
        .find(|p| p.is_file())
        .ok_or_else(|| anyhow!("the rozu CLI wasn't found: run `cargo build -p rozu-cli`"))?;
    let output = Command::new(&on_path)
        .arg("--version")
        .output()
        .with_context(|| format!("cannot run {}", on_path.display()))?;
    let reported = String::from_utf8_lossy(&output.stdout);
    let version = reported.split_whitespace().last().unwrap_or("");
    if version != env!("CARGO_PKG_VERSION") {
        return Err(anyhow!(
            "{} is rozu {version}, but this app is {}: rebuild both with `cargo build -p rozu-cli -p rozu-app`",
            on_path.display(),
            env!("CARGO_PKG_VERSION")
        ));
    }
    Ok(on_path)
}

#[cfg(test)]
mod tests;
