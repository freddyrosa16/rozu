//! Model selection and provider availability, independent of a running chat.

use rozu_cli_providers::claude::install::find_claude_binary;
use rozu_cli_providers::codex::install::find_codex_binary;
use rozu_cli_providers::grok::install::find_grok_binary;
use rozu_providers::registry::{self, CLAUDE_CLI_ID, CODEX_ID, GROK_ID};
use rozu_providers::{ModelChoice, PROVIDERS, ProviderInfo};
use rozu_secrets::{KeyringStore, SecretStore, resolve_api_key};

/// The model for `--provider` / `--model`, defaulting as `rozu chat` does.
pub fn resolve_choice(
    provider: Option<String>,
    model: Option<&str>,
) -> anyhow::Result<ModelChoice> {
    let store = KeyringStore::default();
    let provider = default_provider(provider, model, || any_api_key(&store), cli_installed);
    Ok(registry::resolve(provider.as_deref(), model)?)
}

/// The provider to use when neither `--provider` nor `--model` is given: with no API key
/// configured anywhere, the first installed of claude-cli, codex and grok (claude-cli when
/// none is, so the error says how to install it); else the registry's default.
pub fn default_provider(
    provider: Option<String>,
    model: Option<&str>,
    any_api_key: impl FnOnce() -> bool,
    cli_installed: impl Fn(&str) -> bool,
) -> Option<String> {
    match (provider, model) {
        (Some(provider), _) => Some(provider),
        (None, Some(_)) => None,
        (None, None) if any_api_key() => None,
        (None, None) => Some(
            [CLAUDE_CLI_ID, CODEX_ID, GROK_ID]
                .into_iter()
                .find(|id| cli_installed(id))
                .unwrap_or(CLAUDE_CLI_ID)
                .to_owned(),
        ),
    }
}

/// Whether the vendor CLI behind a CLI-backed provider is installed.
pub fn cli_installed(provider: &str) -> bool {
    match provider {
        CLAUDE_CLI_ID => find_claude_binary().is_some_and(|p| p.exists()),
        CODEX_ID => find_codex_binary().is_some_and(|p| p.exists()),
        GROK_ID => find_grok_binary().is_ok_and(|p| p.exists()),
        _ => false,
    }
}

/// Whether any API-key provider has a key (env var or keychain).
pub fn any_api_key(store: &dyn SecretStore) -> bool {
    PROVIDERS
        .iter()
        .filter(|p| p.needs_api_key())
        .any(|p| api_key_available(store, p.id))
}

/// Installed subscription CLIs first, then providers with an API key. Registry order is kept
/// within each group; a failed keychain lookup leaves that provider unavailable.
pub fn available_providers(store: &dyn SecretStore) -> Vec<&'static ProviderInfo> {
    available_providers_with(cli_installed, |id| api_key_available(store, id))
}

fn api_key_available(store: &dyn SecretStore, provider: &str) -> bool {
    matches!(
        resolve_api_key(store, provider, |var| std::env::var(var).ok()),
        Ok(Some(_))
    )
}

fn available_providers_with(
    cli_installed: impl Fn(&str) -> bool,
    has_api_key: impl Fn(&str) -> bool,
) -> Vec<&'static ProviderInfo> {
    let mut providers: Vec<_> = PROVIDERS
        .iter()
        .filter(|provider| {
            if provider.is_cli_backed() {
                cli_installed(provider.id)
            } else {
                has_api_key(provider.id)
            }
        })
        .collect();
    providers.sort_by_key(|provider| !provider.is_cli_backed());
    providers
}

/// `OPENAI_BASE_URL`, `ANTHROPIC_BASE_URL` or `XAI_BASE_URL`, if set: for proxies and tests.
pub fn base_url_override(provider: &str) -> Option<String> {
    std::env::var(format!("{}_BASE_URL", provider.to_ascii_uppercase()))
        .ok()
        .filter(|url| !url.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn available_providers_keep_cli_first_registry_order() {
        for (clis, keys, expected) in [
            (
                vec![CLAUDE_CLI_ID, CODEX_ID, GROK_ID],
                vec!["openai", "anthropic", "xai"],
                vec![
                    CLAUDE_CLI_ID,
                    CODEX_ID,
                    GROK_ID,
                    "openai",
                    "anthropic",
                    "xai",
                ],
            ),
            (
                vec![GROK_ID, CODEX_ID],
                vec!["xai", "openai"],
                vec![CODEX_ID, GROK_ID, "openai", "xai"],
            ),
            (vec![], vec!["anthropic"], vec!["anthropic"]),
            (vec![], vec![], vec![]),
        ] {
            let providers = available_providers_with(
                |id| {
                    assert!(registry::provider_info(id).unwrap().is_cli_backed());
                    clis.contains(&id)
                },
                |id| {
                    assert!(registry::provider_info(id).unwrap().needs_api_key());
                    keys.contains(&id)
                },
            );
            assert_eq!(providers.iter().map(|p| p.id).collect::<Vec<_>>(), expected);
        }
    }

    #[test]
    fn explicit_choices_do_not_probe_credentials_or_installed_clis() {
        for (provider, model, expected) in [
            (Some("xai".to_owned()), None, Some("xai".to_owned())),
            (None, Some("gpt-5-mini"), None),
        ] {
            assert_eq!(
                default_provider(
                    provider,
                    model,
                    || panic!("an explicit choice must not read credentials"),
                    |_| panic!("an explicit choice must not search for CLIs"),
                ),
                expected
            );
        }
    }

    #[test]
    fn an_api_key_skips_cli_discovery() {
        assert_eq!(
            default_provider(
                None,
                None,
                || true,
                |_| panic!("an API key uses the registry default"),
            ),
            None
        );
    }

    #[test]
    fn claude_cli_then_codex_then_grok_are_the_defaults_without_api_keys() {
        let only = |ids: &'static [&'static str]| move |id: &str| ids.contains(&id);
        for (installed, expected) in [
            (only(&["codex", "grok"]), "codex"),
            (only(&["grok"]), "grok"),
            (only(&["claude-cli", "codex", "grok"]), "claude-cli"),
            (only(&["codex"]), "codex"),
            // With nothing installed, claude-cli's error says how to install it.
            (only(&[]), "claude-cli"),
        ] {
            assert_eq!(
                default_provider(None, None, || false, installed).as_deref(),
                Some(expected)
            );
        }
        let all = |_: &str| true;
        assert_eq!(default_provider(None, None, || true, all), None);
        assert_eq!(
            default_provider(Some("xai".into()), None, || false, all).as_deref(),
            Some("xai")
        );
        assert_eq!(default_provider(None, Some("gpt-5.1"), || false, all), None);
    }
}
