//! Provider discovery, setup commands and display labels shared by the app's views.

use std::process::Command;

use rozu_cli_providers::claude::install::{claude_auth_status, find_claude_binary};
use rozu_cli_providers::codex::install::{LoginStatus, codex_login_status, find_codex_binary};
use rozu_cli_providers::grok::install::find_grok_binary;
use rozu_core::ApprovalMode;
use rozu_providers::registry::{self, CLAUDE_CLI_ID, CODEX_ID, GROK_ID};
use rozu_providers::{ModelChoice, PROVIDERS, ProviderInfo};
pub use rozu_runtime::{approval_arg, approval_mode};
use rozu_secrets::{KeyringStore, resolve_api_key};

/// A subscription the app can sign in with: its CLI, and how to install it and sign in.
pub struct Plan {
    pub id: &'static str,
    pub name: &'static str,
    pub plan: &'static str,
    pub install: &'static str,
    pub login: &'static str,
}

pub const PLANS: &[Plan] = &[
    Plan {
        id: CLAUDE_CLI_ID,
        name: "Claude",
        plan: "Claude Pro or Max, through Claude Code",
        install: "curl -fsSL https://claude.ai/install.sh | bash",
        login: "claude auth login",
    },
    Plan {
        id: CODEX_ID,
        name: "ChatGPT",
        plan: "ChatGPT Plus or Pro, through Codex",
        install: "npm install -g @openai/codex",
        login: "codex login",
    },
    Plan {
        id: GROK_ID,
        name: "Grok",
        plan: "SuperGrok, through Grok Build",
        install: "npm install -g @xai-official/grok",
        login: "grok login",
    },
];

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Checking,
    Missing,
    SignedOut,
    Ready,
    /// Installed; Grok Build can't say whether it is signed in.
    Installed,
}

pub async fn access(id: &str) -> Access {
    match id {
        CLAUDE_CLI_ID => match find_claude_binary().filter(|p| p.exists()) {
            None => Access::Missing,
            Some(bin) => match claude_auth_status(&bin).await.map(|s| s.logged_in) {
                Ok(Some(false)) => Access::SignedOut,
                Ok(Some(true)) => Access::Ready,
                _ => Access::Installed,
            },
        },
        CODEX_ID => match find_codex_binary().filter(|p| p.exists()) {
            None => Access::Missing,
            Some(bin) => match codex_login_status(&bin).await {
                Ok(LoginStatus::NotLoggedIn) => Access::SignedOut,
                Ok(LoginStatus::ChatGpt | LoginStatus::ApiKey) => Access::Ready,
                _ => Access::Installed,
            },
        },
        _ => match find_grok_binary() {
            Ok(p) if p.exists() => Access::Installed,
            _ => Access::Missing,
        },
    }
}

/// Runs `command` in a new Terminal window (installs and sign-ins need one). The command
/// reaches AppleScript through an environment variable, so no quoting of it is needed.
pub fn in_terminal(command: &str) {
    const SCRIPT: &str = "tell application \"Terminal\"\nactivate\ndo script (system attribute \"ROZU_TERMINAL_COMMAND\")\nend tell";
    let _ = Command::new("osascript")
        .args(["-e", SCRIPT])
        .env("ROZU_TERMINAL_COMMAND", command)
        .spawn();
}

/// The approval modes the app offers, with a short description each.
pub const APPROVAL_MODES: &[(ApprovalMode, &str, &str)] = &[
    (
        ApprovalMode::ReadOnly,
        "Read-only",
        "Only reads run on their own; every change asks.",
    ),
    (
        ApprovalMode::AutoWorkspace,
        "Auto",
        "Reads and sandboxed edits in the folder run on their own; network, outside writes and risky commands ask.",
    ),
    (
        ApprovalMode::FullAccess,
        "Full",
        "Everything runs without asking, still sandboxed, except destructive commands, inline code and redirects.",
    ),
];

/// The providers this Mac can use: installed subscription CLIs first, then API keys that are set.
pub fn providers() -> Vec<&'static ProviderInfo> {
    let store = KeyringStore::default();
    let mut all: Vec<_> = PROVIDERS
        .iter()
        .filter(|p| match p.id {
            CLAUDE_CLI_ID => find_claude_binary().is_some_and(|p| p.exists()),
            CODEX_ID => find_codex_binary().is_some_and(|p| p.exists()),
            GROK_ID => find_grok_binary().is_ok_and(|p| p.exists()),
            id => matches!(
                resolve_api_key(&store, id, |v| std::env::var(v).ok()),
                Ok(Some(_))
            ),
        })
        .collect();
    all.sort_by_key(|p| !p.is_cli_backed());
    all
}

/// Models a CLI reported itself (Codex's `model/list`): id and display name.
pub type Live = Vec<(String, String)>;

/// Every model `provider` offers; Codex's own list when it gave one.
pub fn models(provider: &ProviderInfo, live: &Live) -> Vec<ModelChoice> {
    let ids: Vec<String> = if provider.id == CODEX_ID {
        // The current lineup, plus whatever else this Codex build lists.
        let mut ids: Vec<String> = registry::CODEX_STATIC_MODELS
            .iter()
            .map(|id| (*id).to_owned())
            .collect();
        for (id, _) in live {
            if !ids.contains(id) {
                ids.push(id.clone());
            }
        }
        ids
    } else if provider.id == CLAUDE_CLI_ID {
        claude_ids()
    } else {
        registry::models_for(provider.id)
            .into_iter()
            .map(|m| m.id)
            .collect()
    };
    ids.iter()
        .filter_map(|id| registry::resolve(Some(provider.id), Some(id)).ok())
        .collect()
}

/// The newest Claude model of each family, by its full id so the version shows.
fn claude_ids() -> Vec<String> {
    let mut ids: Vec<String> = Vec::new();
    for id in registry::models_for("anthropic").into_iter().map(|m| m.id) {
        let family = id.split('-').nth(1).map(str::to_owned);
        if !ids.iter().any(|i| i.split('-').nth(1) == family.as_deref()) {
            ids.push(id);
        }
    }
    ids
}

/// Swaps a Claude CLI alias (`opus`) saved by older builds for the model it stands for.
pub fn pin(mut choice: ModelChoice) -> ModelChoice {
    if choice.provider.id == CLAUDE_CLI_ID
        && let Some(id) = claude_ids()
            .into_iter()
            .find(|id| id.starts_with(&format!("claude-{}-", choice.model)))
    {
        choice = registry::resolve(Some(CLAUDE_CLI_ID), Some(&id)).unwrap_or(choice);
    }
    choice
}

/// The effort levels a provider takes; empty when rozu can't set it.
pub fn efforts(provider: &str) -> &'static [&'static str] {
    match provider {
        CLAUDE_CLI_ID => &["low", "medium", "high", "xhigh", "max"],
        CODEX_ID => &["low", "medium", "high", "xhigh"],
        _ => &[],
    }
}

/// How a provider is named in the app.
pub fn provider_name(provider: &ProviderInfo) -> &'static str {
    PLANS
        .iter()
        .find(|p| p.id == provider.id)
        .map_or(provider.label, |p| p.name)
}

/// How a model is named in the app: the CLI's own name, else `opus` → `Opus`,
/// `gpt-6.1-sol` → `GPT-6.1 Sol`, `claude-opus-5-5` → `Opus 5.5`.
pub fn model_name(model: &str, live: &Live) -> String {
    if let Some((_, name)) = live.iter().find(|(id, _)| id == model) {
        return name.clone();
    }
    let model = model.strip_prefix("claude-").unwrap_or(model);
    let mut words: Vec<String> = Vec::new();
    for part in model.split('-') {
        let short_num = |s: &str| s.len() <= 2 && s.chars().all(|c| c.is_ascii_digit());
        match words.last_mut() {
            Some(last) if short_num(part) && short_num(last.rsplit('.').next().unwrap_or("")) => {
                last.push('.');
                last.push_str(part);
            }
            Some(last) if last == "GPT" => {
                last.push('-');
                last.push_str(part);
            }
            _ => words.push(match part {
                "gpt" => "GPT".to_owned(),
                _ => {
                    let mut c = part.chars();
                    c.next()
                        .map(|f| f.to_uppercase().chain(c).collect())
                        .unwrap_or_default()
                }
            }),
        }
    }
    words.join(" ")
}

/// How the composer names a model: `Claude Opus · high`.
pub fn label(choice: &ModelChoice, effort: Option<&str>, live: &Live) -> String {
    let name = provider_name(choice.provider);
    let model = match choice.model.as_str() {
        "default" => name.to_owned(),
        model => format!("{name} {}", model_name(model, live)),
    };
    match effort {
        Some(effort) => format!("{model} · {effort}"),
        None => model,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_models_like_the_app() {
        let none = Live::new();
        let opus = pin(registry::resolve(Some(CLAUDE_CLI_ID), Some("opus")).unwrap());
        assert_eq!(label(&opus, Some("high"), &none), "Claude Opus 5.5 · high");
        let codex = registry::resolve(Some(CODEX_ID), None).unwrap();
        assert_eq!(label(&codex, None, &none), "ChatGPT");
        for (id, name) in [
            ("claude-opus-5-5", "Opus 5.5"),
            ("gpt-6.1-sol", "GPT-6.1 Sol"),
            ("grok-build-0.1", "Grok Build 0.1"),
            ("default", "Default"),
        ] {
            assert_eq!(model_name(id, &none), name);
        }
        let live = vec![("gpt-7".to_owned(), "GPT-7 Max".to_owned())];
        assert_eq!(model_name("gpt-7", &live), "GPT-7 Max");
        let codex = registry::provider_info(CODEX_ID).unwrap();
        let ids: Vec<_> = models(codex, &live).into_iter().map(|m| m.model).collect();
        assert_eq!(
            ids,
            [
                "default",
                "gpt-6.1-sol",
                "gpt-6-astra",
                "gpt-6-luna",
                "gpt-7"
            ]
        );
        let names: Vec<_> = models(registry::provider_info(CLAUDE_CLI_ID).unwrap(), &none)
            .iter()
            .map(|m| model_name(&m.model, &none))
            .collect();
        assert_eq!(names, ["Fable 5.1", "Opus 5.5", "Sonnet 5.5", "Haiku 5.5"]);
        assert!(efforts(GROK_ID).is_empty());
    }

    #[test]
    fn every_offered_approval_mode_has_a_cli_name() {
        for (mode, _, _) in APPROVAL_MODES {
            assert_eq!(approval_mode(approval_arg(*mode)), Some(*mode));
        }
    }
}
