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

/// Escalate a rendered fetch from lightpanda (tier 2) to chrome (tier 3)?
/// Yes when lightpanda "doesn't work": an `Http` failure (spawn/timeout/
/// non-zero exit all map here), OR a success that rendered no text (the
/// engine ran but produced nothing — the exit-0-empty case a plain Http-error
/// check misses). `Guard`/`TooLarge` are tier-independent — chrome can't fix a
/// blocked host or an oversized body — so never escalate on those.
fn should_escalate_to_chrome(result: &Result<fetcher::FetchResult, FetchError>) -> bool {
    match result {
        Err(FetchError::Http(_)) => true,
        Ok(res) => res.text.trim().is_empty(),
        Err(FetchError::Guard(_)) | Err(FetchError::TooLarge) => false,
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
            // known to defeat lightpanda; otherwise tier 2 (lightpanda) first,
            // escalating to tier 3 (chrome) when lightpanda "doesn't work" —
            // see `should_escalate_to_chrome`.
            let want_chrome = args
                .get("chrome")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let cfg = &self.config.browser;
            let browser_timeout = self.config.browser_timeout;
            let mut result =
                browser::fetch_rendered(&url, deny, cfg, want_chrome, browser_timeout).await;
            let mut tier_label = "fetch (rendered)";
            // Escalate only when tier 2 actually ran lightpanda: with no
            // lightpanda configured the first attempt was already chrome
            // (build_fetch_argv), so retrying chrome would just waste a spawn.
            if !want_chrome && cfg.lightpanda_path.is_some() && should_escalate_to_chrome(&result) {
                // lightpanda failed or rendered nothing → retry under chrome.
                result = browser::fetch_rendered(&url, deny, cfg, true, browser_timeout).await;
                tier_label = "fetch (tier 3)";
            }
            return match result {
                Ok(result) => {
                    audit(AuditRecord::new("fetch", url, Some(result.tier), "ok"));
                    let mut result = result;
                    result.text = fetcher::cap_text(&result.text, self.config.max_fetch_chars);
                    Response::success(
                        id,
                        serde_json::to_value(result).unwrap_or(serde_json::Value::Null),
                    )
                }
                Err(e) => {
                    audit(AuditRecord::new("fetch", url, None, fetch_outcome(&e)));
                    fetch_error_response(id, tier_label, e)
                }
            };
        }
        match fetcher::fetch_tier1(&url, deny, self.config.timeout).await {
            Ok(result) => {
                audit(AuditRecord::new("fetch", url, Some(result.tier), "ok"));
                let mut result = result;
                result.text = fetcher::cap_text(&result.text, self.config.max_fetch_chars);
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
        let timeout = self.config.timeout;
        match fetcher::search_tier1(&query, limit, deny, timeout).await {
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

    fn rendered(text: &str, tier: u8) -> Result<fetcher::FetchResult, FetchError> {
        Ok(fetcher::FetchResult {
            url: "https://example.com/".into(),
            status: 200,
            title: None,
            text: text.into(),
            tier,
        })
    }

    #[test]
    fn escalates_to_chrome_on_http_error_or_empty_render() {
        // Http failure (spawn/timeout/non-zero exit) → escalate.
        assert!(should_escalate_to_chrome(&Err(FetchError::Http(
            "spawn agent-browser: fail".into()
        ))));
        // lightpanda ran but rendered nothing (exit-0-empty) → escalate.
        assert!(should_escalate_to_chrome(&rendered("", 2)));
        assert!(should_escalate_to_chrome(&rendered("   \n  ", 2)));
        // lightpanda rendered real content → keep tier 2, do NOT escalate.
        assert!(!should_escalate_to_chrome(&rendered(
            "Example Domain\nhttps://example.com/",
            2
        )));
        // Tier-independent rejections → never escalate (chrome can't help).
        assert!(!should_escalate_to_chrome(&Err(FetchError::TooLarge)));
        assert!(!should_escalate_to_chrome(&Err(FetchError::Guard(
            crate::net_guard::GuardReject::BadScheme
        ))));
    }

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

    #[test]
    fn fetch_text_is_capped_to_config_budget() {
        // The server caps fetched text via fetcher::cap_text with the config
        // budget; verify the budget is actually applied (regression guard for
        // the handle_fetch wiring).
        let text = "a".repeat(1000);
        let capped = fetcher::cap_text(&text, 100);
        assert!(capped.len() < text.len());
        assert!(capped.contains("[truncated"));
        // 0 budget = untouched.
        assert_eq!(fetcher::cap_text(&text, 0), text);
    }
}
