//! The [`Tool`] trait, its supporting types and the [`ToolRegistry`].

use std::collections::BTreeMap;
use std::fmt;
use std::sync::Arc;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::Workspace;

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

/// Per-call context: where the tool may operate and when to give up.
#[derive(Debug, Clone)]
pub struct ToolCtx {
    pub workspace: Workspace,
    pub cancel: CancellationToken,
}

impl ToolCtx {
    pub fn new(workspace: Workspace, cancel: CancellationToken) -> Self {
        Self { workspace, cancel }
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
    fn risk(&self, args: &Value) -> Risk;

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
        let registry = ToolRegistry::read_only();
        let names: Vec<_> = registry.iter().map(|t| t.name()).collect();
        assert_eq!(names, ["grep", "list_dir", "read_file"]);
        for tool in registry.iter() {
            assert_eq!(tool.risk(&serde_json::json!({})), Risk::Read);
            let schema = tool.schema();
            assert_eq!(schema["type"], "object", "{}: {schema}", tool.name());
            assert!(schema.get("$schema").is_none());
        }
    }
}
