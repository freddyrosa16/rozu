//! The providers and models rozu knows about, model lookup by `provider/model` or bare id, and
//! construction of a [`Provider`] for a choice.

use std::sync::Arc;

use rozu_core::{ModelInfo, Provider, ProviderResult};
use secrecy::SecretString;

use crate::anthropic::{self, AnthropicConfig, AnthropicProvider};
use crate::openai::{self, OpenAiConfig, OpenAiProvider};
use crate::retry::RetryPolicy;
use crate::xai;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    OpenAi,
    Anthropic,
    Xai,
}

/// A provider rozu can talk to.
#[derive(Debug, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub default_base_url: &'static str,
    /// Used when the user picks the provider but no model. Overridable per run with
    /// `--model` / `ROZU_MODEL`.
    pub default_model: &'static str,
    kind: Kind,
}

pub const PROVIDERS: &[ProviderInfo] = &[
    ProviderInfo {
        id: openai::PROVIDER_ID,
        label: "OpenAI",
        default_base_url: openai::DEFAULT_BASE_URL,
        default_model: "gpt-5.1",
        kind: Kind::OpenAi,
    },
    ProviderInfo {
        id: anthropic::PROVIDER_ID,
        label: "Anthropic",
        default_base_url: anthropic::DEFAULT_BASE_URL,
        default_model: "claude-sonnet-4-5",
        kind: Kind::Anthropic,
    },
    ProviderInfo {
        id: xai::PROVIDER_ID,
        label: "xAI",
        default_base_url: xai::DEFAULT_BASE_URL,
        default_model: "grok-4",
        kind: Kind::Xai,
    },
];

/// Used when neither a provider nor a model is given.
pub const DEFAULT_PROVIDER: &str = openai::PROVIDER_ID;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("unknown provider `{0}` (known: openai, anthropic, xai)")]
    UnknownProvider(String),
    #[error(
        "can't tell which provider serves `{0}`; use provider/model, e.g. anthropic/{0}, or see `rozu models`"
    )]
    UnknownModel(String),
    #[error("model `{model}` names provider `{in_model}`, but --provider is `{flag}`")]
    ProviderMismatch {
        flag: String,
        in_model: String,
        model: String,
    },
}

/// A resolved provider + model. `info` is `None` for a model not in the registry (it can still be
/// used; cost is just unknown).
#[derive(Debug, Clone, PartialEq)]
pub struct ModelChoice {
    pub provider: &'static ProviderInfo,
    pub model: String,
    pub info: Option<ModelInfo>,
}

impl ModelChoice {
    /// `provider/model`.
    pub fn spec(&self) -> String {
        format!("{}/{}", self.provider.id, self.model)
    }
}

pub fn provider_info(id: &str) -> Option<&'static ProviderInfo> {
    PROVIDERS.iter().find(|p| p.id == id)
}

/// Every known model, grouped by provider in [`PROVIDERS`] order.
pub fn models() -> Vec<ModelInfo> {
    PROVIDERS.iter().flat_map(|p| models_for(p.id)).collect()
}

pub fn models_for(provider: &str) -> Vec<ModelInfo> {
    match provider_info(provider).map(|p| p.kind) {
        Some(Kind::OpenAi) => openai::models(),
        Some(Kind::Anthropic) => anthropic::models(),
        Some(Kind::Xai) => xai::models(),
        None => Vec::new(),
    }
}

/// A known model by `provider/model` or by bare model id.
pub fn lookup(spec: &str) -> Option<ModelInfo> {
    match spec.split_once('/') {
        Some((provider, model)) => models_for(provider).into_iter().find(|m| m.id == model),
        None => models().into_iter().find(|m| m.id == spec),
    }
}

/// Picks the provider and model from optional `--provider` / `--model` values.
///
/// `model` may be `provider/model` or a bare id. A bare id that isn't in the registry is placed
/// by `provider`, or else by its prefix (`claude-`, `grok-`, `gpt-`/`o<digit>`). With no model,
/// the provider's default model is used; with neither, [`DEFAULT_PROVIDER`]'s.
pub fn resolve(provider: Option<&str>, model: Option<&str>) -> Result<ModelChoice, RegistryError> {
    let flag = provider
        .map(|id| provider_info(id).ok_or_else(|| RegistryError::UnknownProvider(id.to_owned())))
        .transpose()?;

    let (provider, model) = match model.map(str::trim).filter(|m| !m.is_empty()) {
        Some(spec) => match spec.split_once('/') {
            Some((id, model)) => {
                let named = provider_info(id)
                    .ok_or_else(|| RegistryError::UnknownProvider(id.to_owned()))?;
                if let Some(flag) = flag.filter(|flag| flag.id != named.id) {
                    return Err(RegistryError::ProviderMismatch {
                        flag: flag.id.to_owned(),
                        in_model: id.to_owned(),
                        model: spec.to_owned(),
                    });
                }
                (named, model.to_owned())
            }
            None => {
                let provider = flag
                    .or_else(|| lookup(spec).and_then(|m| provider_info(&m.provider)))
                    .or_else(|| infer_provider(spec))
                    .ok_or_else(|| RegistryError::UnknownModel(spec.to_owned()))?;
                (provider, spec.to_owned())
            }
        },
        None => {
            let provider = flag.unwrap_or_else(|| {
                provider_info(DEFAULT_PROVIDER).expect("default provider is registered")
            });
            (provider, provider.default_model.to_owned())
        }
    };
    let info = lookup(&format!("{}/{model}", provider.id));
    Ok(ModelChoice {
        provider,
        model,
        info,
    })
}

fn infer_provider(model: &str) -> Option<&'static ProviderInfo> {
    let id = if model.starts_with("claude-") {
        anthropic::PROVIDER_ID
    } else if model.starts_with("grok-") {
        xai::PROVIDER_ID
    } else if model.starts_with("gpt-")
        || model.starts_with("codex-")
        || (model.starts_with('o') && model[1..].starts_with(|c: char| c.is_ascii_digit()))
    {
        openai::PROVIDER_ID
    } else {
        return None;
    };
    provider_info(id)
}

/// Builds the adapter for `provider`. `base_url` overrides the provider's default (tests and
/// proxies); `retry` overrides the default retry policy.
pub fn build(
    provider: &ProviderInfo,
    api_key: SecretString,
    base_url: Option<&str>,
    retry: Option<RetryPolicy>,
) -> ProviderResult<Arc<dyn Provider>> {
    Ok(match provider.kind {
        Kind::OpenAi | Kind::Xai => {
            let mut config = if provider.kind == Kind::Xai {
                xai::config(api_key)
            } else {
                OpenAiConfig::new(api_key)
            };
            if let Some(url) = base_url {
                config = config.with_base_url(url);
            }
            if let Some(retry) = retry {
                config = config.with_retry(retry);
            }
            Arc::new(OpenAiProvider::new(config)?)
        }
        Kind::Anthropic => {
            let mut config = AnthropicConfig::new(api_key);
            if let Some(url) = base_url {
                config = config.with_base_url(url);
            }
            if let Some(retry) = retry {
                config = config.with_retry(retry);
            }
            Arc::new(AnthropicProvider::new(config)?)
        }
    })
}

/// Builds [`ModelInfo`]s from `(id, context window, $/M in, $/M out, $/M cached in)` rows. The
/// cache-write price is `cache_write_multiplier` times the input price. All listed models support
/// tool calling.
pub(crate) fn model_table(
    provider: &str,
    cache_write_multiplier: f64,
    rows: &[(&str, u32, f64, f64, f64)],
) -> Vec<ModelInfo> {
    rows.iter()
        .map(
            |&(id, context_window, price_in, price_out, price_cached)| ModelInfo {
                id: id.to_owned(),
                provider: provider.to_owned(),
                context_window,
                supports_tools: true,
                price_in_per_mtok: price_in,
                price_out_per_mtok: price_out,
                price_cached_in_per_mtok: price_cached,
                price_cache_write_per_mtok: price_in * cache_write_multiplier,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn choice(provider: Option<&str>, model: Option<&str>) -> (String, String, bool) {
        let c = resolve(provider, model).unwrap();
        (c.provider.id.to_owned(), c.model, c.info.is_some())
    }

    #[test]
    fn every_provider_lists_its_default_model() {
        for provider in PROVIDERS {
            let models = models_for(provider.id);
            assert!(!models.is_empty(), "{}", provider.id);
            assert!(
                models.iter().any(|m| m.id == provider.default_model),
                "{} default {} is not listed",
                provider.id,
                provider.default_model
            );
            for model in models {
                assert_eq!(model.provider, provider.id);
                assert!(model.context_window > 0 && model.supports_tools);
                assert!(model.price_cached_in_per_mtok <= model.price_in_per_mtok);
                assert!(model.price_cache_write_per_mtok >= model.price_in_per_mtok);
            }
        }
    }

    #[test]
    fn cache_write_prices() {
        let sonnet = lookup("anthropic/claude-sonnet-4-5").unwrap();
        assert!((sonnet.price_cache_write_per_mtok - 3.75).abs() < 1e-9);
        for spec in ["openai/gpt-5.1", "xai/grok-4"] {
            let model = lookup(spec).unwrap();
            assert_eq!(model.price_cache_write_per_mtok, model.price_in_per_mtok);
        }
    }

    #[test]
    fn model_ids_are_unique() {
        let all = models();
        for (i, model) in all.iter().enumerate() {
            assert!(
                all[i + 1..].iter().all(|other| other.id != model.id),
                "duplicate {}",
                model.id
            );
        }
    }

    #[test]
    fn lookup_by_spec_or_id() {
        assert_eq!(
            lookup("anthropic/claude-sonnet-4-5").unwrap().provider,
            "anthropic"
        );
        assert_eq!(lookup("claude-sonnet-4-5").unwrap().provider, "anthropic");
        assert_eq!(lookup("grok-4").unwrap().provider, "xai");
        assert_eq!(lookup("openai/gpt-5.1").unwrap().context_window, 400_000);
        assert!(lookup("openai/claude-sonnet-4-5").is_none());
        assert!(lookup("nope/gpt-5.1").is_none());
        assert!(lookup("gpt-9000").is_none());
    }

    #[test]
    fn resolves_defaults() {
        assert_eq!(
            choice(None, None),
            ("openai".into(), "gpt-5.1".into(), true)
        );
        assert_eq!(
            choice(Some("anthropic"), None),
            ("anthropic".into(), "claude-sonnet-4-5".into(), true)
        );
        assert_eq!(
            choice(Some("xai"), Some("  ")),
            ("xai".into(), "grok-4".into(), true)
        );
    }

    #[test]
    fn resolves_specs_and_bare_ids() {
        assert_eq!(
            choice(None, Some("xai/grok-4")),
            ("xai".into(), "grok-4".into(), true)
        );
        assert_eq!(
            choice(None, Some("claude-haiku-4-5")),
            ("anthropic".into(), "claude-haiku-4-5".into(), true)
        );
        // Unlisted models are allowed, placed by prefix or by --provider, without prices.
        assert_eq!(
            choice(None, Some("claude-next")),
            ("anthropic".into(), "claude-next".into(), false)
        );
        assert_eq!(
            choice(None, Some("o3")),
            ("openai".into(), "o3".into(), false)
        );
        assert_eq!(
            choice(Some("xai"), Some("my-finetune")),
            ("xai".into(), "my-finetune".into(), false)
        );
        assert_eq!(
            choice(Some("anthropic"), Some("anthropic/claude-sonnet-4-5")),
            ("anthropic".into(), "claude-sonnet-4-5".into(), true)
        );
        assert_eq!(
            resolve(None, Some("anthropic/claude-sonnet-4-5"))
                .unwrap()
                .spec(),
            "anthropic/claude-sonnet-4-5"
        );
    }

    #[test]
    fn resolve_errors() {
        assert_eq!(
            resolve(Some("gemini"), None),
            Err(RegistryError::UnknownProvider("gemini".into()))
        );
        assert_eq!(
            resolve(None, Some("gemini/pro")),
            Err(RegistryError::UnknownProvider("gemini".into()))
        );
        assert_eq!(
            resolve(None, Some("mystery")),
            Err(RegistryError::UnknownModel("mystery".into()))
        );
        assert!(matches!(
            resolve(Some("openai"), Some("xai/grok-4")),
            Err(RegistryError::ProviderMismatch { .. })
        ));
    }

    #[test]
    fn builds_each_provider() {
        for provider in PROVIDERS {
            let built = build(provider, SecretString::from("k"), None, None).unwrap();
            assert_eq!(built.id(), provider.id);
        }
    }
}
