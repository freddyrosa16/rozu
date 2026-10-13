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

/// Known xAI models with USD per 1M input / cached input / cache write / output tokens. Caching
/// is implicit, so cache writes cost the normal input price.
// Prices and context windows as of 2026-10-13 from https://docs.x.ai/docs/models. A request whose
// prompt reaches 200K tokens is billed at the higher rate for all of its tokens.
pub fn models() -> Vec<ModelInfo> {
    use crate::registry::prices;
    const LONG: u64 = 200_000;
    let grok_4_7 = prices(2.0, 0.50, 2.0, 6.0);
    let grok_4_7_long = prices(4.0, 1.0, 4.0, 12.0);
    let grok_4_3 = prices(1.25, 0.20, 1.25, 2.50);
    let grok_4_3_long = prices(2.50, 0.40, 2.50, 5.0);
    crate::registry::model_table(
        PROVIDER_ID,
        &[
            ("grok-4.7", 500_000, grok_4_7, Some((LONG, grok_4_7_long))),
            ("grok-4.6", 500_000, grok_4_7, Some((LONG, grok_4_7_long))),
            (
                "grok-4.5",
                500_000,
                prices(2.0, 0.30, 2.0, 6.0),
                Some((LONG, prices(4.0, 0.60, 4.0, 12.0))),
            ),
            ("grok-4.3", 1_000_000, grok_4_3, Some((LONG, grok_4_3_long))),
            (
                "grok-4.20-0309-reasoning",
                1_000_000,
                grok_4_3,
                Some((LONG, grok_4_3_long)),
            ),
            (
                "grok-4.20-0309-non-reasoning",
                1_000_000,
                grok_4_3,
                Some((LONG, grok_4_3_long)),
            ),
            (
                "grok-build-0.1",
                256_000,
                prices(1.0, 0.20, 1.0, 2.0),
                Some((LONG, prices(2.0, 0.40, 2.0, 4.0))),
            ),
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
