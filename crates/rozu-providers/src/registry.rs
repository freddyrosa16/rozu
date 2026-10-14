//! The providers and models rozu knows about, model lookup by `provider/model` or bare id, and
//! construction of a [`Provider`] for a choice.

use std::sync::Arc;

use rozu_core::{
    LongContextPrices, MemoryFeatures, ModelInfo, Provider, ProviderCaps, ProviderError,
    ProviderResult,
};
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
    /// The official `claude` CLI on the user's subscription; built by `rozu-cli-providers`.
    ClaudeCli,
    /// The official Codex CLI's `codex app-server` on the user's ChatGPT subscription; built by
    /// `rozu-cli-providers`.
    Codex,
    /// xAI's official Grok Build CLI (`grok agent stdio`, ACP) on the user's SuperGrok
    /// subscription; built by `rozu-cli-providers`.
    Grok,
}

/// Id of the CLI-backed Claude provider (`rozu_cli_providers::claude::PROVIDER_ID`).
pub const CLAUDE_CLI_ID: &str = "claude-cli";
/// Context window assumed for the `claude` CLI's model aliases.
const CLAUDE_CLI_CONTEXT: u32 = 200_000;
/// Id of the CLI-backed Codex provider (`rozu_cli_providers::codex::PROVIDER_ID`).
pub const CODEX_ID: &str = "codex";
/// Codex models listed when the app-server's own list (`rozu models` asks it) isn't available.
/// **Unverified**: not checked against what Codex accepts. `default` sends no model, so Codex
/// uses the one it is configured with.
pub const CODEX_STATIC_MODELS: &[&str] = &["default", "gpt-6.1-sol", "gpt-6-astra", "gpt-6-luna"];
/// Context window assumed for Codex models (unverified; Codex reports the real one per turn).
const CODEX_CONTEXT: u32 = 272_000;
/// Id of the CLI-backed Grok provider (`rozu_cli_providers::grok::PROVIDER_ID`).
pub const GROK_ID: &str = "grok";
/// Grok Build models rozu lists. **Unverified**: seen in Grok Build 1.0.52 output (`grok-4.7`
/// over ACP, `grok-4.7-build` in usage, `grok-4.6` in the signed-out `initialize`), not checked
/// against what the CLI accepts for `-m`. `default` passes no `-m`, so Grok uses its own default.
pub const GROK_STATIC_MODELS: &[&str] = &["default", "grok-4.7", "grok-4.7-build", "grok-4.6"];
/// Context window assumed for Grok Build models (unverified; `initialize` reported 500k for the
/// 4.5/4.6 models).
const GROK_CONTEXT: u32 = 500_000;
/// What rozu can do through Grok Build: it can't control the prompt prefix per turn (Grok adds
/// its own priority rules, environment block and meta-tools, and caching is per Grok session, so
/// a history rewrite means a new session with a cold cache), and cache writes are never reported.
pub const GROK_CAPS: ProviderCaps = ProviderCaps {
    prompt_prefix_control: false,
    cache_write_reported: false,
    memory_features: MemoryFeatures::Limited,
};

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
        default_model: "gpt-6.1-sol",
        kind: Kind::OpenAi,
    },
    ProviderInfo {
        id: anthropic::PROVIDER_ID,
        label: "Anthropic",
        default_base_url: anthropic::DEFAULT_BASE_URL,
        default_model: "claude-sonnet-5-5",
        kind: Kind::Anthropic,
    },
    ProviderInfo {
        id: xai::PROVIDER_ID,
        label: "xAI",
        default_base_url: xai::DEFAULT_BASE_URL,
        default_model: "grok-4.7",
        kind: Kind::Xai,
    },
    ProviderInfo {
        id: CLAUDE_CLI_ID,
        label: "Claude (subscription, via the claude CLI)",
        default_base_url: "",
        default_model: "sonnet",
        kind: Kind::ClaudeCli,
    },
    ProviderInfo {
        id: CODEX_ID,
        label: "OpenAI (ChatGPT subscription, via the codex CLI)",
        default_base_url: "",
        default_model: "default",
        kind: Kind::Codex,
    },
    ProviderInfo {
        id: GROK_ID,
        label: "xAI Grok (SuperGrok subscription, via the Grok Build CLI)",
        default_base_url: "",
        default_model: "default",
        kind: Kind::Grok,
    },
];

impl ProviderInfo {
    /// Whether this provider drives a vendor CLI on the user's subscription (no API key; built
    /// by `rozu-cli-providers`, not [`build`]).
    pub fn is_cli_backed(&self) -> bool {
        matches!(self.kind, Kind::ClaudeCli | Kind::Codex | Kind::Grok)
    }

    /// Context window assumed for a model of a CLI-backed provider that isn't listed.
    fn cli_context(&self) -> u32 {
        match self.kind {
            Kind::Codex => CODEX_CONTEXT,
            Kind::Grok => GROK_CONTEXT,
            _ => CLAUDE_CLI_CONTEXT,
        }
    }

    /// A subscription model of this CLI-backed provider, with the provider's capabilities.
    fn cli_model(&self, id: &str) -> ModelInfo {
        let model = ModelInfo::subscription(self.id, id, self.cli_context());
        if self.kind == Kind::Grok {
            model.with_caps(GROK_CAPS)
        } else {
            model
        }
    }

    /// Whether this provider needs an API key.
    pub fn needs_api_key(&self) -> bool {
        !self.is_cli_backed()
    }
}

/// Used when neither a provider nor a model is given.
pub const DEFAULT_PROVIDER: &str = openai::PROVIDER_ID;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RegistryError {
    #[error("unknown provider `{0}` (known: openai, anthropic, xai, claude-cli, codex, grok)")]
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
        // The CLI's aliases, which follow the newest model of each family. Any other id the CLI
        // accepts works too (see `resolve`).
        Some(Kind::ClaudeCli) => ["sonnet", "opus", "fable", "haiku"]
            .into_iter()
            .map(|id| ModelInfo::subscription(CLAUDE_CLI_ID, id, CLAUDE_CLI_CONTEXT))
            .collect(),
        Some(Kind::Codex) => CODEX_STATIC_MODELS
            .iter()
            .map(|id| ModelInfo::subscription(CODEX_ID, *id, CODEX_CONTEXT))
            .collect(),
        Some(Kind::Grok) => {
            let grok = provider_info(GROK_ID).expect("grok is registered");
            GROK_STATIC_MODELS
                .iter()
                .map(|id| grok.cli_model(id))
                .collect()
        }
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
    let info = lookup(&format!("{}/{model}", provider.id)).or_else(|| {
        // Every model is covered by the subscription, listed or not.
        provider.is_cli_backed().then(|| provider.cli_model(&model))
    });
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
/// proxies); `retry` overrides the default retry policy. CLI-backed providers
/// ([`ProviderInfo::is_cli_backed`]) are built by `rozu-cli-providers` and fail here.
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
        Kind::ClaudeCli | Kind::Codex | Kind::Grok => {
            return Err(ProviderError::BadRequest {
                status: 0,
                message: format!(
                    "{} is CLI-backed and takes no API key; build it with rozu-cli-providers",
                    provider.id
                ),
            });
        }
    })
}

/// USD per million tokens, in the order the pricing pages list them: input, cached input (cache
/// read), cache write, output. Providers that don't bill cache writes use the input price.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Prices {
    pub input: f64,
    pub cached: f64,
    pub write: f64,
    pub output: f64,
}

pub(crate) const fn prices(input: f64, cached: f64, write: f64, output: f64) -> Prices {
    Prices {
        input,
        cached,
        write,
        output,
    }
}

/// A model's prices: standard, plus an optional long-context tier `(threshold tokens, prices)`
/// that applies to requests whose prompt has at least that many tokens.
pub(crate) type ModelRow = (&'static str, u32, Prices, Option<(u64, Prices)>);

/// Builds [`ModelInfo`]s from `(id, context window, prices, long-context tier)` rows. All listed
/// models support tool calling.
pub(crate) fn model_table(provider: &str, rows: &[ModelRow]) -> Vec<ModelInfo> {
    rows.iter()
        .map(|&(id, context_window, p, long)| ModelInfo {
            id: id.to_owned(),
            provider: provider.to_owned(),
            context_window,
            supports_tools: true,
            price_in_per_mtok: p.input,
            price_out_per_mtok: p.output,
            price_cached_in_per_mtok: p.cached,
            price_cache_write_per_mtok: p.write,
            long_context: long.map(|(threshold_tokens, p)| LongContextPrices {
                threshold_tokens,
                price_in_per_mtok: p.input,
                price_out_per_mtok: p.output,
                price_cached_in_per_mtok: p.cached,
                price_cache_write_per_mtok: p.write,
            }),
            billing: rozu_core::Billing::Metered,
            caps: ProviderCaps::FULL,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use rozu_core::Usage;

    use super::*;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    /// `(provider, model)` input and the expected `(provider id, model, priced)`.
    type ResolveCase<'a> = (Option<&'a str>, Option<&'a str>, (&'a str, &'a str, bool));

    fn assert_resolves(cases: &[ResolveCase]) {
        for &(provider, model, want) in cases {
            let c = resolve(provider, model).unwrap();
            assert_eq!(
                (c.provider.id, c.model.as_str(), c.info.is_some()),
                want,
                "{provider:?} {model:?}"
            );
        }
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
                if let Some(long) = model.long_context {
                    assert!(
                        long.threshold_tokens < u64::from(model.context_window),
                        "{}",
                        model.id
                    );
                    assert!(long.price_in_per_mtok > model.price_in_per_mtok);
                    assert!(long.price_out_per_mtok > model.price_out_per_mtok);
                    assert!(long.price_cached_in_per_mtok >= model.price_cached_in_per_mtok);
                    assert!(long.price_cache_write_per_mtok >= model.price_cache_write_per_mtok);
                }
            }
        }
    }

    #[test]
    fn retired_and_unlisted_models_are_gone() {
        for spec in [
            "anthropic/claude-opus-4-1",
            "anthropic/claude-sonnet-4-5",
            "xai/grok-4",
            "xai/grok-4-fast-reasoning",
        ] {
            assert!(lookup(spec).is_none(), "{spec} should not be listed");
        }
    }

    #[test]
    fn cache_write_prices() {
        let sonnet = lookup("anthropic/claude-sonnet-5-5").unwrap();
        assert!(close(sonnet.price_cache_write_per_mtok, 2.50));
        assert!(close(sonnet.price_cached_in_per_mtok, 0.10));
        // OpenAI bills cache writes on the gpt-6 family, not on older models; xAI never does.
        assert!(close(
            lookup("openai/gpt-6.1-sol")
                .unwrap()
                .price_cache_write_per_mtok,
            2.50
        ));
        for spec in ["openai/gpt-5.1", "xai/grok-4.7"] {
            let model = lookup(spec).unwrap();
            assert_eq!(model.price_cache_write_per_mtok, model.price_in_per_mtok);
        }
    }

    #[test]
    fn long_context_tiers_match_the_pricing_pages() {
        let usage = |input| Usage {
            input_tokens: input,
            output_tokens: 1_000_000,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
        };
        for (spec, input, expected) in [
            // gpt-6.1-sol: over 272K prompt tokens, 4.00 in / 15.00 out.
            ("openai/gpt-6.1-sol", 272_000, 0.544 + 10.0),
            ("openai/gpt-6.1-sol", 272_001, 1.088_004 + 15.0),
            // gpt-6-astra: 10 / 50 up to 272K, then 20 in / 75 out.
            ("openai/gpt-6-astra", 272_000, 2.72 + 50.0),
            ("openai/gpt-6-astra", 272_001, 5.440_02 + 75.0),
            // gpt-6-luna: 0.10 / 0.50 up to 272K, then 0.20 in / 0.75 out.
            ("openai/gpt-6-luna", 272_000, 0.0272 + 0.5),
            ("openai/gpt-6-luna", 272_001, 0.054_400_2 + 0.75),
            // Haiku 5.5: over 100,000 prompt tokens, 0.50 in / 2.50 out.
            ("anthropic/claude-haiku-5-5", 100_000, 0.01 + 0.5),
            ("anthropic/claude-haiku-5-5", 100_001, 0.050_000_5 + 2.5),
            // xAI: a prompt reaching 200K tokens doubles every price.
            ("xai/grok-4.7", 199_999, 0.399_998 + 6.0),
            ("xai/grok-4.7", 200_000, 0.8 + 12.0),
        ] {
            let cost = lookup(spec).unwrap().cost_usd(&usage(input));
            assert!(close(cost, expected), "{spec} at {input}: {cost}");
        }
        for (spec, cached, write) in [
            ("openai/gpt-6-astra", 2.0, 25.0),
            ("openai/gpt-6-luna", 0.02, 0.25),
        ] {
            let long = lookup(spec).unwrap().long_context.unwrap();
            assert_eq!(long.threshold_tokens, 272_001, "{spec}");
            assert!(close(long.price_cached_in_per_mtok, cached), "{spec}");
            assert!(close(long.price_cache_write_per_mtok, write), "{spec}");
        }
        // Sonnet 5.5 has 1M context at standard prices.
        assert!(
            lookup("anthropic/claude-sonnet-5-5")
                .unwrap()
                .long_context
                .is_none()
        );
    }

    #[test]
    fn model_ids_are_unique() {
        let all = models();
        for (i, model) in all.iter().enumerate() {
            for other in all[i + 1..].iter().filter(|other| other.id == model.id) {
                // Codex and Grok serve OpenAI's and xAI's models on the subscription (and both
                // have `default`); a bare id still means the provider listed first.
                let allowed = matches!(
                    (model.provider.as_str(), other.provider.as_str()),
                    (openai::PROVIDER_ID, CODEX_ID)
                        | (xai::PROVIDER_ID, GROK_ID)
                        | (CODEX_ID, GROK_ID)
                );
                assert!(
                    allowed,
                    "duplicate {} ({} and {})",
                    model.id, model.provider, other.provider
                );
                assert_eq!(lookup(&model.id).unwrap().provider, model.provider);
            }
        }
    }

    #[test]
    fn lookup_by_spec_or_id() {
        assert_eq!(
            lookup("anthropic/claude-sonnet-4-6").unwrap().provider,
            "anthropic"
        );
        assert_eq!(lookup("claude-sonnet-4-6").unwrap().provider, "anthropic");
        assert_eq!(lookup("grok-4.7").unwrap().provider, "xai");
        assert_eq!(lookup("openai/gpt-5.1").unwrap().context_window, 400_000);
        assert_eq!(
            lookup("openai/gpt-6.1-sol").unwrap().context_window,
            1_050_000
        );
        assert!(lookup("openai/claude-sonnet-4-6").is_none());
        assert!(lookup("nope/gpt-5.1").is_none());
        assert!(lookup("gpt-9000").is_none());
    }

    #[test]
    fn resolves_defaults() {
        assert_resolves(&[
            (None, None, ("openai", "gpt-6.1-sol", true)),
            (
                Some("anthropic"),
                None,
                ("anthropic", "claude-sonnet-5-5", true),
            ),
            (Some("xai"), Some("  "), ("xai", "grok-4.7", true)),
        ]);
    }

    #[test]
    fn resolves_specs_and_bare_ids() {
        // Retired and unlisted ids still resolve (the provider decides), placed by prefix or by
        // --provider, just without prices.
        assert_resolves(&[
            (None, Some("xai/grok-4.7"), ("xai", "grok-4.7", true)),
            (None, Some("xai/grok-4"), ("xai", "grok-4", false)),
            (
                None,
                Some("claude-haiku-4-5"),
                ("anthropic", "claude-haiku-4-5", true),
            ),
            (
                None,
                Some("claude-next"),
                ("anthropic", "claude-next", false),
            ),
            (None, Some("o3"), ("openai", "o3", false)),
            (
                Some("xai"),
                Some("my-finetune"),
                ("xai", "my-finetune", false),
            ),
            (
                Some("anthropic"),
                Some("anthropic/claude-sonnet-4-6"),
                ("anthropic", "claude-sonnet-4-6", true),
            ),
            (
                None,
                Some("anthropic/claude-sonnet-4-5"),
                ("anthropic", "claude-sonnet-4-5", false),
            ),
        ]);
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
            resolve(Some("openai"), Some("xai/grok-4.7")),
            Err(RegistryError::ProviderMismatch { .. })
        ));
    }

    #[test]
    fn builds_each_http_provider() {
        for provider in PROVIDERS {
            let built = build(provider, SecretString::from("k"), None, None);
            if provider.is_cli_backed() {
                assert!(built.is_err(), "{}", provider.id);
            } else {
                assert_eq!(built.unwrap().id(), provider.id);
            }
        }
    }

    #[test]
    fn claude_cli_models_are_subscription_billed() {
        let provider = provider_info(CLAUDE_CLI_ID).unwrap();
        assert!(provider.is_cli_backed() && !provider.needs_api_key());
        assert!(!provider_info("anthropic").unwrap().is_cli_backed());
        for model in models_for(CLAUDE_CLI_ID) {
            assert!(!model.is_metered(), "{}", model.id);
        }
        let choice = resolve(None, Some("opus")).unwrap();
        assert_eq!(choice.spec(), "claude-cli/opus");
        let choice = resolve(Some(CLAUDE_CLI_ID), None).unwrap();
        assert_eq!(choice.spec(), "claude-cli/sonnet");
        let choice = resolve(Some(CLAUDE_CLI_ID), Some("claude-opus-5-5")).unwrap();
        assert!(choice.info.is_some_and(|info| !info.is_metered()));
        assert!(lookup("anthropic/claude-opus-4-5").is_none_or(|m| m.is_metered()));
    }

    #[test]
    fn grok_models_are_subscription_billed_with_limited_memory_features() {
        let provider = provider_info(GROK_ID).unwrap();
        assert!(provider.is_cli_backed() && !provider.needs_api_key());
        let models = models_for(GROK_ID);
        assert!(models.iter().any(|m| m.id == "grok-4.7"));
        for model in &models {
            assert!(!model.is_metered(), "{}", model.id);
            assert_eq!(model.caps, GROK_CAPS, "{}", model.id);
        }
        assert_eq!(
            GROK_CAPS,
            ProviderCaps {
                prompt_prefix_control: false,
                cache_write_reported: false,
                memory_features: MemoryFeatures::Limited,
            }
        );
        assert_eq!(resolve(Some(GROK_ID), None).unwrap().spec(), "grok/default");
        // `--provider grok --model grok-4.7` is the subscription, not the xAI API.
        let choice = resolve(Some(GROK_ID), Some("grok-4.7")).unwrap();
        assert_eq!(choice.spec(), "grok/grok-4.7");
        let info = choice.info.unwrap();
        assert!(!info.is_metered() && info.caps == GROK_CAPS);
        let unlisted = resolve(Some(GROK_ID), Some("grok-build-next")).unwrap();
        assert_eq!(unlisted.info.unwrap().caps, GROK_CAPS);
        // A bare id still means the API-key provider, with full capabilities.
        let api = resolve(None, Some("grok-4.7")).unwrap();
        assert_eq!(api.spec(), "xai/grok-4.7");
        assert_eq!(api.info.unwrap().caps, ProviderCaps::FULL);
    }

    #[test]
    fn codex_models_are_subscription_billed() {
        let provider = provider_info(CODEX_ID).unwrap();
        assert!(provider.is_cli_backed() && !provider.needs_api_key());
        assert!(models_for(CODEX_ID).iter().all(|m| !m.is_metered()));
        assert_eq!(
            resolve(Some(CODEX_ID), None).unwrap().spec(),
            "codex/default"
        );
        let choice = resolve(Some(CODEX_ID), Some("gpt-6.1-sol")).unwrap();
        assert_eq!(choice.spec(), "codex/gpt-6.1-sol");
        assert!(choice.info.is_some_and(|info| !info.is_metered()));
        let unlisted = resolve(Some(CODEX_ID), Some("gpt-7-preview")).unwrap();
        assert_eq!(unlisted.info.unwrap().context_window, CODEX_CONTEXT);
        // A bare OpenAI id still means the API-key provider.
        assert_eq!(
            resolve(None, Some("gpt-6.1-sol")).unwrap().provider.id,
            "openai"
        );
        assert!(matches!(
            build(provider, SecretString::from("x".to_owned()), None, None),
            Err(ProviderError::BadRequest { .. })
        ));
    }
}
