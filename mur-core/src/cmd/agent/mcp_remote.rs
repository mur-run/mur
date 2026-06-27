//! Remote MCP URL validation and one-shot probe (`initialize` + `tools/list`).
//!
//! Network-free helpers (`validate_remote_url`, `parse_resource_metadata`,
//! `parse_tools_list`) are fully unit-testable. `probe_remote` makes real HTTP
//! calls and is covered by manual/live integration tests.

use anyhow::{Result, bail};
use reqwest::Url;
use serde::Serialize;

// ─── Task 1: URL validation ────────────────────────────────────────────────

/// Validate and normalize a remote MCP URL.
///
/// Requires `https`, except `http` is allowed on `localhost`/`127.0.0.1`/`::1`
/// for local development. Strips a single trailing `/` from bare-host URLs.
pub fn validate_remote_url(raw: &str) -> Result<String> {
    let trimmed = raw.trim();
    let url = Url::parse(trimmed).map_err(|e| anyhow::anyhow!("invalid URL: {e}"))?;
    let host = url.host_str().unwrap_or("");
    let is_local = matches!(host, "localhost" | "127.0.0.1" | "::1");
    match url.scheme() {
        "https" => {}
        "http" if is_local => {}
        "http" => bail!("remote MCP URLs must use https (got http://{host})"),
        other => bail!("unsupported URL scheme: {other}"),
    }
    Ok(url.as_str().trim_end_matches('/').to_string())
}

// ─── Task 2: parse helpers — network-free, fully unit-testable ────────────

/// A single tool entry returned by `tools/list`.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProbeTool {
    pub name: String,
    pub description: String,
    pub input_schema: serde_json::Value,
}

/// Pull the `resource_metadata="..."` URL out of a `WWW-Authenticate` header
/// (RFC 9728). Returns `None` if absent or empty.
pub fn parse_resource_metadata(www_authenticate: &str) -> Option<String> {
    let key = "resource_metadata=";
    let start = www_authenticate.find(key)? + key.len();
    let rest = &www_authenticate[start..];
    let rest = rest.trim_start_matches('"');
    let end = rest.find('"').unwrap_or(rest.len());
    let val = rest[..end].trim();
    (!val.is_empty()).then(|| val.to_string())
}

/// Extract tools from a JSON-RPC `tools/list` response body.
///
/// Tolerant: missing fields default to empty/null. Accepts both `inputSchema`
/// (spec) and `input_schema` (snake_case variant).
pub fn parse_tools_list(body: &serde_json::Value) -> Vec<ProbeTool> {
    let Some(tools) = body
        .get("result")
        .and_then(|r| r.get("tools"))
        .and_then(|t| t.as_array())
    else {
        return Vec::new();
    };
    tools
        .iter()
        .map(|t| ProbeTool {
            name: t
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            description: t
                .get("description")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string(),
            input_schema: t
                .get("inputSchema")
                .or_else(|| t.get("input_schema"))
                .cloned()
                .unwrap_or(serde_json::Value::Null),
        })
        .collect()
}

// ─── Task 3: async probe — initialize + tools/list + 401 detection ────────

/// MCP protocol revision we advertise in `initialize`.
///
/// Bump when MUR adopts a newer spec revision. (Constant — not a hardcoded
/// literal scattered across the codebase.)
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Transport variant detected during probe.
#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum ProbeTransport {
    StreamableHttp,
    LegacySse,
    Unknown,
}

/// Outcome of a one-shot remote MCP probe.
#[derive(Debug, Clone, Serialize)]
pub struct ProbeOutcome {
    pub transport: ProbeTransport,
    pub needs_auth: bool,
    pub resource_metadata: Option<String>,
    pub tools: Vec<ProbeTool>,
}

/// Build the JSON-RPC `initialize` request body.
pub fn initialize_request_body() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": MCP_PROTOCOL_VERSION,
            "clientInfo": {
                "name": "mur-hub",
                "version": env!("CARGO_PKG_VERSION")
            },
            "capabilities": {}
        }
    })
}

/// Probe a remote MCP server. POSTs `initialize`; on `401` reports
/// `needs_auth` + RFC-9728 metadata URL; on success reports Streamable-HTTP
/// and fetches `tools/list`; on 400/404/405 reports deprecated SSE transport.
pub async fn probe_remote(url: &str, bearer: Option<&str>) -> Result<ProbeOutcome> {
    let url = validate_remote_url(url)?;
    let http = reqwest::Client::new();

    let mut req = http
        .post(&url)
        .header("Accept", "application/json, text/event-stream")
        .header("Content-Type", "application/json")
        .json(&initialize_request_body());
    if let Some(tok) = bearer {
        req = req.bearer_auth(tok);
    }
    let resp = req
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("connect failed: {e}"))?;
    let status = resp.status();

    if status.as_u16() == 401 {
        let wa = resp
            .headers()
            .get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("");
        return Ok(ProbeOutcome {
            transport: ProbeTransport::StreamableHttp,
            needs_auth: true,
            resource_metadata: parse_resource_metadata(wa),
            tools: Vec::new(),
        });
    }

    if matches!(status.as_u16(), 400 | 404 | 405) {
        return Ok(ProbeOutcome {
            transport: ProbeTransport::LegacySse,
            needs_auth: false,
            resource_metadata: None,
            tools: Vec::new(),
        });
    }

    if !status.is_success() {
        bail!("server returned {status}");
    }

    // tools/list — best-effort; server may require a session we don't keep.
    // An empty list still lets the user add the server, just without preview.
    let mut tools = Vec::new();
    let tl_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "tools/list",
        "params": {}
    });
    let mut tl_req = http
        .post(&url)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .json(&tl_body);
    if let Some(tok) = bearer {
        tl_req = tl_req.bearer_auth(tok);
    }
    if let Ok(r) = tl_req.send().await
        && r.status().is_success()
        && let Ok(body) = r.json::<serde_json::Value>().await
    {
        tools = parse_tools_list(&body);
    }

    Ok(ProbeOutcome {
        transport: ProbeTransport::StreamableHttp,
        needs_auth: false,
        resource_metadata: None,
        tools,
    })
}

// ─── Hub-facing helpers ────────────────────────────────────────────────────

/// Store a bearer token for a remote MCP server in the OS keychain and return
/// the `SecretRef` that resolves it.
///
/// Key derivation exactly matches `cmd_secret_set(agent, "mcp/<server_id>",
/// value)`: service = `"mur-agent"`, account = `"<agent>/mcp/<server_id>"`.
/// The returned `SecretRef::Keychain` resolves to the same item.
pub async fn store_remote_mcp_bearer(
    agent: &str,
    server_id: &str,
    token: &str,
) -> Result<mur_common::secret::SecretRef> {
    use super::secret::SECRET_SERVICE;
    let account = format!("{agent}/mcp/{server_id}");
    mur_common::secret::keychain_set(SECRET_SERVICE, &account, token)
        .await
        .map_err(|e| anyhow::anyhow!("keychain write failed: {e}"))?;
    Ok(mur_common::secret::SecretRef::Keychain {
        service: SECRET_SERVICE.to_string(),
        account,
    })
}

/// Compute the tool-description hash from a probe result's tool list.
///
/// Maps `ProbeTool` → `McpToolDescription` and delegates to
/// `cmd::agent_mcp_pin::compute_description_hash`. Returns `None` when
/// `tools` is empty (nothing to pin).
pub fn compute_probe_description_hash(tools: &[ProbeTool]) -> Option<String> {
    if tools.is_empty() {
        return None;
    }
    let descs: Vec<crate::cmd::agent_mcp_pin::McpToolDescription> = tools
        .iter()
        .map(|t| crate::cmd::agent_mcp_pin::McpToolDescription {
            name: t.name.clone(),
            description: t.description.clone(),
            input_schema: t.input_schema.clone(),
        })
        .collect();
    Some(crate::cmd::agent_mcp_pin::compute_description_hash(&descs))
}

// ─── Tests ─────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── Task 1 tests ──────────────────────────────────────────────────────

    #[test]
    fn accepts_https() {
        let result = validate_remote_url("https://mcp.example.com/mcp");
        assert!(result.is_ok(), "expected Ok, got {result:?}");
        assert_eq!(result.unwrap(), "https://mcp.example.com/mcp");
    }

    #[test]
    fn rejects_plain_http_remote() {
        let result = validate_remote_url("http://mcp.example.com/mcp");
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(msg.contains("https"), "error should mention https: {msg}");
    }

    #[test]
    fn accepts_http_localhost() {
        assert!(validate_remote_url("http://localhost:8080/mcp").is_ok());
        assert!(validate_remote_url("http://127.0.0.1:3000/mcp").is_ok());
    }

    #[test]
    fn rejects_unsupported_scheme() {
        let result = validate_remote_url("ftp://mcp.example.com/mcp");
        assert!(result.is_err());
    }

    #[test]
    fn rejects_non_url() {
        assert!(validate_remote_url("not a url").is_err());
    }

    #[test]
    fn strips_trailing_slash_from_bare_host() {
        let result = validate_remote_url("https://mcp.example.com/").unwrap();
        assert_eq!(result, "https://mcp.example.com");
    }

    #[test]
    fn preserves_path() {
        let result = validate_remote_url("https://mcp.example.com/v1/mcp").unwrap();
        assert_eq!(result, "https://mcp.example.com/v1/mcp");
    }

    // ── Task 2 tests ──────────────────────────────────────────────────────

    #[test]
    fn parses_resource_metadata_from_header() {
        let header = r#"Bearer realm="mcp", resource_metadata="https://auth.example.com/.well-known/oauth-protected-resource""#;
        let result = parse_resource_metadata(header);
        assert_eq!(
            result.as_deref(),
            Some("https://auth.example.com/.well-known/oauth-protected-resource")
        );
    }

    #[test]
    fn returns_none_when_no_metadata() {
        assert!(parse_resource_metadata("Bearer").is_none());
        assert!(parse_resource_metadata("").is_none());
    }

    #[test]
    fn parses_tools_list_response() {
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "tools": [
                    {
                        "name": "search",
                        "description": "Search docs",
                        "inputSchema": { "type": "object" }
                    }
                ]
            }
        });
        let tools = parse_tools_list(&body);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "search");
        assert_eq!(tools[0].description, "Search docs");
    }

    #[test]
    fn parse_tools_list_tolerates_missing_fields() {
        let body = serde_json::json!({ "result": { "tools": [{}] } });
        let tools = parse_tools_list(&body);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "");
        assert_eq!(tools[0].description, "");
        assert_eq!(tools[0].input_schema, serde_json::Value::Null);
    }

    #[test]
    fn parse_tools_list_empty_on_missing_result() {
        let body = serde_json::json!({});
        assert!(parse_tools_list(&body).is_empty());
    }

    #[test]
    fn parse_tools_list_uses_snake_case_input_schema_fallback() {
        let schema = serde_json::json!({ "type": "object", "properties": {} });
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 2,
            "result": {
                "tools": [
                    {
                        "name": "do_thing",
                        "description": "Does a thing",
                        "input_schema": schema
                    }
                ]
            }
        });
        let tools = parse_tools_list(&body);
        assert_eq!(tools.len(), 1);
        assert_ne!(
            tools[0].input_schema,
            serde_json::Value::Null,
            "input_schema fallback should produce a non-null value"
        );
        assert_eq!(tools[0].input_schema["type"], "object");
    }

    // ── Task 3 tests ──────────────────────────────────────────────────────

    #[test]
    fn initialize_body_has_protocol_and_client() {
        let b = initialize_request_body();
        assert_eq!(b["method"], "initialize");
        assert_eq!(b["params"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(b["params"]["clientInfo"]["name"], "mur-hub");
    }
}
