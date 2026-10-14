//! `rozu auth`: manage provider API keys in the OS keychain, report the vendor CLIs' logins
//! (which the CLIs manage themselves; rozu never sees their credentials), and start the official
//! `codex login` / `grok login`.

use std::io::{self, IsTerminal, Read};
use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::{Context, anyhow, bail};
use clap::{Subcommand, ValueEnum};
use rozu_cli_providers::claude::install::{
    AuthStatus, CLAUDE_BIN_ENV, claude_auth_status, claude_version, find_claude_binary,
};
use rozu_cli_providers::codex::install::{
    CODEX_BIN_ENV, LoginStatus, codex_login_status, codex_version, find_codex_binary,
};
use rozu_cli_providers::grok::install::{find_grok_binary, grok_version};
use rozu_providers::registry::{self, CLAUDE_CLI_ID, CODEX_ID, GROK_ID};
use rozu_secrets::{KeyringStore, SERVICE, SecretStore, env_var_for};
use secrecy::SecretString;

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Store a provider API key in the OS keychain. Prompts without echo, or reads piped stdin.
    Set { provider: ProviderName },
    /// Show which providers have a key and where it comes from (never prints the key), and
    /// whether the claude and codex CLIs are installed and logged in.
    Status,
    /// Remove a provider API key from the OS keychain.
    Delete { provider: ProviderName },
    /// Log in to a subscription with the vendor's own CLI (`codex`: runs the official
    /// `codex login`, which opens the browser for Sign in with ChatGPT; `grok`: runs xAI's
    /// official `grok login`). rozu never sees the credentials.
    Login { cli: LoginCli },
}

/// CLIs `rozu auth login` can start the login of.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum LoginCli {
    Codex,
    Grok,
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
        AuthCommand::Login {
            cli: LoginCli::Codex,
        } => codex_login(),
        AuthCommand::Login {
            cli: LoginCli::Grok,
        } => grok_login(),
    }
}

/// Runs xAI's official `grok login` with the terminal attached (inherited stdio). Grok Build
/// does the whole login and stores its own credentials; rozu only passes its exit status on.
fn grok_login() -> anyhow::Result<ExitCode> {
    let binary = find_grok_binary().map_err(|err| anyhow!(err))?;
    eprintln!(
        "Running the official `grok login` ({}). Grok Build keeps its own credentials; rozu \
         never sees them.",
        binary.display()
    );
    let status = std::process::Command::new(&binary)
        .arg("login")
        .status()
        .with_context(|| format!("could not run {}", binary.display()))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        None => ExitCode::FAILURE,
    })
}

/// Runs the official `codex login` with the terminal attached. Codex does the whole login and
/// stores its own credentials; rozu only passes its exit status on.
fn codex_login() -> anyhow::Result<ExitCode> {
    let binary = find_codex_binary().ok_or_else(|| {
        anyhow!(
            "the Codex CLI isn't installed (looked on PATH and in /opt/homebrew/bin); install it \
             or set {CODEX_BIN_ENV}"
        )
    })?;
    eprintln!(
        "Running the official `codex login` ({}). Codex keeps its own credentials; rozu never \
         sees them.",
        binary.display()
    );
    let status = std::process::Command::new(&binary)
        .arg("login")
        .status()
        .with_context(|| format!("could not run {}", binary.display()))?;
    Ok(match status.code() {
        Some(0) => ExitCode::SUCCESS,
        Some(code) => ExitCode::from(u8::try_from(code).unwrap_or(1)),
        None => ExitCode::FAILURE,
    })
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
    println!(
        "{}",
        runtime.block_on(codex_cli_status(find_codex_binary()))
    );
    println!("{}", runtime.block_on(grok_cli_status(find_grok_binary())));
    Ok(ExitCode::SUCCESS)
}

/// One line about Grok Build: found where (the native binary) and which version. Grok Build
/// has no login-status command, so whether it is signed in stays unknown here; rozu never
/// reads its credential file to find out.
async fn grok_cli_status(binary: Result<PathBuf, String>) -> String {
    let id = GROK_ID;
    let binary = match binary {
        Ok(binary) => binary,
        Err(err) => return format!("{id}: {err}"),
    };
    let version = match grok_version(&binary).await {
        Ok(version) => version,
        Err(err) => return format!("{id}: {} doesn't run ({err})", binary.display()),
    };
    format!(
        "{id}: {} ({version}), login status unknown (Grok Build has no status command); run \
         `rozu auth login grok` if a chat says it isn't signed in",
        binary.display()
    )
}

/// One line about the Codex CLI: found where, which version, logged in or not (and how). Only
/// the auth mode is taken from `codex login status`; nothing it prints is shown.
async fn codex_cli_status(binary: Option<PathBuf>) -> String {
    let id = CODEX_ID;
    let Some(binary) = binary else {
        return format!(
            "{id}: codex CLI not found (PATH, /opt/homebrew/bin); install the Codex CLI or set \
             {CODEX_BIN_ENV}"
        );
    };
    let version = match codex_version(&binary).await {
        Ok(version) => version,
        Err(err) => return format!("{id}: {} doesn't run ({err})", binary.display()),
    };
    let login = match codex_login_status(&binary).await {
        Ok(LoginStatus::ChatGpt) => "logged in (ChatGPT)".to_owned(),
        Ok(LoginStatus::ApiKey) => "logged in (API key)".to_owned(),
        Ok(LoginStatus::NotLoggedIn) => "not logged in; run `rozu auth login codex`".to_owned(),
        Ok(LoginStatus::Unknown) => {
            "login status unknown (couldn't read `codex login status`)".to_owned()
        }
        Err(err) => format!("login status unknown ({err})"),
    };
    format!("{id}: {} ({version}), {login}", binary.display())
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

    /// Writes an executable script, then waits out ETXTBSY: a parallel test's fork can briefly
    /// hold the write fd open until it execs.
    fn exe(path: &std::path::Path, body: &str) {
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        for _ in 0..200 {
            match std::process::Command::new(path)
                .arg("--version")
                .stdin(std::process::Stdio::null())
                .output()
            {
                Err(e) if e.raw_os_error() == Some(26) => {
                    std::thread::sleep(std::time::Duration::from_millis(10))
                }
                _ => return,
            }
        }
    }

    /// The fake `claude` from the provider fixtures, wrapped so it finds its auth.json.
    fn fake_claude(dir: &std::path::Path) -> PathBuf {
        let fixtures = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../rozu-cli-providers/tests/fixtures/claude"
        );
        let wrapper = dir.join("claude");
        exe(
            &wrapper,
            &format!(
                "#!/bin/sh\nFAKE_CLAUDE_TURNS='{fixtures}' exec /bin/sh '{fixtures}/fake_claude.sh' \"$@\"\n"
            ),
        );
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

    /// The fake `codex` from the provider fixtures, printing `status` for `codex login status`.
    fn fake_codex(dir: &std::path::Path, status: &str) -> PathBuf {
        let fake = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../rozu-cli-providers/tests/fixtures/codex/fake_codex.sh"
        );
        let wrapper = dir.join(format!("codex-{}", status.len()));
        exe(
            &wrapper,
            &format!(
                "#!/bin/sh\nFAKE_CODEX_LOGIN_STATUS='{status}' exec /bin/sh '{fake}' \"$@\"\n"
            ),
        );
        wrapper
    }

    #[tokio::test]
    async fn codex_cli_status_shows_version_and_auth_mode_only() {
        let dir = tempfile::tempdir().unwrap();
        let line = codex_cli_status(Some(fake_codex(dir.path(), "Logged in using ChatGPT"))).await;
        assert!(line.starts_with("codex: "), "{line}");
        assert!(line.ends_with("(0.147.0), logged in (ChatGPT)"), "{line}");

        // The masked key `codex login status` prints is never shown.
        let line = codex_cli_status(Some(fake_codex(
            dir.path(),
            "Logged in using an API key - sk-proj-***ABCD",
        )))
        .await;
        assert!(line.ends_with("logged in (API key)"), "{line}");
        assert!(!line.contains("ABCD"), "{line}");

        let line = codex_cli_status(Some(fake_codex(dir.path(), "Not logged in"))).await;
        assert!(line.contains("run `rozu auth login codex`"), "{line}");

        let missing = codex_cli_status(None).await;
        assert!(
            missing.contains("not found") && missing.contains(CODEX_BIN_ENV),
            "{missing}"
        );
    }

    #[tokio::test]
    async fn grok_cli_status_shows_binary_and_version_only() {
        let fake = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../rozu-cli-providers/tests/fixtures/grok/fake_grok.sh"
        );
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("grok");
        exe(&bin, &std::fs::read_to_string(fake).unwrap());
        let line = grok_cli_status(Ok(bin)).await;
        assert!(line.starts_with("grok: "), "{line}");
        assert!(line.contains("(1.0.52), login status unknown"), "{line}");
        assert!(line.contains("rozu auth login grok"), "{line}");

        let missing = grok_cli_status(Err("Grok Build isn't installed".into())).await;
        assert_eq!(missing, "grok: Grok Build isn't installed");
    }
}

fn env_is_set(var: &str) -> bool {
    std::env::var(var).is_ok_and(|value| !value.trim().is_empty())
}
