//! `rozu`: headless CLI driver for the agent, used for the dev loop and evals before the GUI exists.

mod auth;
mod chat;

use std::process::ExitCode;

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
    /// Manage provider API keys (stored in the OS keychain).
    Auth {
        #[command(subcommand)]
        command: auth::AuthCommand,
    },
    /// Chat with the agent. OPENAI_API_KEY, if set, overrides the keychain.
    Chat(chat::ChatArgs),
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

fn main() -> ExitCode {
    rozu_core::init_tracing();
    let cli = Cli::parse();

    let result = match cli.command {
        Some(Command::Doctor) => {
            for (name, version) in crate_versions() {
                println!("{name:<16}{version}");
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Auth { command }) => auth::run(command),
        Some(Command::Chat(args)) => chat::run(args),
        None => {
            println!(
                "rozu {}: try `rozu chat`, `rozu auth set openai` or `rozu --help`.",
                env!("CARGO_PKG_VERSION")
            );
            Ok(ExitCode::SUCCESS)
        }
    };
    result.unwrap_or_else(|err| {
        eprintln!("error: {err:#}");
        ExitCode::FAILURE
    })
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn chat_takes_an_unquoted_prompt() {
        let cli = Cli::try_parse_from(["rozu", "chat", "--model", "gpt-5", "list", "the", "files"])
            .unwrap();
        let Some(Command::Chat(args)) = cli.command else {
            panic!("expected chat");
        };
        let debug = format!("{args:?}");
        assert!(
            debug.contains("gpt-5") && debug.contains("\"files\""),
            "{debug}"
        );
    }
}
