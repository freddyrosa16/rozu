//! `rozu`: headless CLI driver for the agent, used for the dev loop and evals before the GUI exists.

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "rozu", version, about = "A native Rust coding agent")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print versions of the rozu crates.
    Doctor,
}

/// Name and version of every crate linked into this binary.
fn crate_versions() -> [(&'static str, &'static str); 7] {
    [
        ("rozu-cli", env!("CARGO_PKG_VERSION")),
        ("rozu-core", rozu_core::VERSION),
        ("rozu-providers", rozu_providers::VERSION),
        ("rozu-tools", rozu_tools::VERSION),
        ("rozu-pty", rozu_pty::VERSION),
        ("rozu-secrets", rozu_secrets::VERSION),
        ("rozu-store", rozu_store::VERSION),
    ]
}

fn main() {
    rozu_core::init_tracing();
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Doctor) => {
            for (name, version) in crate_versions() {
                println!("{name:<16}{version}");
            }
        }
        None => {
            println!(
                "rozu {}: nothing to do yet. Try `rozu doctor`.",
                env!("CARGO_PKG_VERSION")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }
}
