// mur-mcp-server/src/tools.rs
use serde::Serialize;
use serde_json::Value;

/// JSON Schema for a tool parameter (MCP uses JSON Schema subset).
#[derive(Debug, Clone, Serialize)]
pub struct ToolParam {
    #[serde(rename = "type")]
    pub param_type: String,
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
}

/// MCP tool definition returned by tools/list.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")]
    pub input_schema: ToolInputSchema,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolInputSchema {
    #[serde(rename = "type")]
    pub schema_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub properties: Option<Vec<(String, ToolParam)>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub required: Option<Vec<String>>,
}

/// Return all registered tools.
pub fn all_tools() -> Vec<Tool> {
    vec![
        // Task 4 will populate these
    ]
}

/// Dispatch a tool call by name. Returns the result as a JSON Value.
/// Async because some tools (project_search, hook_context) need tokio.
pub async fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        other => Err(format!("Unknown tool: {}", other)),
    }
}
