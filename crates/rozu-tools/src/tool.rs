//! The [`Tool`] trait, its supporting types and the [`ToolRegistry`].

use std::collections::BTreeMap;
use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::{AllowRule, NoSandbox, Sandbox, Workspace};

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
    /// Can't be read off the call: interpreter code (`python -c`, `eval`, …) or an output
    /// redirect to a file. Prompts in every mode (an "always allow" rule may cover a named
    /// script, never inline code).
    Opaque,
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
            Risk::Opaque => "opaque (interpreter code or output redirect)",
            Risk::Destructive => "destructive",
        })
    }
}

/// Writes outside the workspace that a call would make, as far as its arguments say.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OutsideWrites {
    /// Absolute paths, as written (not canonicalized).
    pub paths: Vec<PathBuf>,
    /// Some outside write can't be read off the arguments.
    pub unknown: bool,
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecPermissions {
    /// Run under [`ToolCtx::sandbox`]. False only for the user's explicit "run unsandboxed,
    /// this command only" (or where the OS has no sandbox).
    pub sandbox: bool,
    /// Allow network access inside the sandbox.
    pub network: bool,
    /// Extra paths (canonical) this one call may write inside the sandbox: the outside targets
    /// the user approved.
    pub extra_writable: Vec<PathBuf>,
}

impl Default for ExecPermissions {
    fn default() -> Self {
        Self {
            sandbox: true,
            network: false,
            extra_writable: Vec::new(),
        }
    }
}

impl ExecPermissions {
    /// No sandbox at all.
    pub fn unsandboxed() -> Self {
        Self {
            sandbox: false,
            network: true,
            extra_writable: Vec::new(),
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
    /// The session's private temp dir (see [`crate::SessionTemp`]): subprocesses get it as
    /// `$TMPDIR`, and it is the only temp location the sandbox lets them write.
    pub temp_dir: Option<PathBuf>,
}

impl ToolCtx {
    /// A context without an OS sandbox or temp dir (subprocesses still get the default
    /// permissions).
    pub fn new(workspace: Workspace, cancel: CancellationToken) -> Self {
        Self {
            workspace,
            cancel,
            sandbox: Arc::new(NoSandbox),
            permissions: ExecPermissions::default(),
            temp_dir: None,
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

    pub fn with_temp_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.temp_dir = Some(dir.into());
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

    /// Writes outside the workspace this call would make (only meaningful for sandboxed tools:
    /// an approved call may write exactly these paths).
    fn outside_writes(&self, _args: &Value, _ctx: &ToolCtx) -> OutsideWrites {
        OutsideWrites::default()
    }

    /// The rule "always allow" would store for this call, if the tool supports them.
    fn allow_rule(&self, _args: &Value) -> Option<AllowRule> {
        None
    }

    /// Whether stored "always allow" `rules` cover this call.
    fn rules_allow(&self, _args: &Value, _rules: &[AllowRule]) -> bool {
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
