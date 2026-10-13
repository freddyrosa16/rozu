//! **Secondary mode** (behind a flag; ACP in [`super::GrokSession`] is the default): one
//! headless Grok Build run per turn,
//!
//! `grok --no-auto-update -p <turn> --output-format streaming-json [-m <model>] --tools use_tool
//! --disallowed-tools <built-ins> --no-subagents --no-plan --disable-web-search
//! --allow "MCPTool(rozu__*)" --system-prompt-override <profile> [-r <session>]`
//!
//! continuing Grok's session with `-r` from the second turn on. Limits compared with ACP:
//! headless mode has no flag to pass an MCP server, so rozu's tools are only there if the user
//! registered `rozu mcp-serve` with Grok themselves (rozu never edits `~/.grok`); there are no
//! permission requests (`--allow "MCPTool(rozu__*)"` lets rozu's tools run, `rozu mcp-serve`
//! still gates each call); cancel kills the run; each turn pays process start-up. Usage comes
//! from the `end` event, whose `input_tokens` is uncached only and is normalized.

use std::ffi::OsString;
use std::io;
use std::sync::Arc;

use async_trait::async_trait;
use rozu_core::{
    ChatRequest, EventStream, Provider, ProviderError, ProviderKind, ProviderResult, StreamEvent,
};
use serde_json::Value;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use super::protocol::{self, Mapper};
use super::{DEFAULT_MODEL, GrokConfig, HISTORY_HEADER, PROVIDER_ID};
use crate::process::CliProcess;
use crate::turn::turn_text_with;

/// Built-in tools denied on top of the `--tools use_tool` allowlist (belt and braces). These
/// are the names xAI's headless docs use; the full built-in list wasn't checked (unverified).
pub const DISALLOWED_BUILTINS: &[&str] = &[
    "run_terminal_cmd",
    "read_file",
    "grep",
    "list_dir",
    "search_replace",
    "web_search",
    "web_fetch",
    "Agent",
];

/// The headless Grok provider.
pub struct GrokHeadless {
    config: Arc<GrokConfig>,
    /// Grok's session id (from `end`), resumed with `-r`.
    session: Arc<Mutex<Option<String>>>,
}

impl GrokHeadless {
    pub fn new(config: GrokConfig) -> Self {
        Self {
            config: Arc::new(config),
            session: Arc::default(),
        }
    }

    /// The arguments of one headless run.
    pub fn args(
        &self,
        model: &str,
        text: &str,
        system_profile: &str,
        resume: Option<&str>,
    ) -> Vec<OsString> {
        let mut args: Vec<OsString> = vec![
            "--no-auto-update".into(),
            "-p".into(),
            text.into(),
            "--output-format".into(),
            "streaming-json".into(),
        ];
        if model != DEFAULT_MODEL {
            args.push("-m".into());
            args.push(model.into());
        }
        for arg in [
            "--tools",
            protocol::USE_TOOL,
            "--disallowed-tools",
            &DISALLOWED_BUILTINS.join(","),
            "--no-subagents",
            "--no-plan",
            "--disable-web-search",
            "--allow",
            "MCPTool(rozu__*)",
            "--system-prompt-override",
            system_profile,
        ] {
            args.push(arg.into());
        }
        if let Some(session) = resume {
            args.push("-r".into());
            args.push(session.into());
        }
        args
    }
}

/// What one headless event meant for the turn.
#[derive(Debug, Default, PartialEq)]
pub struct Step {
    pub events: Vec<StreamEvent>,
    pub done: bool,
    pub error: Option<ProviderError>,
    /// Grok's session id, from `end`.
    pub session: Option<String>,
}

/// Maps one `streaming-json` event.
pub fn map_event(mapper: &mut Mapper, event: &Value) -> Step {
    let mut step = Step::default();
    match event["type"].as_str().unwrap_or_default() {
        "text" => {
            if let Some(text) = event["data"].as_str().filter(|t| !t.is_empty()) {
                step.events.push(StreamEvent::TextDelta(text.to_owned()));
            }
        }
        "tool_call" | "tool_call_update" => step.events = mapper.map_update(event),
        "usage" => {
            info!(target: "rozu::usage", provider = "grok", mode = "headless",
                usage_raw = %event["usage"], "model call usage");
        }
        "end" => {
            info!(target: "rozu::usage", provider = "grok", mode = "headless",
                usage_raw = %event["usage"], model_usage = %event["modelUsage"],
                num_turns = event["num_turns"].as_u64().unwrap_or(0), "token usage");
            step.events
                .push(StreamEvent::Usage(protocol::usage_from_headless(
                    &event["usage"],
                )));
            step.session = event["sessionId"].as_str().map(str::to_owned);
            match protocol::stop_reason(event["stopReason"].as_str()) {
                Ok(stop) => step.events.push(StreamEvent::Done(stop)),
                Err(err) => step.error = Some(err),
            }
            step.done = true;
        }
        "error" => {
            step.error = Some(protocol::classify_error(
                event["message"].as_str().unwrap_or("grok failed"),
            ));
            step.done = true;
        }
        other => debug!(target: "rozu::cli", event = other, "ignored grok headless event"),
    }
    step
}

#[async_trait]
impl Provider for GrokHeadless {
    fn id(&self) -> &str {
        PROVIDER_ID
    }

    fn kind(&self) -> ProviderKind {
        ProviderKind::AgentBackend
    }

    async fn stream(
        &self,
        req: ChatRequest,
        cancel: CancellationToken,
    ) -> ProviderResult<EventStream> {
        let mut session = self.session.clone().lock_owned().await;
        let text = turn_text_with(&req, session.is_some(), HISTORY_HEADER);
        let args = self.args(
            &req.model,
            &text,
            &self.config.system_profile(&req),
            session.as_deref(),
        );
        let mut process = CliProcess::spawn(&self.config.command(args))
            .map_err(|err: io::Error| self.config.spawn_error(&err))?;
        Ok(Box::pin(async_stream::stream! {
            let mut mapper = Mapper::new("");
            loop {
                let line = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => None,
                    line = process.next_json() => Some(line),
                };
                let Some(line) = line else {
                    // Headless runs can't be cancelled politely; the session stays resumable.
                    process.kill();
                    yield Err(ProviderError::Cancelled);
                    break;
                };
                match line {
                    Ok(Some(event)) => {
                        let step = map_event(&mut mapper, &event);
                        if let Some(id) = step.session {
                            *session = Some(id);
                        }
                        for event in step.events {
                            yield Ok(event);
                        }
                        if let Some(err) = step.error {
                            yield Err(err);
                            break;
                        }
                        if step.done {
                            break;
                        }
                    }
                    Ok(None) => {
                        let stderr = process.stderr_tail().join("\n").to_lowercase();
                        yield Err(if protocol::looks_logged_out(&stderr) {
                            protocol::classify_error("not signed in")
                        } else {
                            ProviderError::UnexpectedEof
                        });
                        break;
                    }
                    Err(err) => {
                        yield Err(ProviderError::Decode(format!("grok output: {err}")));
                        break;
                    }
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use rozu_core::{StopReason, Usage};
    use serde_json::json;

    use super::*;

    #[test]
    fn maps_a_headless_run() {
        // Shapes from a Grok Build 1.0.52 `streaming-json` run (ids and signatures dropped).
        let mut mapper = Mapper::new("");
        let events = [
            json!({"type": "thought", "data": "Checking"}),
            json!({"type": "text", "data": "I'll check."}),
            json!({"type": "tool_call", "toolCallId": "c1", "title": "use_tool", "kind": "use_tool",
                "status": "pending", "toolName": "use_tool",
                "rawInput": {"tool_name": "rozu__grep", "tool_input": {"pattern": "x"}}}),
            json!({"type": "tool_call_update", "toolCallId": "c1", "status": "completed",
                "rawOutput": {"type": "MCP", "output": {"OkayOutput": "a.rs:1:x"}}}),
            json!({"type": "usage", "usage": {"input_tokens": 380, "cache_read_input_tokens": 4992}}),
            json!({"type": "end", "stopReason": "end_turn", "sessionId": "sess-1",
                "usage": {"input_tokens": 4220, "cache_read_input_tokens": 6144,
                    "cache_creation_input_tokens": 0, "output_tokens": 448}}),
        ];
        let steps: Vec<Step> = events.iter().map(|e| map_event(&mut mapper, e)).collect();
        assert_eq!(
            steps[1].events,
            [StreamEvent::TextDelta("I'll check.".into())]
        );
        assert_eq!(
            steps[2].events[0],
            StreamEvent::ToolCallStart {
                id: "c1".into(),
                name: "grep".into()
            }
        );
        assert_eq!(
            steps[3].events,
            [StreamEvent::ToolResult {
                id: "c1".into(),
                output: "a.rs:1:x".into(),
                is_error: false
            }]
        );
        assert!(steps[4].events.is_empty());
        let end = &steps[5];
        assert!(end.done);
        assert_eq!(end.session.as_deref(), Some("sess-1"));
        assert_eq!(
            end.events,
            [
                StreamEvent::Usage(Usage {
                    input_tokens: 10364,
                    output_tokens: 448,
                    cache_read_tokens: 6144,
                    cache_write_tokens: 0,
                }),
                StreamEvent::Done(StopReason::EndTurn),
            ]
        );
        let error = map_event(
            &mut mapper,
            &json!({"type": "error", "message": "Not signed in. Run grok login"}),
        );
        assert!(matches!(
            error.error,
            Some(ProviderError::NotLoggedIn { .. })
        ));
    }
}
