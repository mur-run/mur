// mur-research-gateway/src/server.rs
use crate::browser::{self, BrowserCfg};
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

/// Default installed Lightpanda path (`~/.mur/aura/lightpanda`, verified
/// present 2026-07-08 — `gotcha_agent_browser_lightpanda_engine_dead`). Only
/// used when `MUR_RESEARCH_LIGHTPANDA_PATH` is unset AND the default actually
/// exists on disk — never claim a path that isn't there.
// TODO(Task 6): read from config.yaml
fn default_lightpanda_path() -> Option<String> {
    let home = std::env::var("HOME").ok()?;
    let path = format!("{home}/.mur/aura/lightpanda");
    std::path::Path::new(&path).exists().then_some(path)
}

// TODO(Task 6): read from config.yaml
pub(crate) fn browser_cfg_from_env() -> BrowserCfg {
    let agent_browser_bin = std::env::var("MUR_RESEARCH_AGENT_BROWSER_BIN")
        .unwrap_or_else(|_| "agent-browser".to_string());
    let lightpanda_path = std::env::var("MUR_RESEARCH_LIGHTPANDA_PATH")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(default_lightpanda_path);
    let chrome_stealth_args = std::env::var("MUR_RESEARCH_CHROME_STEALTH_ARGS")
        .unwrap_or_else(|_| "--no-sandbox,--disable-blink-features=AutomationControlled".into());
    BrowserCfg {
        agent_browser_bin,
        lightpanda_path,
        chrome_stealth_args,
    }
}

fn fetch_error_response(id: Option<serde_json::Value>, verb: &str, err: FetchError) -> Response {
    match err {
        FetchError::Guard(reject) => Response::error(
            id,
            -32000,
            format!("{verb} blocked by SSRF guard: {:?}", reject),
        ),
        FetchError::Http(msg) => Response::error(id, -32001, format!("{verb} failed: {msg}")),
        FetchError::TooLarge => {
            Response::error(id, -32002, format!("{verb} response exceeded size cap"))
        }
    }
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
            "search" => self.handle_search(id, args).await,
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
        let deny = deny_hosts_from_env();
        let timeout = timeout_from_env();
        if render {
            // Caller may force tier 3 (chrome) directly, e.g. for anti-bot pages
            // known to defeat lightpanda; otherwise tier 2 first, escalating to
            // tier 3 only on an actual fetch failure (Http) — Guard/TooLarge
            // outcomes are tier-independent, so retrying under chrome can't help.
            let want_chrome = args
                .get("chrome")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let cfg = browser_cfg_from_env();
            return match browser::fetch_rendered(&url, &deny, &cfg, want_chrome, timeout).await {
                Ok(result) => Response::success(
                    id,
                    serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
                ),
                Err(FetchError::Http(_)) if !want_chrome => {
                    match browser::fetch_rendered(&url, &deny, &cfg, true, timeout).await {
                        Ok(result) => Response::success(
                            id,
                            serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
                        ),
                        Err(e) => fetch_error_response(id, "fetch (tier 3)", e),
                    }
                }
                Err(e) => fetch_error_response(id, "fetch (rendered)", e),
            };
        }
        match fetcher::fetch_tier1(&url, &deny, timeout).await {
            Ok(result) => Response::success(
                id,
                serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
            ),
            Err(e) => fetch_error_response(id, "fetch", e),
        }
    }

    async fn handle_search(
        &mut self,
        id: Option<serde_json::Value>,
        args: serde_json::Value,
    ) -> Response {
        let query = match args.get("query").and_then(|v| v.as_str()) {
            Some(q) => q.to_string(),
            None => return Response::error(id, -32602, "search requires 'query'".to_string()),
        };
        let limit = args
            .get("limit")
            .and_then(|v| v.as_u64())
            .unwrap_or(5)
            .clamp(1, 20) as usize;
        let cfg = browser_cfg_from_env();
        let timeout = timeout_from_env();
        match browser::search(&query, limit, &cfg, timeout).await {
            Ok(hits) => Response::success(
                id,
                serde_json::to_value(hits).unwrap_or(serde_json::Value::Null),
            ),
            Err(e) => fetch_error_response(id, "search", e),
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
