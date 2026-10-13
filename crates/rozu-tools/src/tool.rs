//! The [`Tool`] trait, its supporting types and the [`ToolRegistry`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{NoSandbox, Sandbox, Workspace};

/// How risky a particular tool call is; drives the approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Risk {
    /// Reads inside the workspace. Always auto-approved.
    Read,
    /// Writes inside the workspace.
    WriteWorkspace,
    /// Writes outside the workspace.
    WriteOutside,
    /// Talks to the network.
    Network,
    /// Hard to undo (`rm -rf`, force push, …). Always prompts.
    Destructive,
}

impl fmt::Display for Risk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Risk::Read => "read",
            Risk::WriteWorkspace => "write in workspace",
            Risk::WriteOutside => "write outside workspace",
            Risk::Network => "network",
            Risk::Destructive => "destructive",
        })
    }
}

/// The result of a tool call, sent back to the model as text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOutput {
    pub content: String,
    pub is_error: bool,
}

impl ToolOutput {
    pub fn ok(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: false,
        }
    }

    pub fn error(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            is_error: true,
        }
    }
}

/// What an approved subprocess may do. Decided per call by the approval policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecPermissions {
    /// Run under [`ToolCtx::sandbox`]. When false the command runs unrestricted.
    pub sandbox: bool,
    /// Allow network access inside the sandbox.
    pub network: bool,
}

impl Default for ExecPermissions {
    fn default() -> Self {
        Self {
            sandbox: true,
            network: false,
        }
    }
}

/// Per-call context: where the tool may operate, how subprocesses are confined and when to give
/// up.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    pub workspace: Workspace,
    pub cancel: CancellationToken,
    /// Sandbox for subprocesses (see [`crate::platform_sandbox`]).
    pub sandbox: Arc<dyn Sandbox>,
    pub permissions: ExecPermissions,
}

impl ToolCtx {
    /// A context without an OS sandbox (subprocesses still get the default permissions).
    pub fn new(workspace: Workspace, cancel: CancellationToken) -> Self {
        Self {
            workspace,
            cancel,
            sandbox: Arc::new(NoSandbox),
            permissions: ExecPermissions::default(),
        }
    }

    pub fn with_sandbox(mut self, sandbox: Arc<dyn Sandbox>) -> Self {
        self.sandbox = sandbox;
        self
    }

    pub fn with_permissions(mut self, permissions: ExecPermissions) -> Self {
        self.permissions = permissions;
        self
    }
}

/// Something the agent can call.
#[async_trait]
pub trait Tool: Send + Sync {
    /// Name the model calls the tool by. Must be unique within a registry.
    fn name(&self) -> &'static str;

    /// One-paragraph description for the model.
    fn description(&self) -> &'static str;

    /// JSON Schema of the arguments object (see [`schema_for`]).
    fn schema(&self) -> Value;

    /// Risk class of this particular call.
    fn risk(&self, args: &Value, ctx: &ToolCtx) -> Risk;

    /// Rejects a call before anyone is asked to approve it (bad arguments, a path outside the
    /// workspace), so the user never approves something that can't run.
    fn validate(&self, _args: &Value, _ctx: &ToolCtx) -> Result<(), ToolOutput> {
        Ok(())
    }

    /// What the call will do, shown verbatim on the approval prompt (the command, the diff…).
    fn preview(&self, args: &Value) -> String {
        serde_json::to_string_pretty(args).unwrap_or_else(|_| args.to_string())
    }

    /// Whether the tool runs a subprocess under [`ToolCtx::sandbox`].
    fn sandboxed(&self) -> bool {
        false
    }

    /// Whether this call needs network access (only meaningful for sandboxed tools).
    fn wants_network(&self, _args: &Value, _ctx: &ToolCtx) -> bool {
        false
    }

    /// How long the agent should let this call run, when the tool manages its own timeout.
    fn timeout(&self, _args: &Value) -> Option<Duration> {
        None
    }

    /// The prefix an "always allow" rule for this call would store, if the tool supports them.
    fn allow_prefix(&self, _args: &Value) -> Option<String> {
        None
    }

    /// Whether stored "always allow" `prefixes` cover this call.
    fn prefix_allows(&self, _args: &Value, _prefixes: &[String]) -> bool {
        false
    }

    /// Runs the tool. Failures are reported as [`ToolOutput::error`] so the model can react.
    async fn call(&self, args: Value, ctx: &ToolCtx) -> ToolOutput;
}

/// JSON Schema for `T`, trimmed of the metadata keys model APIs don't need.
pub fn schema_for<T: JsonSchema>() -> Value {
    let mut schema = serde_json::to_value(schemars::schema_for!(T)).expect("schema serializes");
    if let Some(map) = schema.as_object_mut() {
        map.remove("$schema");
        map.remove("title");
    }
    schema
}

/// Deserializes tool arguments, turning failures into an error result for the model.
pub fn parse_args<T: DeserializeOwned>(args: Value) -> Result<T, ToolOutput> {
    serde_json::from_value(args)
        .map_err(|err| ToolOutput::error(format!("invalid arguments: {err}")))
}

/// The set of tools offered to the model, keyed by name.
#[derive(Default, Clone)]
pub struct ToolRegistry {
    tools: BTreeMap<&'static str, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// A registry with the read-only file tools: `read_file`, `list_dir` and `grep`.
    pub fn read_only() -> Self {
        let mut registry = Self::new();
        registry.register(crate::ReadFile);
        registry.register(crate::ListDir);
        registry.register(crate::Grep);
        registry
    }

    /// The read-only tools plus `write_file`, `apply_patch` and `shell`.
    pub fn standard() -> Self {
        let mut registry = Self::read_only();
        registry.register(crate::WriteFile);
        registry.register(crate::ApplyPatch);
        registry.register(crate::Shell);
        registry
    }

    /// Adds `tool`, replacing any tool with the same name.
    pub fn register(&mut self, tool: impl Tool + 'static) {
        self.tools.insert(tool.name(), Arc::new(tool));
    }

    pub fn get(&self, name: &str) -> Option<&Arc<dyn Tool>> {
        self.tools.get(name)
    }

    /// Tools in name order.
    pub fn iter(&self) -> impl Iterator<Item = &Arc<dyn Tool>> {
        self.tools.values()
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}

impl fmt::Debug for ToolRegistry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.tools.keys()).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_registry_has_the_fs_tools() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolCtx::new(
            Workspace::new(dir.path()).unwrap(),
            CancellationToken::new(),
        );
        let registry = ToolRegistry::read_only();
        let names: Vec<_> = registry.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["grep", "list_dir", "read_file"]);
        for tool in registry.iter() {
            assert_eq!(tool.risk(&serde_json::json!({}), &ctx), Risk::Read);
        }
    }

    #[test]
    fn standard_registry_schemas_are_objects() {
        let registry = ToolRegistry::standard();
        let names: Vec<_> = registry.iter().map(|t| t.name()).collect();
        assert_eq!(
            names,
            [
                "apply_patch",
                "grep",
                "list_dir",
                "read_file",
                "shell",
                "write_file"
            ]
        );
        for tool in registry.iter() {
            let schema = tool.schema();
            assert_eq!(schema["type"], "object", "{}: {schema}", tool.name());
            assert!(schema.get("$schema").is_none());
        }
    }
}
