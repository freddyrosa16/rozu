//! `rozu grok-hook` (hidden): Grok Build's `PostToolUse` command hook for rozu's MCP tools. It
//! reads the hook event on stdin and prints Grok's `updatedToolOutput` JSON when the policy
//! changes the result; with no policy flag it is a no-op passthrough. See
//! `rozu_cli_providers::grok::hook`.

use std::io::{self, Read, Write};
use std::process::ExitCode;

use anyhow::Context;
use clap::Args;
use rozu_cli_providers::grok::hook::{HookPolicy, respond};

#[derive(Debug, Args)]
pub struct GrokHookArgs {
    /// Keep the head and tail of rozu tool results longer than this many bytes.
    #[arg(long, conflicts_with = "replace")]
    max_bytes: Option<usize>,
    /// Replace every rozu tool result with this text (`{tool}` = the tool's name).
    #[arg(long)]
    replace: Option<String>,
}

impl GrokHookArgs {
    fn policy(&self) -> HookPolicy {
        match (&self.max_bytes, &self.replace) {
            (Some(max_bytes), _) => HookPolicy::Truncate {
                max_bytes: *max_bytes,
            },
            (None, Some(text)) => HookPolicy::Replace { text: text.clone() },
            (None, None) => HookPolicy::Passthrough,
        }
    }
}

pub fn run(args: GrokHookArgs) -> anyhow::Result<ExitCode> {
    let mut input = String::new();
    io::stdin()
        .read_to_string(&mut input)
        .context("could not read the hook event")?;
    if let Some(output) = respond(&input, &args.policy()) {
        let mut stdout = io::stdout().lock();
        writeln!(stdout, "{output}").context("could not write the hook output")?;
    }
    // Always 0: exit 2 would feed stderr to the model, anything else is a recorded failure.
    Ok(ExitCode::SUCCESS)
}
