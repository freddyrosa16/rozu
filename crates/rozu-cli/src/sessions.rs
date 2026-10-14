//! `rozu sessions`: list saved chats, newest first.

use std::path::PathBuf;
use std::process::ExitCode;

use anyhow::Context;
use clap::Args;
use rozu_store::{SessionStore, unix_now};
use rozu_tools::Workspace;

#[derive(Debug, Args)]
pub struct SessionsArgs {
    /// Only chats in this workspace.
    #[arg(long, default_value = ".")]
    workspace: PathBuf,
    /// Chats in every workspace.
    #[arg(long)]
    all: bool,
    #[arg(long, default_value_t = 20)]
    limit: usize,
    /// Text to look for in the first message (or an id prefix).
    query: Option<String>,
}

pub fn run(args: SessionsArgs) -> anyhow::Result<ExitCode> {
    let store = SessionStore::open_default().context("cannot open the saved sessions")?;
    let workspace = Workspace::new(&args.workspace)
        .with_context(|| format!("cannot open workspace {}", args.workspace.display()))?;
    let filter = (!args.all).then(|| workspace.root());
    let rows = store.list(filter, args.query.as_deref(), args.limit)?;
    if rows.is_empty() {
        eprintln!(
            "no saved chats{}",
            if args.all {
                ""
            } else {
                " in this workspace (try --all)"
            }
        );
        return Ok(ExitCode::SUCCESS);
    }
    let now = unix_now();
    for row in rows {
        let mut line = format!(
            "{}  {:>8}  {}/{}  {} msgs  {}",
            row.id,
            age(now.saturating_sub(row.updated)),
            row.provider,
            row.model,
            row.messages,
            row.title
        );
        if args.all {
            line.push_str(&format!("  ({})", row.workspace.display()));
        }
        println!("{line}");
    }
    Ok(ExitCode::SUCCESS)
}

fn age(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{}m ago", secs / 60),
        3_600..86_400 => format!("{}h ago", secs / 3_600),
        _ => format!("{}d ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn ages() {
        assert_eq!(super::age(5), "just now");
        assert_eq!(super::age(125), "2m ago");
        assert_eq!(super::age(7_200), "2h ago");
        assert_eq!(super::age(200_000), "2d ago");
    }
}
