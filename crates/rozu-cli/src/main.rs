//! `rozu`: headless CLI driver for the agent, used for the dev loop and evals before the GUI exists.

mod approve;
mod auth;
mod chat;
mod grok_hook;
mod interrupt;
mod mcp_serve;
mod models;
mod sandbox_warning;
mod term;

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
    /// Print versions of the rozu crates, where logs go and how shell commands are sandboxed.
    Doctor,
    /// Manage provider API keys (stored in the OS keychain), check the claude, codex and grok
    /// CLIs, and start `codex login` or `grok login`.
    Auth {
        #[command(subcommand)]
        command: auth::AuthCommand,
    },
    /// Chat with the agent. OPENAI_API_KEY / ANTHROPIC_API_KEY / XAI_API_KEY, if set, override
    /// the keychain; `--provider claude-cli` uses your Claude subscription via the claude CLI,
    /// `--provider codex` your ChatGPT subscription via the Codex CLI, `--provider grok` your
    /// SuperGrok subscription via xAI's Grok Build CLI.
    Chat(chat::ChatArgs),
    /// List known models with context window and prices.
    Models,
    /// Run your shell inside a rozu PTY session (a smoke test for the terminal layer).
    Term,
    /// rozu's stdio MCP server, started by the claude or codex CLI (internal).
    #[command(hide = true)]
    McpServe(mcp_serve::McpServeArgs),
    /// Grok Build's PostToolUse hook for rozu's MCP tools (internal).
    #[command(hide = true)]
    GrokHook(grok_hook::GrokHookArgs),
}

/// Name and version of every crate linked into this binary.
fn crate_versions() -> [(&'static str, &'static str); 9] {
    [
        ("rozu-cli", env!("CARGO_PKG_VERSION")),
        ("rozu-core", rozu_core::VERSION),
        ("rozu-providers", rozu_providers::VERSION),
        ("rozu-cli-providers", rozu_cli_providers::VERSION),
        ("rozu-mcp", rozu_mcp::VERSION),
        ("rozu-tools", rozu_tools::VERSION),
        ("rozu-pty", rozu_pty::VERSION),
        ("rozu-secrets", rozu_secrets::VERSION),
        ("rozu-store", rozu_store::VERSION),
    ]
}

fn main() -> ExitCode {
    let log = rozu_core::init_tracing();
    let cli = Cli::parse();

    let result = match cli.command {
        Some(Command::Doctor) => {
            for (name, version) in crate_versions() {
                println!("{name:<20}{version}");
            }
            match log {
                rozu_core::LogTarget::File(path) => {
                    println!("{:<20}{}", "log file", path.display())
                }
                rozu_core::LogTarget::Stderr => println!("{:<20}stderr", "log file"),
                rozu_core::LogTarget::Off => println!("{:<20}off", "log file"),
            }
            let sandbox = rozu_tools::platform_sandbox();
            if sandbox.enforced() {
                println!("{:<20}{} (shell commands)", "sandbox", sandbox.name());
            } else {
                println!("{:<20}NONE (shell commands are not confined)", "sandbox");
                sandbox_warning::warn_if_unsandboxed();
            }
            Ok(ExitCode::SUCCESS)
        }
        Some(Command::Auth { command }) => auth::run(command),
        Some(Command::Chat(args)) => chat::run(args),
        Some(Command::Models) => models::run(),
        Some(Command::Term) => term::run(),
        Some(Command::McpServe(args)) => mcp_serve::run(args),
        Some(Command::GrokHook(args)) => grok_hook::run(args),
        None => {
            println!(
                "rozu {}: try `rozu chat`, `rozu chat --provider claude-cli`, `rozu chat --provider codex`, `rozu chat --provider grok`, `rozu models`, `rozu auth status` or `rozu --help`.",
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

    #[test]
    fn max_steps_must_be_1_to_200() {
        for bad in ["0", "201"] {
            let err = Cli::try_parse_from(["rozu", "chat", "--max-steps", bad, "hi"]).unwrap_err();
            assert_eq!(err.kind(), clap::error::ErrorKind::ValueValidation, "{bad}");
            assert!(err.to_string().contains("1..=200"), "{err}");
        }
        for good in ["1", "200"] {
            let cli = Cli::try_parse_from(["rozu", "chat", "--max-steps", good, "hi"]).unwrap();
            let debug = format!("{:?}", cli.command);
            assert!(debug.contains(&format!("max_steps: {good}")), "{debug}");
        }
    }

    #[test]
    fn chat_takes_provider_and_model() {
        let cli = Cli::try_parse_from([
            "rozu",
            "chat",
            "--provider",
            "anthropic",
            "--model",
            "claude-haiku-4-5",
            "hi",
        ])
        .unwrap();
        let debug = format!("{:?}", cli.command);
        assert!(
            debug.contains("\"anthropic\"") && debug.contains("\"claude-haiku-4-5\""),
            "{debug}"
        );
    }
}
