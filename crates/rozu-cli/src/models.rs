//! `rozu models`: list the known models with context window and prices.

use std::process::ExitCode;

use rozu_providers::registry;

pub fn run() -> anyhow::Result<ExitCode> {
    println!(
        "{:<38}{:>9}{:>9}{:>9}{:>12}{:>14}",
        "MODEL", "CONTEXT", "IN $/M", "OUT $/M", "CACHED $/M", "CACHE WR $/M"
    );
    for model in registry::models() {
        let is_default = registry::provider_info(&model.provider)
            .is_some_and(|provider| provider.default_model == model.id);
        let name = format!(
            "{}/{}{}",
            model.provider,
            model.id,
            if is_default { " *" } else { "" }
        );
        println!(
            "{name:<38}{:>9}{:>9}{:>9}{:>12}{:>14}",
            tokens(model.context_window),
            price(model.price_in_per_mtok),
            price(model.price_out_per_mtok),
            price(model.price_cached_in_per_mtok),
            price(model.price_cache_write_per_mtok),
        );
    }
    println!(
        "\n* default for its provider. Prices are USD per million tokens, copied by hand from the \
         providers' pricing pages; verify them before relying on cost estimates."
    );
    Ok(ExitCode::SUCCESS)
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
    }
}
