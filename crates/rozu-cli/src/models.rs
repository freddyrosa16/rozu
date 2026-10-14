//! `rozu models`: list the known models with context window and prices.
//!
//! Codex's models come from the installed Codex CLI (`codex app-server`'s `model/list`) when it
//! answers; otherwise a small static list, marked unverified, is shown. Grok's list is always
//! the static, unverified one (Grok Build isn't asked).

use std::process::ExitCode;

use rozu_cli_providers::codex::DEFAULT_MODEL as CODEX_DEFAULT_MODEL;
use rozu_cli_providers::codex::install::{CodexModel, find_codex_binary, list_models};
use rozu_providers::registry::{self, CODEX_ID, GROK_ID};

pub fn run() -> anyhow::Result<ExitCode> {
    let codex_live = codex_models();
    println!(
        "{:<38}{:>9}{:>9}{:>9}{:>12}{:>14}",
        "MODEL", "CONTEXT", "IN $/M", "OUT $/M", "CACHED $/M", "CACHE WR $/M"
    );
    let mut codex_listed = false;
    for model in registry::models() {
        if model.provider == CODEX_ID
            && let Some(live) = &codex_live
        {
            if !codex_listed {
                print_codex_models(live);
                codex_listed = true;
            }
            continue;
        }
        let is_default = registry::provider_info(&model.provider)
            .is_some_and(|provider| provider.default_model == model.id);
        let name = format!(
            "{}/{}{}",
            model.provider,
            model.id,
            if is_default { " *" } else { "" }
        );
        if !model.is_metered() {
            let note = if model.provider == GROK_ID {
                " · unverified · memory features limited"
            } else if model.provider == CODEX_ID {
                " · unverified"
            } else {
                ""
            };
            println!(
                "{name:<38}{:>9}   subscription (no per-token cost){note}",
                tokens(model.context_window)
            );
            continue;
        }
        println!(
            "{name:<38}{:>9}{:>9}{:>9}{:>12}{:>14}",
            tokens(model.context_window),
            price(model.price_in_per_mtok),
            price(model.price_out_per_mtok),
            price(model.price_cached_in_per_mtok),
            price(model.price_cache_write_per_mtok),
        );
        if let Some(long) = model.long_context {
            println!(
                "{:<38}{:>9}{:>9}{:>9}{:>12}{:>14}",
                format!("  prompt {}", threshold(long.threshold_tokens)),
                "",
                price(long.price_in_per_mtok),
                price(long.price_out_per_mtok),
                price(long.price_cached_in_per_mtok),
                price(long.price_cache_write_per_mtok),
            );
        }
    }
    println!(
        "\n* default for its provider. Prices are USD per million tokens, copied by hand from the \
         providers' pricing pages (as of 2026-10-13); verify them before relying on cost \
         estimates. An indented `prompt` row is a long-context tier: a request with that many \
         prompt tokens pays those prices for all of its tokens. claude-cli models run on your \
         Claude subscription through the official claude CLI: tokens are counted, cost isn't; \
         any model id the CLI accepts works. codex models run on your ChatGPT subscription \
         through the official Codex CLI the same way; `codex/default` is the model Codex is \
         configured with. grok models run on your SuperGrok subscription through xAI's Grok \
         Build CLI; the list is static and unverified, `grok/default` is Grok Build's own \
         default, and rozu's memory features are limited there (no per-turn prompt-prefix \
         control, cache writes not reported). {}",
        if codex_live.is_some() {
            "The codex list comes from the installed Codex CLI."
        } else {
            "The codex list is a static, unverified one (the Codex CLI wasn't available to ask)."
        }
    );
    Ok(ExitCode::SUCCESS)
}

/// The installed Codex CLI's model list, if it answers.
fn codex_models() -> Option<Vec<CodexModel>> {
    let binary = find_codex_binary()?;
    let runtime = tokio::runtime::Runtime::new().ok()?;
    match runtime.block_on(list_models(&binary)) {
        Ok(models) if !models.is_empty() => Some(models),
        Ok(_) => None,
        Err(err) => {
            tracing::info!(error = %err, "codex model list unavailable; showing the static list");
            None
        }
    }
}

fn print_codex_models(models: &[CodexModel]) {
    let default =
        registry::provider_info(CODEX_ID).map_or(CODEX_DEFAULT_MODEL, |p| p.default_model);
    println!(
        "{:<38}{:>9}   subscription (Codex's configured model)",
        format!("{CODEX_ID}/{default} *"),
        ""
    );
    for model in models {
        let name = format!(
            "{CODEX_ID}/{}{}",
            model.id,
            if model.is_default {
                " (codex default)"
            } else {
                ""
            }
        );
        println!(
            "{name:<38}{:>9}   subscription (no per-token cost) · {}",
            "", model.display_name
        );
    }
}

/// `>272K` for a tier starting just over a round number, `≥200K` for one starting at it.
fn threshold(tokens: u64) -> String {
    if tokens % 1000 == 1 {
        format!(">{}K", tokens / 1000)
    } else {
        format!(
            "≥{}",
            self::tokens(u32::try_from(tokens).unwrap_or(u32::MAX))
        )
    }
}

/// `400K`, `2M`, `1.05M`.
fn tokens(n: u32) -> String {
    if n >= 1_000_000 {
        let m = format!("{:.2}", f64::from(n) / 1_000_000.0);
        format!("{}M", m.trim_end_matches('0').trim_end_matches('.'))
    } else {
        format!("{}K", n / 1000)
    }
}

/// `3.00`, `0.125`: two decimals unless a third is needed.
fn price(usd: f64) -> String {
    let three = format!("{usd:.3}");
    match three.strip_suffix('0') {
        Some(two) => two.to_owned(),
        None => three,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_columns() {
        assert_eq!(tokens(400_000), "400K");
        assert_eq!(tokens(131_072), "131K");
        assert_eq!(tokens(2_000_000), "2M");
        assert_eq!(tokens(1_047_576), "1.05M");
        assert_eq!(price(3.0), "3.00");
        assert_eq!(price(0.125), "0.125");
        assert_eq!(price(0.05), "0.05");
        assert_eq!(threshold(272_001), ">272K");
        assert_eq!(threshold(100_001), ">100K");
        assert_eq!(threshold(200_000), "≥200K");
    }
}
