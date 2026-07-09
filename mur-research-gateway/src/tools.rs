// mur-research-gateway/src/tools.rs
use serde::Serialize;

/// MCP tool definition returned by tools/list.
#[derive(Debug, Clone, Serialize)]
pub struct Tool {
    pub name: String,
    pub description: String,
    #[serde(rename = "inputSchema")] // MCP wire protocol is camelCase
    pub input_schema: serde_json::Value,
}

/// The two read-only verbs workers may call. No navigation, no POST.
pub fn all_tools() -> Vec<Tool> {
    vec![
        Tool {
            name: "search".into(),
            description: "Web search. Returns [{title,url,snippet}]. Read-only.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "number"}
                },
                "required": ["query"]
            }),
        },
        Tool {
            name: "fetch".into(),
            description: "Fetch one URL's readable text. Read-only GET. SSRF-guarded.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "render": {"type": "boolean"}
                },
                "required": ["url"]
            }),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declares_search_and_fetch() {
        let names: Vec<_> = all_tools().into_iter().map(|t| t.name).collect();
        assert_eq!(names, vec!["search", "fetch"]);
    }
}
