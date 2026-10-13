//! The MCP server: newline-delimited JSON-RPC 2.0 over stdio (MCP's stdio transport), with
//! `initialize`, `ping`, `tools/list` and `tools/call`.
//!
//! Requests are handled concurrently (a tool waiting for the user's approval doesn't block a
//! read), and responses are written as they finish. At end of input the server waits for the
//! calls in flight, then returns.

use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use rozu_core::{AgentEvent, Decision, DecisionSource, ToolExecutor, log_decision};
use rozu_tools::{MAX_OUTPUT_BYTES, ToolOutput, Workspace, truncate_head_tail};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::sync::mpsc;
use tokio::task::JoinSet;
use tokio_util::codec::{FramedRead, LinesCodec};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};

use crate::bridge::{SocketGate, next_line};
use crate::claude::{PermissionRequest, map_permission_request, permission_response};
use crate::jsonrpc::{INVALID_REQUEST, METHOD_NOT_FOUND, PARSE_ERROR, RpcError, failure, success};

/// The protocol revision this server implements (and answers with when the client asks for one
/// it doesn't know).
pub const PROTOCOL_VERSION: &str = "2025-06-18";
/// Revisions whose subset used here (initialize, tools/list, tools/call) is the same.
const SUPPORTED_VERSIONS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18"];
pub const SERVER_NAME: &str = "rozu";
/// Name of the permission-prompt tool (`mcp__rozu__approve` to Claude).
pub const APPROVE_TOOL: &str = "approve";
/// Longest JSON-RPC message accepted.
const MAX_MESSAGE_BYTES: usize = 16 << 20;

/// Serves rozu's tools and the `approve` tool.
pub struct McpServer {
    exec: ToolExecutor,
    workspace: Workspace,
    /// Where decision-log entries go (the parent rozu process).
    bridge: SocketGate,
    cancel: CancellationToken,
    next_call: AtomicU64,
}

impl McpServer {
    /// `exec`'s gate asks the user (normally a [`SocketGate`]); `bridge` receives the decision
    /// log.
    pub fn new(exec: ToolExecutor, workspace: Workspace, bridge: SocketGate) -> Self {
        Self {
            exec,
            workspace,
            bridge,
            cancel: CancellationToken::new(),
            next_call: AtomicU64::new(1),
        }
    }

    /// Reads requests from `input` and writes responses to `output` until `input` ends.
    pub async fn serve<R, W>(self: Arc<Self>, input: R, output: W) -> io::Result<()>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let writer = tokio::spawn(async move {
            let mut output = output;
            while let Some(message) = rx.recv().await {
                let mut line = message.to_string();
                line.push('\n');
                output.write_all(line.as_bytes()).await?;
                output.flush().await?;
            }
            io::Result::Ok(())
        });

        let mut lines = FramedRead::new(input, LinesCodec::new_with_max_length(MAX_MESSAGE_BYTES));
        let mut calls = JoinSet::new();
        loop {
            match next_line(&mut lines).await {
                Ok(Some(line)) if line.trim().is_empty() => {}
                Ok(Some(line)) => {
                    let server = self.clone();
                    let tx = tx.clone();
                    calls.spawn(async move {
                        if let Some(response) = server.handle_line(&line).await {
                            let _ = tx.send(response);
                        }
                    });
                }
                Ok(None) => break,
                Err(err) => {
                    // The codec can't resync after an error, so this ends the session.
                    warn!(error = %err, "unreadable MCP input");
                    let _ = tx.send(failure(
                        Value::Null,
                        RpcError::new(PARSE_ERROR, err.to_string()),
                    ));
                    break;
                }
            }
        }
        while calls.join_next().await.is_some() {}
        drop(tx);
        writer.await.map_err(io::Error::other)?
    }

    /// Handles one message; `None` for notifications and stray responses.
    pub async fn handle_line(&self, line: &str) -> Option<Value> {
        let message: Value = match serde_json::from_str(line) {
            Ok(message) => message,
            Err(err) => {
                return Some(failure(
                    Value::Null,
                    RpcError::new(PARSE_ERROR, format!("invalid JSON: {err}")),
                ));
            }
        };
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        match (method, id) {
            (Some(method), Some(id)) => {
                let params = message.get("params").cloned().unwrap_or(Value::Null);
                Some(match self.request(method, params).await {
                    Ok(result) => success(id, result),
                    Err(err) => failure(id, err),
                })
            }
            (Some(method), None) => {
                debug!(method, "MCP notification");
                None
            }
            // A response to a request; this server never sends any.
            (None, Some(_)) if message.get("result").or(message.get("error")).is_some() => None,
            _ => Some(failure(
                Value::Null,
                RpcError::new(INVALID_REQUEST, "not a JSON-RPC request"),
            )),
        }
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => Ok(self.initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": self.tool_list() })),
            "tools/call" => self.call(params).await,
            _ => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("method not found: {method}"),
            )),
        }
    }

    fn initialize(&self, params: &Value) -> Value {
        let requested = params.get("protocolVersion").and_then(Value::as_str);
        let version = requested
            .filter(|v| SUPPORTED_VERSIONS.contains(v))
            .unwrap_or(PROTOCOL_VERSION);
        let client = params
            .pointer("/clientInfo/name")
            .and_then(Value::as_str)
            .unwrap_or("?");
        info!(client, version, "MCP session started");
        json!({
            "protocolVersion": version,
            "capabilities": { "tools": { "listChanged": false } },
            "serverInfo": { "name": SERVER_NAME, "version": env!("CARGO_PKG_VERSION") },
            "instructions": "rozu's workspace tools. Every call goes through rozu's approval \
                policy; the user may deny it with a reason.",
        })
    }

    /// `approve` plus every tool in the registry.
    fn tool_list(&self) -> Vec<Value> {
        let mut tools = vec![json!({
            "name": APPROVE_TOOL,
            "description": "rozu's permission prompt: decides whether a tool call may run.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "tool_name": { "type": "string" },
                    "input": { "type": "object" },
                    "tool_use_id": { "type": "string" },
                },
                "required": ["tool_name", "input"],
            },
        })];
        tools.extend(self.exec.tools.iter().map(|tool| {
            json!({
                "name": tool.name(),
                "description": tool.description(),
                "inputSchema": tool.schema(),
            })
        }));
        tools
    }

    async fn call(&self, params: Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("tools/call needs a tool name"))?;
        let arguments = params.get("arguments").cloned().unwrap_or(json!({}));
        if name == APPROVE_TOOL {
            let text = self.approve(&arguments).await?;
            return Ok(tool_result(ToolOutput::ok(text)));
        }
        if self.exec.tools.get(name).is_none() {
            return Err(RpcError::invalid_params(format!("unknown tool: {name}")));
        }
        let call_id = self.call_id();
        let ctx = self.exec.ctx(self.workspace.clone(), self.cancel.clone());
        // Forward the decision as soon as it's made, not after the (maybe long) call.
        let (events, records) = mpsc::channel(4);
        let run = async {
            let output = self
                .exec
                .execute(&call_id, name, &arguments.to_string(), &ctx, &events)
                .await;
            drop(events);
            output
        };
        let (output, ()) = tokio::join!(run, self.forward_records(records));
        Ok(tool_result(output))
    }

    /// The permission-prompt tool: returns the JSON text Claude expects.
    async fn approve(&self, arguments: &Value) -> Result<String, RpcError> {
        let tool_name = arguments
            .get("tool_name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::invalid_params("approve needs tool_name"))?;
        let input = arguments.get("input").cloned().unwrap_or(json!({}));
        let call_id = arguments
            .get("tool_use_id")
            .and_then(Value::as_str)
            .map_or_else(|| self.call_id(), str::to_owned);
        let ctx = self.exec.ctx(self.workspace.clone(), self.cancel.clone());
        let mapped = map_permission_request(tool_name, &input, &call_id, &ctx, &self.exec.policy);

        let (decision, request, source) = match mapped {
            PermissionRequest::RozuTool(name) => {
                debug!(tool = %name, %call_id,
                    "permission prompt for a rozu tool: allowed, gated when it runs");
                return Ok(permission_response(true, &input, ""));
            }
            PermissionRequest::Policy { request, rule_tool } => {
                let (events, records) = mpsc::channel(4);
                let (decision, _) = self
                    .exec
                    .decide(&request, rule_tool, &self.cancel, &events)
                    .await;
                drop(events);
                self.forward_records(records).await;
                return Ok(match decision {
                    Decision::Deny { reason } => {
                        permission_response(false, &input, &format!("denied by rozu: {reason}"))
                    }
                    _ => permission_response(true, &input, ""),
                });
            }
            PermissionRequest::AskAlways(request) => {
                let decision = self.exec.gate.decide(request.clone()).await;
                (decision, request, DecisionSource::Gate)
            }
            PermissionRequest::Deny { request, reason } => {
                (Decision::Deny { reason }, request, DecisionSource::Policy)
            }
        };
        self.bridge
            .send_record(log_decision(&request, &decision, &source))
            .await;
        Ok(match decision {
            Decision::Deny { reason } => {
                permission_response(false, &input, &format!("denied by rozu: {reason}"))
            }
            _ => permission_response(true, &input, ""),
        })
    }

    /// Sends the decisions logged on `records` (whose senders are gone) to the parent.
    async fn forward_records(&self, mut records: mpsc::Receiver<AgentEvent>) {
        while let Some(event) = records.recv().await {
            if let AgentEvent::Approval(record) = event {
                self.bridge.send_record(record).await;
            }
        }
    }

    fn call_id(&self) -> String {
        format!("mcp-{}", self.next_call.fetch_add(1, Ordering::Relaxed))
    }
}

fn tool_result(output: ToolOutput) -> Value {
    json!({
        "content": [{
            "type": "text",
            "text": truncate_head_tail(&output.content, MAX_OUTPUT_BYTES),
        }],
        "isError": output.is_error,
    })
}
