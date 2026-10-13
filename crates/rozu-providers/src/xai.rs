//! xAI through the OpenAI Responses adapter: xAI serves the same `/responses` wire format at
//! `https://api.x.ai/v1`, with its own API key (Bearer auth). No adapter flags are needed so far;
//! add them to [`OpenAiConfig`] if the formats drift. Grok subscription logins are out of scope.

use rozu_core::{ModelInfo, ProviderResult};
use secrecy::SecretString;

use crate::openai::{OpenAiConfig, OpenAiProvider};

pub const DEFAULT_BASE_URL: &str = "https://api.x.ai/v1";
pub const PROVIDER_ID: &str = "xai";

/// An OpenAI adapter config pointed at xAI.
pub fn config(api_key: SecretString) -> OpenAiConfig {
    OpenAiConfig::new(api_key)
        .with_base_url(DEFAULT_BASE_URL)
        .with_provider_id(PROVIDER_ID)
}

/// An xAI provider with default settings.
pub fn provider(api_key: SecretString) -> ProviderResult<OpenAiProvider> {
    OpenAiProvider::new(config(api_key))
}

/// Known xAI models: (id, context window, USD per 1M input / output / cached input tokens).
// Prices NEED VERIFICATION against https://docs.x.ai/docs/models before relying on cost totals;
// they are list prices as last checked by hand and change without notice (grok-4 also charges
// more above 128k input tokens, which is not modeled).
pub fn models() -> Vec<ModelInfo> {
    crate::registry::model_table(
        PROVIDER_ID,
        &[
            ("grok-4", 256_000, 3.0, 15.0, 0.75),
            ("grok-4-fast-reasoning", 2_000_000, 0.20, 0.50, 0.05),
            ("grok-4-fast-non-reasoning", 2_000_000, 0.20, 0.50, 0.05),
            ("grok-code-fast-1", 256_000, 0.20, 1.50, 0.02),
            ("grok-3-mini", 131_072, 0.30, 0.50, 0.075),
        ],
    )
}

#[cfg(test)]
mod tests {
    use rozu_core::Provider;

    use super::*;

    #[test]
    fn reuses_openai_adapter_with_xai_settings() {
        let config = config(SecretString::from("xai-test"));
        assert_eq!(config.base_url, "https://api.x.ai/v1");
        let provider = OpenAiProvider::new(config).unwrap();
        assert_eq!(provider.id(), "xai");
    }
}
