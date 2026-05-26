use std::sync::Arc;

/// Read-only snapshot of MCP tool names visible to one agent.
/// Cheap to build, cheap to clone — just `Vec<String>` behind an `Arc`.
/// Built once per inject pass and threaded into the resolver.
#[derive(Debug, Clone, Default)]
pub struct McpInventory {
    tools: Arc<Vec<String>>,
}

impl McpInventory {
    pub fn from_tool_names(tools: Vec<String>) -> Self {
        Self {
            tools: Arc::new(tools),
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t == name)
    }

    pub fn iter(&self) -> impl Iterator<Item = &str> {
        self.tools.iter().map(|s| s.as_str())
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
