// mur-research-gateway/src/server.rs
use crate::jsonrpc::{Request, Response};
use crate::tools;

pub struct McpServer;

impl McpServer {
    pub fn new() -> Self {
        McpServer
    }

    pub async fn handle(&mut self, request: Request) -> Response {
        match request.method.as_str() {
            "initialize" => Response::success(
                request.id,
                serde_json::json!({
                    "protocolVersion": "2025-11-25",
                    "capabilities": {"tools": {}},
                    "serverInfo": {
                        "name": "mur-research-gateway",
                        "version": env!("CARGO_PKG_VERSION"),
                    },
                }),
            ),
            "tools/list" => {
                let tools: Vec<serde_json::Value> = tools::all_tools()
                    .iter()
                    .map(|t| serde_json::to_value(t).unwrap_or(serde_json::Value::Null))
                    .collect();
                Response::success(request.id, serde_json::json!({ "tools": tools }))
            }
            "tools/call" => Response::error(request.id, -32601, "not implemented yet".to_string()),
            "notifications/initialized" => Response {
                jsonrpc: "2.0",
                id: None,
                result: None,
                error: None,
            },
            "" => Response::error(request.id, -32700, "Parse error".to_string()),
            _ => Response::error(
                request.id,
                -32601,
                format!("Method not found: {}", request.method),
            ),
        }
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}
