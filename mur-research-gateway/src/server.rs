// mur-research-gateway/src/server.rs
use crate::audit::{AuditRecord, audit};
use crate::browser::{self, BrowserCfg};
use crate::config::{self, GatewayConfig};
use crate::fetcher::{self, FetchError};
use crate::jsonrpc::{Request, Response};
use crate::tools;

/// `denied` for the SSRF guard rejection, `error` for every other `FetchError`
/// variant (Http/TooLarge) — the audit's outcome taxonomy per spec §7.2/§7.4.
fn fetch_outcome(err: &FetchError) -> &'static str {
    match err {
        FetchError::Guard(_) => "denied",
        FetchError::Http(_) | FetchError::TooLarge => "error",
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

pub struct McpServer {
    config: GatewayConfig,
}

impl McpServer {
    /// Loads `GatewayConfig` ONCE (from `~/.mur/config.yaml`'s
    /// `research_gateway:` block + env overrides) and stores it for the
    /// lifetime of the server — see `config::load`.
    pub fn new() -> Self {
        let mur_home = config::mur_home_dir();
        McpServer {
            config: config::load(&mur_home),
        }
    }

    /// Exposes the loaded browser config for `main`'s startup preflight, so
    /// preflight reads the SAME config the server will use — never a second,
    /// independently-loaded copy.
    pub(crate) fn browser_cfg(&self) -> &BrowserCfg {
        &self.config.browser
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
        let deny = &self.config.deny_hosts;
        if render {
            // Caller may force tier 3 (chrome) directly, e.g. for anti-bot pages
            // known to defeat lightpanda; otherwise tier 2 first, escalating to
            // tier 3 only on an actual fetch failure (Http) — Guard/TooLarge
            // outcomes are tier-independent, so retrying under chrome can't help.
            let want_chrome = args
                .get("chrome")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let cfg = &self.config.browser;
            let browser_timeout = self.config.browser_timeout;
            return match browser::fetch_rendered(&url, deny, cfg, want_chrome, browser_timeout)
                .await
            {
                Ok(result) => {
                    audit(AuditRecord::new("fetch", url, Some(result.tier), "ok"));
                    Response::success(
                        id,
                        serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
                    )
                }
                Err(FetchError::Http(_)) if !want_chrome => {
                    match browser::fetch_rendered(&url, deny, cfg, true, browser_timeout).await {
                        Ok(result) => {
                            audit(AuditRecord::new("fetch", url, Some(result.tier), "ok"));
                            Response::success(
                                id,
                                serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
                            )
                        }
                        Err(e) => {
                            audit(AuditRecord::new("fetch", url, None, fetch_outcome(&e)));
                            fetch_error_response(id, "fetch (tier 3)", e)
                        }
                    }
                }
                Err(e) => {
                    audit(AuditRecord::new("fetch", url, None, fetch_outcome(&e)));
                    fetch_error_response(id, "fetch (rendered)", e)
                }
            };
        }
        match fetcher::fetch_tier1(&url, deny, self.config.timeout).await {
            Ok(result) => {
                audit(AuditRecord::new("fetch", url, Some(result.tier), "ok"));
                Response::success(
                    id,
                    serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
                )
            }
            Err(e) => {
                audit(AuditRecord::new("fetch", url, None, fetch_outcome(&e)));
                fetch_error_response(id, "fetch", e)
            }
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
            .map(|v| v as usize)
            .unwrap_or(self.config.search_limit)
            .clamp(config::MIN_SEARCH_LIMIT, config::MAX_SEARCH_LIMIT);
        let deny = &self.config.deny_hosts;
        let cfg = &self.config.browser;
        let timeout = self.config.browser_timeout;
        match browser::search(&query, limit, deny, cfg, timeout).await {
            Ok(hits) => {
                audit(AuditRecord::new("search", query, None, "ok"));
                Response::success(
                    id,
                    serde_json::to_value(hits).unwrap_or(serde_json::Value::Null),
                )
            }
            Err(e) => {
                audit(AuditRecord::new("search", query, None, fetch_outcome(&e)));
                fetch_error_response(id, "search", e)
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
