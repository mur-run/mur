// mur-mcp-server/src/tools.rs
use serde::Serialize;
use serde_json::{json, Value};

use mur_core::cmd::notes_cmd;

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
        Tool {
            name: "mur_notes_search".into(),
            description: "Search MUR notes and patterns by keyword query. Returns ranked results with name, description, maturity, and relevance score.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(vec![
                    ("query".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Search query".into(),
                        default: None,
                    }),
                    ("limit".into(), ToolParam {
                        param_type: "integer".into(),
                        description: "Max results, 1-10 (default: 5)".into(),
                        default: Some(json!(5)),
                    }),
                ]),
                required: Some(vec!["query".into()]),
            },
        },
        Tool {
            name: "mur_notes_show".into(),
            description: "Load a specific note or pattern by name. Returns full body, metadata, maturity, and tags.".into(),
            input_schema: ToolInputSchema {
                schema_type: "object".into(),
                properties: Some(vec![
                    ("name".into(), ToolParam {
                        param_type: "string".into(),
                        description: "Note name (exact match)".into(),
                        default: None,
                    }),
                ]),
                required: Some(vec!["name".into()]),
            },
        },
    ]
}

/// Dispatch a tool call by name. Returns the result as a JSON Value.
/// Async because some tools (project_search, hook_context) need tokio.
pub async fn call_tool(name: &str, arguments: &Value) -> Result<Value, String> {
    match name {
        "mur_notes_search" => {
            let query = arguments.get("query")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'query' (string)".to_string())?;
            let limit = arguments.get("limit")
                .and_then(|v| v.as_u64())
                .unwrap_or(5)
                .clamp(1, 10) as usize;

            let home = resolve_mur_home().map_err(|e| format!("Failed to resolve MUR home: {}", e))?;
            let results = notes_cmd::do_search(&home, query, limit)
                .map_err(|e| format!("Search failed: {}", e))?;

            let items: Vec<Value> = results.iter().map(|scored| {
                json!({
                    "name": scored.item.manifest.name,
                    "description": scored.item.manifest.description,
                    "score": scored.score,
                    "maturity": format!("{:?}", scored.item.stats.lifecycle_state),
                })
            }).collect();

            Ok(json!({
                "results": items,
                "count": items.len(),
            }))
        }

        "mur_notes_show" => {
            let name = arguments.get("name")
                .and_then(|v| v.as_str())
                .ok_or_else(|| "Missing required parameter: 'name' (string)".to_string())?;

            let home = resolve_mur_home().map_err(|e| format!("Failed to resolve MUR home: {}", e))?;
            let view = notes_cmd::do_show(&home, name)
                .map_err(|e| format!("Note not found: {}", e))?;

            Ok(json!({
                "name": view.name,
                "description": view.description,
                "maturity": format!("{:?}", view.maturity),
                "body": view.body,
            }))
        }

        _ => Err(format!("Unknown tool: {}", name)),
    }
}

/// Resolve ~/.mur from environment or default.
fn resolve_mur_home() -> anyhow::Result<std::path::PathBuf> {
    mur_core::cmd::resolve_mur_home()
}
