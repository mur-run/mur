// mur-research-gateway/src/bin/stub_gateway.rs
//!
//! Deterministic test fixture for Task 10 of the MUR-native deep-research
//! plan: a second binary that speaks the SAME stdio MCP handshake as the
//! real `mur-research-gateway` (`initialize` / `tools/list` / `tools/call`
//! for `search` / `fetch`) but returns a FIXED corpus — no network, no
//! agent-browser, no SSRF guard. This is what a later operator E2E
//! (Task 11) points provisioned workers at to exercise decompose → research
//! → verify → synthesize → marker-convergence end to end against a known,
//! reproducible fact + citation, without depending on live web access.
//!
//! Deliberately self-contained (does not depend on the real gateway's
//! `server`/`fetcher`/`browser`/`net_guard` modules, none of which are
//! exposed from a lib target): the whole point of a stub is that it MUST
//! NOT be able to reach the network even by accident.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{BufRead, Write};

/// JSON-RPC 2.0 request (what the client sends us). Mirrors
/// `mur-research-gateway`'s `jsonrpc::Request` wire shape.
#[derive(Debug, Deserialize)]
struct Request {
    #[allow(dead_code)]
    #[serde(default)]
    jsonrpc: String,
    id: Option<Value>,
    #[serde(default)]
    method: String,
    #[serde(default)]
    params: Option<Value>,
}

/// JSON-RPC 2.0 response.
#[derive(Debug, Serialize, PartialEq)]
struct Response {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize, PartialEq)]
struct JsonRpcError {
    code: i32,
    message: String,
}

impl Response {
    fn success(id: Option<Value>, result: Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Option<Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(JsonRpcError { code, message }),
        }
    }
}

/// The fixed, citable fact every `fetch` in this stub corpus resolves to.
/// Task 11's E2E can assert a synthesized report cites this exact URL.
const STUB_FACT_URL: &str = "https://stub.mur.test/deep-research-fixture";
const STUB_FACT_TEXT: &str = "Fixed fixture fact: the MUR deep-research gateway routes every worker's outbound web access through a single audited proxy process (source: stub.mur.test/deep-research-fixture, corpus revision 1).";

const STUB_HIT_2_URL: &str = "https://stub.mur.test/deep-research-fixture-2";

/// Fixed `tools/list` schema — same shape as the real gateway's `search`/`fetch`.
fn tools_list() -> Value {
    serde_json::json!([
        {
            "name": "search",
            "description": "Web search. Returns [{title,url,snippet}]. Read-only.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {"type": "string"},
                    "limit": {"type": "number"}
                },
                "required": ["query"]
            }
        },
        {
            "name": "fetch",
            "description": "Fetch one URL's readable text. Read-only GET. SSRF-guarded.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "url": {"type": "string"},
                    "render": {"type": "boolean"}
                },
                "required": ["url"]
            }
        }
    ])
}

/// Fixed 2-hit corpus returned by `search`, regardless of the query.
fn search_hits() -> Value {
    serde_json::json!([
        {
            "title": "MUR Deep Research Fixture — Primary Source",
            "url": STUB_FACT_URL,
            "snippet": "Fixed fixture fact: the MUR deep-research gateway routes every worker's outbound web access through a single audited proxy process."
        },
        {
            "title": "MUR Deep Research Fixture — Secondary Source",
            "url": STUB_HIT_2_URL,
            "snippet": "Corroborating fixture source for the deterministic stub corpus."
        }
    ])
}

/// Fixed fetch body for any URL: this stub never makes a real request, so it
/// always returns the same known fact + citable URL regardless of what was
/// asked for. Good enough to prove the wiring; NOT a substitute for testing
/// against real, varied content (that's Task 11's job).
fn fetch_result(url: &str) -> Value {
    serde_json::json!({
        "url": url,
        "status": 200,
        "title": "MUR Deep Research Fixture",
        "text": STUB_FACT_TEXT,
        "tier": 1
    })
}

/// Handle one JSON-RPC request against the fixed corpus. Pure — no I/O — so
/// it is directly unit-testable without spawning the process.
fn handle(request: &Request) -> Response {
    match request.method.as_str() {
        "initialize" => Response::success(
            request.id.clone(),
            serde_json::json!({
                "protocolVersion": "2025-11-25",
                "capabilities": {"tools": {}},
                "serverInfo": {
                    "name": "mur-research-gateway-stub",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        ),
        "tools/list" => Response::success(
            request.id.clone(),
            serde_json::json!({ "tools": tools_list() }),
        ),
        "tools/call" => handle_tool_call(request.id.clone(), request.params.clone()),
        "notifications/initialized" => Response {
            jsonrpc: "2.0",
            id: None,
            result: None,
            error: None,
        },
        "" => Response::error(request.id.clone(), -32700, "Parse error".to_string()),
        other => Response::error(
            request.id.clone(),
            -32601,
            format!("Method not found: {other}"),
        ),
    }
}

fn handle_tool_call(id: Option<Value>, params: Option<Value>) -> Response {
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
        "search" => {
            if args.get("query").and_then(|v| v.as_str()).is_none() {
                return Response::error(id, -32602, "search requires 'query'".to_string());
            }
            Response::success(id, search_hits())
        }
        "fetch" => match args.get("url").and_then(|v| v.as_str()) {
            Some(url) => Response::success(id, fetch_result(url)),
            None => Response::error(id, -32602, "fetch requires 'url'".to_string()),
        },
        other => Response::error(id, -32602, format!("Unknown tool: {other}")),
    }
}

/// Read one JSON-RPC request from stdin. Blocks until a complete line.
/// Returns None if stdin closes.
fn read_request() -> Option<Request> {
    let stdin = std::io::stdin();
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        Ok(0) => None, // EOF
        Ok(_) => {
            let trimmed = line.trim();
            if trimmed.is_empty() {
                return read_request(); // skip blank lines
            }
            match serde_json::from_str::<Request>(trimmed) {
                Ok(req) => Some(req),
                Err(_) => Some(Request {
                    jsonrpc: "2.0".into(),
                    id: None,
                    method: String::new(),
                    params: None,
                }),
            }
        }
        Err(_) => None,
    }
}

fn write_response(resp: &Response) {
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let json = serde_json::to_string(resp).unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"serialize failure"}}"#.to_string()
    });
    writeln!(handle, "{json}").ok();
    handle.flush().ok();
}

fn main() {
    while let Some(request) = read_request() {
        let is_notification = request.id.is_none() && request.method.starts_with("notifications/");
        let response = handle(&request);
        if !is_notification {
            write_response(&response);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, params: Option<Value>) -> Request {
        Request {
            jsonrpc: "2.0".into(),
            id: Some(serde_json::json!(1)),
            method: method.to_string(),
            params,
        }
    }

    #[test]
    fn initialize_advertises_stub_server_info() {
        let resp = handle(&req("initialize", None));
        assert!(resp.error.is_none());
        let name = resp.result.unwrap()["serverInfo"]["name"].clone();
        assert_eq!(name, serde_json::json!("mur-research-gateway-stub"));
    }

    #[test]
    fn tools_list_declares_search_and_fetch() {
        let resp = handle(&req("tools/list", None));
        let tools = resp.result.unwrap()["tools"].clone();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["search", "fetch"]);
    }

    #[test]
    fn search_returns_fixed_two_hit_corpus() {
        let resp = handle(&req(
            "tools/call",
            Some(serde_json::json!({"name": "search", "arguments": {"query": "anything"}})),
        ));
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let hits = resp.result.unwrap();
        let arr = hits.as_array().unwrap();
        assert_eq!(arr.len(), 2, "corpus must have exactly 2 fixed hits");
        assert_eq!(arr[0]["url"], serde_json::json!(STUB_FACT_URL));
        assert_eq!(arr[1]["url"], serde_json::json!(STUB_HIT_2_URL));
    }

    #[test]
    fn search_missing_query_errors() {
        let resp = handle(&req(
            "tools/call",
            Some(serde_json::json!({"name": "search", "arguments": {}})),
        ));
        assert!(resp.error.is_some());
    }

    #[test]
    fn fetch_returns_known_fact_and_echoes_requested_url() {
        let resp = handle(&req(
            "tools/call",
            Some(serde_json::json!({"name": "fetch", "arguments": {"url": STUB_FACT_URL}})),
        ));
        assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
        let result = resp.result.unwrap();
        assert_eq!(result["url"], serde_json::json!(STUB_FACT_URL));
        assert_eq!(result["status"], serde_json::json!(200));
        let text = result["text"].as_str().unwrap();
        assert!(
            text.contains("audited proxy process"),
            "fetch text must contain the known fixture fact, got: {text}"
        );
    }

    #[test]
    fn fetch_missing_url_errors() {
        let resp = handle(&req(
            "tools/call",
            Some(serde_json::json!({"name": "fetch", "arguments": {}})),
        ));
        assert!(resp.error.is_some());
    }

    #[test]
    fn fetch_never_touches_the_network_even_for_a_localhost_url() {
        // The real gateway's SSRF guard would reject this; the stub has NO
        // guard because it also has no network access path — it just
        // returns the same fixed fact regardless of URL. This test pins
        // that behavior so nobody "fixes" the stub into making real requests.
        let resp = handle(&req(
            "tools/call",
            Some(serde_json::json!({"name": "fetch", "arguments": {"url": "http://127.0.0.1:1/"}})),
        ));
        assert!(resp.error.is_none());
        assert!(
            resp.result.unwrap()["text"]
                .as_str()
                .unwrap()
                .contains("audited proxy process")
        );
    }

    #[test]
    fn unknown_tool_errors() {
        let resp = handle(&req(
            "tools/call",
            Some(serde_json::json!({"name": "nope", "arguments": {}})),
        ));
        assert!(resp.error.is_some());
    }

    #[test]
    fn unknown_method_errors() {
        let resp = handle(&req("bogus/method", None));
        assert!(resp.error.is_some());
    }

    #[test]
    fn notifications_initialized_produces_empty_response_shape() {
        let mut r = req("notifications/initialized", None);
        r.id = None;
        let resp = handle(&r);
        assert!(resp.result.is_none() && resp.error.is_none());
    }
}
