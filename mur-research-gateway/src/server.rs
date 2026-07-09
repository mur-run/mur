// mur-research-gateway/src/server.rs
use crate::fetcher::{self, FetchError};
use crate::jsonrpc::{Request, Response};
use crate::tools;
use std::time::Duration;

/// Interim config (Task placeholder): env vars until config.yaml lands (Task 6).
// TODO(Task 6): read from config.yaml
fn deny_hosts_from_env() -> Vec<String> {
    std::env::var("MUR_RESEARCH_DENY_HOSTS")
        .ok()
        .map(|v| {
            v.split(',')
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

// TODO(Task 6): read from config.yaml
fn timeout_from_env() -> Duration {
    let secs = std::env::var("MUR_RESEARCH_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(20);
    Duration::from_secs(secs)
}

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
            "tools/call" => self.handle_tool_call(request.id, request.params).await,
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

    async fn handle_tool_call(
        &mut self,
        id: Option<serde_json::Value>,
        params: Option<serde_json::Value>,
    ) -> Response {
        let params = match params {
            Some(p) => p,
            None => return Response::error(id, -32602, "tools/call requires params".to_string()),
        };
        let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let args = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({}));
        match name {
            "fetch" => self.handle_fetch(id, args).await,
            "search" => Response::error(id, -32601, "search: not implemented yet".to_string()),
            _ => Response::error(id, -32602, format!("Unknown tool: {}", name)),
        }
    }

    async fn handle_fetch(
        &mut self,
        id: Option<serde_json::Value>,
        args: serde_json::Value,
    ) -> Response {
        let url = match args.get("url").and_then(|v| v.as_str()) {
            Some(u) => u.to_string(),
            None => return Response::error(id, -32602, "fetch requires 'url'".to_string()),
        };
        let render = args
            .get("render")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if render {
            // Headless-render tier is a later task; tier-1 handles render=false/absent only.
            return Response::error(
                id,
                -32601,
                "fetch: render=true (tier-2) not implemented yet".to_string(),
            );
        }
        let deny = deny_hosts_from_env();
        let timeout = timeout_from_env();
        match fetcher::fetch_tier1(&url, &deny, timeout).await {
            Ok(result) => Response::success(
                id,
                serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
            ),
            Err(FetchError::Guard(reject)) => Response::error(
                id,
                -32000,
                format!("fetch blocked by SSRF guard: {:?}", reject),
            ),
            Err(FetchError::Http(msg)) => {
                Response::error(id, -32001, format!("fetch failed: {}", msg))
            }
            Err(FetchError::TooLarge) => {
                Response::error(id, -32002, "fetch response exceeded size cap".to_string())
            }
        }
    }
}

impl Default for McpServer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, params: Option<serde_json::Value>) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(1)),
            method: method.to_string(),
            params,
        }
    }

    #[tokio::test]
    async fn fetch_call_rejects_private_target() {
        let mut server = McpServer::new();
        let resp = server
            .handle(req(
                "tools/call",
                Some(serde_json::json!({"name": "fetch", "arguments": {"url": "http://127.0.0.1:1/"}})),
            ))
            .await;
        assert!(
            resp.error.is_some(),
            "expected guard rejection, got {:?}",
            resp
        );
    }

    #[tokio::test]
    async fn fetch_call_missing_url_errors() {
        let mut server = McpServer::new();
        let resp = server
            .handle(req(
                "tools/call",
                Some(serde_json::json!({"name": "fetch", "arguments": {}})),
            ))
            .await;
        assert!(resp.error.is_some());
    }

    #[tokio::test]
    async fn unknown_tool_errors() {
        let mut server = McpServer::new();
        let resp = server
            .handle(req(
                "tools/call",
                Some(serde_json::json!({"name": "nope", "arguments": {}})),
            ))
            .await;
        assert!(resp.error.is_some());
    }
}
