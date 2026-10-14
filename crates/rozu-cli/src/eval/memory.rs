//! `rozu eval memory`: replays each scripted conversation in `evals/memory` under each memory
//! condition (a [`MemoryPolicy`] preset, so the eval measures what ships) and appends one JSON
//! row per run: tokens, cache reads/writes, cache hit rate, cost, latency and recall.
//!
//! Dry runs use [`RecallMock`], which reads files when told to, restates every `Remember:` line
//! it can still see when asked a question, and reports usage with prefix-cache hits estimated
//! from the previous request.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context, bail};
use async_trait::async_trait;
use clap::Args;
use futures::StreamExt;
use futures::stream;
use rozu_core::compact::SUMMARY_PROMPT;
use rozu_core::replay::simulated_usage;
use rozu_core::{
    Agent, AgentConfig, AgentError, ApprovalMode, ApprovalPolicy, ChatRequest, ContentPart,
    EventStream, MemoryPolicy, Message, ModelInfo, Provider, ProviderError, ProviderResult, Role,
    Session, StopReason, StreamEvent, new_session_id,
};
use rozu_tools::{ToolRegistry, platform_sandbox};
use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio_util::sync::CancellationToken;

use super::{EvalGate, Mode, cache_hit_rate, last_answer, quiet_turn, temp_repo};
use crate::approve::{ApprovalArg, CliGate};
use crate::chat::{AgentFactory, resolve_choice};

#[derive(Debug, Args)]
pub struct MemoryArgs {
    /// A memory preset (off, mask, summarize, default) or all.
    #[arg(long, default_value = "all")]
    condition: String,
    /// Runs per scenario and condition.
    #[arg(long, default_value_t = 1)]
    seeds: u32,
    #[arg(long, value_enum, default_value_t = Mode::Dry)]
    mode: Mode,
    #[arg(long, default_value = "evals/memory")]
    dir: PathBuf,
    /// Where the rows go (JSON lines).
    #[arg(long, default_value = "rozu-eval-memory.jsonl")]
    out: PathBuf,
    /// Skip runs already in --out (rate-limited ones run again).
    #[arg(long)]
    resume: bool,
    /// Space runs out to at most this many an hour.
    #[arg(long)]
    max_runs_per_hour: Option<u32>,
    /// For --mode live.
    #[arg(long, env = "ROZU_PROVIDER")]
    provider: Option<String>,
    /// For --mode live.
    #[arg(long, env = "ROZU_MODEL")]
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Scenario {
    name: String,
    /// The context window the run pretends the model has, so compaction kicks in early.
    context_window: u32,
    #[serde(default)]
    files: BTreeMap<String, String>,
    /// Pinned project memory (as ROZU.md would give).
    #[serde(default)]
    pinned: Option<String>,
    turns: Vec<String>,
    /// Facts the last answer should still know.
    expect: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Row {
    scenario: String,
    condition: String,
    seed: u32,
    mode: String,
    provider: String,
    model: String,
    /// ok, failed or rate_limited.
    status: String,
    input_tokens: u64,
    output_tokens: u64,
    cache_read_tokens: u64,
    /// `None` when the provider doesn't report cache writes.
    cache_write_tokens: Option<u64>,
    cache_hit_rate: Option<f64>,
    /// `None` for subscription (CLI-backed) or unpriced models.
    cost_usd: Option<f64>,
    latency_ms: u64,
    recall: f64,
    compactions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub async fn run(args: MemoryArgs) -> anyhow::Result<ExitCode> {
    let scenarios = load(&args.dir)?;
    let conditions: Vec<(&str, MemoryPolicy)> = match args.condition.as_str() {
        "all" => MemoryPolicy::PRESETS.to_vec(),
        name => {
            let preset = MemoryPolicy::PRESETS
                .iter()
                .find(|(n, _)| *n == name)
                .with_context(|| {
                    format!("unknown condition {name}; try off, mask, summarize, default or all")
                })?;
            vec![*preset]
        }
    };
    let choice = match args.mode {
        Mode::Dry => None,
        Mode::Live => Some(resolve_choice(
            args.provider.clone(),
            args.model.as_deref(),
        )?),
    };
    let done = if args.resume {
        finished_runs(&args.out)?
    } else if args.out.exists() {
        bail!(
            "{} exists; pass --resume to continue it or --out for a new file",
            args.out.display()
        );
    } else {
        HashSet::new()
    };
    let mut out = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&args.out)
        .with_context(|| format!("cannot write {}", args.out.display()))?;
    let interval = args
        .max_runs_per_hour
        .filter(|n| *n > 0)
        .map(|n| Duration::from_secs_f64(3600.0 / f64::from(n)));
    let mut last_start: Option<Instant> = None;
    let mut rows = Vec::new();

    for scenario in &scenarios {
        for (condition, policy) in &conditions {
            for seed in 0..args.seeds {
                let key = (scenario.name.clone(), condition.to_string(), seed);
                if done.contains(&key) {
                    continue;
                }
                if let (Some(interval), Some(last)) = (interval, last_start) {
                    tokio::time::sleep(interval.saturating_sub(last.elapsed())).await;
                }
                last_start = Some(Instant::now());
                let row = run_one(scenario, condition, *policy, seed, choice.as_ref()).await?;
                writeln!(out, "{}", serde_json::to_string(&row)?)?;
                out.flush()?;
                eprintln!(
                    "{} / {condition} / seed {seed}: {} · recall {:.2} · {} in ({} cached) / {} out",
                    scenario.name,
                    row.status,
                    row.recall,
                    row.input_tokens,
                    row.cache_read_tokens,
                    row.output_tokens
                );
                if row.status == "rate_limited" {
                    eprintln!(
                        "stopped: the provider is rate limited; rerun with --resume later to continue"
                    );
                    return Ok(ExitCode::SUCCESS);
                }
                rows.push(row);
            }
        }
    }
    summarize(&rows);
    Ok(if rows.iter().any(|r| r.status == "failed") {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    })
}

fn load(dir: &Path) -> anyhow::Result<Vec<Scenario>> {
    let mut paths: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("cannot read {}", dir.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    paths.sort();
    paths
        .iter()
        .map(|p| {
            let text = std::fs::read_to_string(p)?;
            serde_json::from_str(&text).with_context(|| format!("bad scenario {}", p.display()))
        })
        .collect()
}

fn finished_runs(out: &Path) -> anyhow::Result<HashSet<(String, String, u32)>> {
    let Ok(text) = std::fs::read_to_string(out) else {
        return Ok(HashSet::new());
    };
    Ok(text
        .lines()
        .filter_map(|line| serde_json::from_str::<Row>(line).ok())
        .filter(|row| row.status != "rate_limited")
        .map(|row| (row.scenario, row.condition, row.seed))
        .collect())
}

async fn run_one(
    scenario: &Scenario,
    condition: &str,
    policy: MemoryPolicy,
    seed: u32,
    choice: Option<&rozu_providers::ModelChoice>,
) -> anyhow::Result<Row> {
    let (_dir, workspace) = temp_repo(&scenario.files)?;
    let (agent, info, provider, model) = match choice {
        None => {
            let info = ModelInfo::subscription("recall-mock", "mock", scenario.context_window);
            let mut config = AgentConfig::new("mock").with_model_info(Some(info.clone()));
            config.memory = policy;
            let policy = ApprovalPolicy::for_project(ApprovalMode::ReadOnly, workspace.root())?;
            let agent = Agent::new(
                Arc::new(RecallMock::default()),
                Arc::new(ToolRegistry::standard()),
                workspace.clone(),
                config,
            )
            .with_approval_policy(Arc::new(policy))
            .with_approval_gate(EvalGate::new(true))
            .with_sandbox(platform_sandbox());
            (
                agent,
                Some(info),
                "recall-mock".to_owned(),
                "mock".to_owned(),
            )
        }
        Some(choice) => {
            let (gate, _asks) = CliGate::new(false);
            let mut factory =
                AgentFactory::new(&workspace, ApprovalArg::ReadOnly, gate, new_session_id())?;
            factory.memory = policy;
            let info = choice.info.clone().map(|mut info| {
                info.context_window = scenario.context_window;
                info
            });
            let mut choice = choice.clone();
            choice.info = info.clone();
            let agent = factory.build(&choice)?;
            (agent, info, choice.provider.id.to_owned(), choice.model)
        }
    };

    let mut session = Session::with_system_prompt(crate::chat::system_prompt(
        &workspace,
        ApprovalMode::ReadOnly,
    ));
    session
        .messages
        .extend(scenario.pinned.as_deref().map(Message::system));
    let started = Instant::now();
    let mut status = "ok";
    let mut error = None;
    let mut compactions = 0;
    for turn in &scenario.turns {
        match quiet_turn(&agent, &mut session, turn).await {
            Ok(_) => compactions += u32::from(session.last_compaction.is_some()),
            Err(AgentError::Provider(err)) => {
                status = if rate_limited(&err) {
                    "rate_limited"
                } else {
                    "failed"
                };
                error = Some(err.to_string());
                break;
            }
        }
    }
    let answer = last_answer(&session).to_lowercase();
    let recalled = scenario
        .expect
        .iter()
        .filter(|fact| answer.contains(&fact.to_lowercase()))
        .count();
    let usage = session.usage;
    Ok(Row {
        scenario: scenario.name.clone(),
        condition: condition.to_owned(),
        seed,
        mode: if choice.is_some() { "live" } else { "dry" }.to_owned(),
        provider,
        model,
        status: status.to_owned(),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cache_read_tokens: usage.cache_read_tokens,
        cache_write_tokens: info
            .as_ref()
            .filter(|i| i.caps.cache_write_reported)
            .map(|_| usage.cache_write_tokens),
        cache_hit_rate: cache_hit_rate(&usage),
        cost_usd: info
            .as_ref()
            .filter(|i| i.is_metered())
            .map(|_| session.cost_usd),
        latency_ms: started.elapsed().as_millis() as u64,
        recall: recalled as f64 / scenario.expect.len().max(1) as f64,
        compactions,
        error,
    })
}

fn rate_limited(err: &ProviderError) -> bool {
    match err {
        ProviderError::RateLimited { .. } => true,
        other => {
            let text = other.to_string().to_lowercase();
            text.contains("rate limit") || text.contains("usage limit")
        }
    }
}

fn summarize(rows: &[Row]) {
    let mut by_condition: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        by_condition.entry(&row.condition).or_default().push(row);
    }
    for (condition, rows) in by_condition {
        let n = rows.len() as f64;
        let recall = rows.iter().map(|r| r.recall).sum::<f64>() / n;
        let input: u64 = rows.iter().map(|r| r.input_tokens).sum();
        let cached: u64 = rows.iter().map(|r| r.cache_read_tokens).sum();
        let hit = if input == 0 {
            0.0
        } else {
            cached as f64 / input as f64
        };
        println!(
            "{}",
            json!({"condition": condition, "runs": rows.len(), "recall": recall,
                   "input_tokens": input, "cache_hit_rate": hit})
        );
    }
}

/// The dry-run model. See the module docs.
#[derive(Default)]
struct RecallMock {
    last: Mutex<Option<ChatRequest>>,
}

const FACT: &str = "Remember:";

fn facts(text: &str) -> Vec<String> {
    text.lines()
        .filter_map(|line| line.find(FACT).map(|i| line[i..].trim().to_owned()))
        .collect()
}

impl RecallMock {
    fn answer(req: &ChatRequest) -> Message {
        let visible = |messages: &[Message]| {
            let mut seen = Vec::new();
            for part in messages.iter().flat_map(|m| &m.content) {
                let text = match part {
                    ContentPart::Text { text } => text,
                    ContentPart::ToolResult { output, .. } => output,
                    ContentPart::ToolCall { .. } => continue,
                };
                for fact in facts(text) {
                    if !seen.contains(&fact) {
                        seen.push(fact);
                    }
                }
            }
            seen
        };
        if req.messages.first().map(Message::text_content).as_deref() == Some(SUMMARY_PROMPT) {
            let kept = visible(&req.messages[1..]);
            return Message::assistant(if kept.is_empty() {
                "- (nothing to keep)".to_owned()
            } else {
                kept.iter()
                    .map(|f| format!("- {f}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            });
        }
        let last = req.messages.last().expect("a request has messages");
        let text = last.text_content();
        if last.role == Role::User {
            if let Some(path) = text.strip_prefix("read ") {
                return Message::new(
                    Role::Assistant,
                    vec![ContentPart::ToolCall {
                        id: format!("call_{}", req.messages.len()),
                        name: "read_file".into(),
                        args_json: json!({"path": path.trim()}).to_string(),
                    }],
                );
            }
            if text.contains('?') {
                return Message::assistant(visible(&req.messages).join("\n"));
            }
        }
        Message::assistant("OK.")
    }
}

#[async_trait]
impl Provider for RecallMock {
    fn id(&self) -> &str {
        "recall-mock"
    }

    async fn stream(&self, req: ChatRequest, _: CancellationToken) -> ProviderResult<EventStream> {
        let answer = Self::answer(&req);
        let usage = {
            let mut last = self.last.lock().expect("mock lock");
            let usage = simulated_usage(last.as_ref(), &req, &answer);
            *last = Some(req);
            usage
        };
        let mut events = Vec::new();
        let mut stop = StopReason::EndTurn;
        for part in answer.content {
            match part {
                ContentPart::Text { text } => events.push(StreamEvent::TextDelta(text)),
                ContentPart::ToolCall {
                    id,
                    name,
                    args_json,
                } => {
                    stop = StopReason::ToolUse;
                    events.push(StreamEvent::ToolCallStart {
                        id: id.clone(),
                        name,
                    });
                    events.push(StreamEvent::ToolCallArgsDelta {
                        id: id.clone(),
                        json: args_json,
                    });
                    events.push(StreamEvent::ToolCallEnd { id });
                }
                ContentPart::ToolResult { .. } => {}
            }
        }
        events.push(StreamEvent::Usage(usage));
        events.push(StreamEvent::Done(stop));
        Ok(stream::iter(events.into_iter().map(Ok)).boxed())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rate_and_usage_limits_stop_the_run() {
        let limited = ProviderError::RateLimited {
            attempts: 3,
            message: "slow down".into(),
        };
        let usage = ProviderError::Api {
            code: None,
            message: "Claude usage limit reached".into(),
        };
        assert!(rate_limited(&limited) && rate_limited(&usage));
        assert!(!rate_limited(&ProviderError::UnexpectedEof));
    }
}
