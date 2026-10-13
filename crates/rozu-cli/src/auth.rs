//! `rozu auth`: manage provider API keys in the OS keychain.

use std::io::{self, IsTerminal, Read};
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use rozu_secrets::{KeyringStore, SERVICE, SecretStore, env_var_for};
use secrecy::SecretString;

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Store a provider API key in the OS keychain. Prompts without echo, or reads piped stdin.
    Set { provider: ProviderName },
    /// Show which providers have a key and where it comes from (never prints the key).
    Status,
    /// Remove a provider API key from the OS keychain.
    Delete { provider: ProviderName },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderName {
    Openai,
}

impl ProviderName {
    const ALL: [Self; 1] = [Self::Openai];

    pub fn id(self) -> &'static str {
        match self {
            Self::Openai => "openai",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Openai => "OpenAI",
        }
    }
}

pub fn run(command: AuthCommand) -> anyhow::Result<ExitCode> {
    let store = KeyringStore::default();
    match command {
        AuthCommand::Set { provider } => set(&store, provider),
        AuthCommand::Status => status(&store),
        AuthCommand::Delete { provider } => {
            if store.delete(provider.id())? {
                println!(
                    "Removed the {} API key from the keychain.",
                    provider.label()
                );
            } else {
                println!("No {} API key in the keychain.", provider.label());
            }
            Ok(ExitCode::SUCCESS)
        }
    }
}

fn set(store: &dyn SecretStore, provider: ProviderName) -> anyhow::Result<ExitCode> {
    let key = read_key(provider)?;
    store.set(provider.id(), &key)?;
    println!(
        "Saved the {} API key to the keychain (service {SERVICE}, account {}).",
        provider.label(),
        provider.id()
    );
    if let Some(var) = env_var_for(provider.id()).filter(|var| env_is_set(var)) {
        println!("Note: {var} is set in this shell and takes precedence over the keychain.");
    }
    Ok(ExitCode::SUCCESS)
}

/// Prompts on the terminal without echo, or reads all of stdin when it is piped.
fn read_key(provider: ProviderName) -> anyhow::Result<SecretString> {
    let raw = if io::stdin().is_terminal() {
        rpassword::prompt_password(format!("{} API key: ", provider.label()))
            .context("could not read the key from the terminal")?
    } else {
        let mut raw = String::new();
        io::stdin()
            .read_to_string(&mut raw)
            .context("could not read the key from stdin")?;
        raw
    };
    let key = raw.trim();
    if key.is_empty() {
        bail!("no API key given");
    }
    if provider == ProviderName::Openai && !key.starts_with("sk-") {
        eprintln!("warning: OpenAI API keys usually start with `sk-`; saving it anyway.");
    }
    Ok(SecretString::from(key.to_owned()))
}

fn status(store: &dyn SecretStore) -> anyhow::Result<ExitCode> {
    for provider in ProviderName::ALL {
        let keychain = match store.get(provider.id()) {
            Ok(Some(_)) => "stored".to_owned(),
            Ok(None) => "not stored".to_owned(),
            Err(err) => format!("unavailable ({err})"),
        };
        let env = env_var_for(provider.id()).filter(|var| env_is_set(var));
        match env {
            Some(var) => println!(
                "{}: using {var} from the environment (keychain: {keychain})",
                provider.id()
            ),
            None if keychain == "stored" => println!("{}: using the keychain", provider.id()),
            None => println!(
                "{}: not configured (keychain: {keychain}); run `rozu auth set {}` or set {}",
                provider.id(),
                provider.id(),
                env_var_for(provider.id()).unwrap_or("its API key variable"),
            ),
        }
    }
    Ok(ExitCode::SUCCESS)
}

fn env_is_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|value| !value.trim().is_empty())
}
