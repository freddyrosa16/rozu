//! `rozu auth`: manage provider API keys in the OS keychain, and report the claude CLI's login
//! (which the CLI itself manages; rozu never sees its credentials).

use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, bail};
use clap::{Subcommand, ValueEnum};
use rozu_cli_providers::claude::install::{
    AuthStatus, CLAUDE_BIN_ENV, claude_auth_status, claude_version, find_claude_binary,
};
use rozu_providers::registry::{self, CLAUDE_CLI_ID};
use rozu_secrets::{KeyringStore, SERVICE, SecretStore, env_var_for};
use secrecy::SecretString;

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Store a provider API key in the OS keychain. Prompts without echo, or reads piped stdin.
    Set { provider: ProviderName },
    /// Show which providers have a key and where it comes from (never prints the key), and
    /// whether the claude CLI is installed and logged in.
    Status,
    /// Remove a provider API key from the OS keychain.
    Delete { provider: ProviderName },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ProviderName {
    Openai,
    Anthropic,
    Xai,
}

impl ProviderName {
    const ALL: [Self; 3] = [Self::Openai, Self::Anthropic, Self::Xai];

    pub fn id(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
            Self::Xai => "xai",
        }
    }

    fn label(self) -> &'static str {
        registry::provider_info(self.id()).map_or("API", |p| p.label)
    }

    /// The prefix this provider's API keys usually start with.
    fn key_prefix(self) -> &'static str {
        match self {
            Self::Openai => "sk-",
            Self::Anthropic => "sk-ant-",
            Self::Xai => "xai-",
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
    if !key.starts_with(provider.key_prefix()) {
        eprintln!(
            "warning: {} API keys usually start with `{}`; saving it anyway.",
            provider.label(),
            provider.key_prefix()
        );
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
    let runtime = tokio::runtime::Runtime::new().context("could not start the async runtime")?;
    println!(
        "{}",
        runtime.block_on(claude_cli_status(find_claude_binary()))
    );
    Ok(ExitCode::SUCCESS)
}

/// One line about the claude CLI: found where, which version, logged in or not. Only `loggedIn`
/// and `authMethod` are read from `claude auth status`; nothing else it prints is shown.
async fn claude_cli_status(binary: Option<PathBuf>) -> String {
    let id = CLAUDE_CLI_ID;
    let Some(binary) = binary else {
        return format!(
            "{id}: claude CLI not found (PATH, ~/.local/bin); install Claude Code or set \
             {CLAUDE_BIN_ENV}"
        );
    };
    let version = match claude_version(&binary).await {
        Ok(version) => version,
        Err(err) => return format!("{id}: {} doesn't run ({err})", binary.display()),
    };
    let login = match claude_auth_status(&binary).await {
        Ok(AuthStatus {
            logged_in: Some(true),
            auth_method,
        }) => match auth_method {
            Some(method) => format!("logged in ({method})"),
            None => "logged in".to_owned(),
        },
        Ok(AuthStatus {
            logged_in: Some(false),
            ..
        }) => "not logged in; run `claude` and use /login".to_owned(),
        Ok(_) => "login status unknown (couldn't read `claude auth status`)".to_owned(),
        Err(err) => format!("login status unknown ({err})"),
    };
    format!("{id}: {} ({version}), {login}", binary.display())
}

#[cfg(all(test, unix))]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// The fake `claude` from the provider fixtures, wrapped so it finds its auth.json.
    fn fake_claude(dir: &std::path::Path) -> PathBuf {
        let fixtures = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../rozu-cli-providers/tests/fixtures/claude"
        );
        let wrapper = dir.join("claude");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nFAKE_CLAUDE_TURNS='{fixtures}' exec /bin/sh '{fixtures}/fake_claude.sh' \"$@\"\n"
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        wrapper
    }

    #[tokio::test]
    async fn claude_cli_status_shows_version_and_login_only() {
        let dir = tempfile::tempdir().unwrap();
        let line = claude_cli_status(Some(fake_claude(dir.path()))).await;
        assert!(line.starts_with("claude-cli: "), "{line}");
        assert!(line.ends_with("(9.9.9), logged in (claude.ai)"), "{line}");
        // Other fields of `claude auth status` never reach the output.
        assert!(
            !line.contains("example.invalid") && !line.contains("Fake Org"),
            "{line}"
        );

        let missing = claude_cli_status(None).await;
        assert!(
            missing.contains("not found") && missing.contains(CLAUDE_BIN_ENV),
            "{missing}"
        );
    }
}

fn env_is_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|value| !value.trim().is_empty())
}
