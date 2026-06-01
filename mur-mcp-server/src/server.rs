// mur-mcp-server/src/server.rs
use crate::jsonrpc::{Request, Response};
use crate::tools;
use serde_json::{Value, json};

pub struct McpServer {
    /// Server name sent in initialize response.
    name: String,
    version: String,
    /// Whether initialize has been called.
    initialized: bool,
}

impl McpServer {
    pub fn new() -> Self {
        Self {
            name: "mur-mcp-server".into(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            initialized: false,
        }
    }

    pub async fn handle(&mut self, request: Request) -> Response {
        match request.method.as_str() {
            "initialize" => self.handle_initialize(request.id, &request.params),
            "tools/list" => {
                if !self.initialized {
                    return Response::error(
                        request.id,
                        -32002,
                        "Not initialized. Call 'initialize' first.".into(),
                    );
                }
                self.handle_tools_list(request.id)
            }
            "tools/call" => {
                if !self.initialized {
                    return Response::error(
                        request.id,
                        -32002,
                        "Not initialized. Call 'initialize' first.".into(),
                    );
                }
                self.handle_tools_call(request.id, &request.params).await
            }
            "notifications/initialized" => {
                self.initialized = true;
                tracing::info!("client confirmed initialization");
                Response {
                    jsonrpc: "2.0",
                    id: None,
                    result: None,
                    error: None,
                }
            }
            "" => Response::error(request.id, -32700, "Parse error".into()),
            _ => Response::error(
                request.id,
                -32601,
                format!("Method not found: {}", request.method),
            ),
        }
    }

    fn handle_initialize(&mut self, id: Option<Value>, _params: &Option<Value>) -> Response {
        Response::success(
            id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": self.name,
                    "version": self.version,
                },
            }),
        )
    }

    fn handle_tools_list(&self, id: Option<Value>) -> Response {
        let tools: Vec<Value> = tools::all_tools()
            .iter()
            .map(|t| serde_json::to_value(t).unwrap_or(Value::Null))
            .collect();
        Response::success(id, json!({ "tools": tools }))
    }

    async fn handle_tools_call(&self, id: Option<Value>, params: &Option<Value>) -> Response {
        let params = match params {
            Some(p) => p,
            None => {
                return Response::error(
                    id,
                    -32602,
                    "Missing params. Expected: {name: string, arguments: object}".into(),
                );
            }
        };

        let tool_name = match params.get("name").and_then(|v| v.as_str()) {
            Some(n) => n,
            None => return Response::error(id, -32602, "Missing 'name' in params".into()),
        };

        let arguments = params.get("arguments").unwrap_or(&Value::Null);

        match tools::call_tool(tool_name, arguments).await {
            Ok(result) => {
                let content = match result {
                    Value::String(s) => vec![json!({"type": "text", "text": s})],
                    other => vec![
                        json!({"type": "text", "text": serde_json::to_string_pretty(&other).unwrap_or_else(|_| format!("{:?}", other))}),
                    ],
                };
                Response::success(id, json!({ "content": content }))
            }
            Err(e) => {
                let content = vec![json!({"type": "text", "text": format!("Error: {}", e)})];
                Response::success(id, json!({"content": content, "isError": true}))
            }
        }
    }
}
