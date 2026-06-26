//! OAuth 2.1 helpers for remote MCP connections.
//!
//! - `pkce`  — PKCE (RFC 7636) + discovery-URL builders (RFC 8414 / 9728).
//! - This module — AS metadata discovery (RFC 8414 / 9728) + dynamic client
//!   registration (RFC 7591).

pub mod pkce;

use anyhow::Context as _;
use serde::Deserialize;

// ── AS Metadata ──────────────────────────────────────────────────────────────

/// Authorization Server Metadata (RFC 8414 §2).
///
/// Only the fields required by the remote-MCP OAuth flow are captured here.
#[derive(Debug, Clone, Deserialize)]
pub struct AsMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    /// RFC 7591 dynamic registration endpoint (optional).
    #[serde(default)]
    pub registration_endpoint: Option<String>,
}

/// Parse an AS metadata JSON document (pure, no I/O).
pub fn parse_as_metadata(json: &str) -> anyhow::Result<AsMetadata> {
    serde_json::from_str(json).context("invalid AS metadata JSON")
}

// ── Discovery ─────────────────────────────────────────────────────────────────

/// Discover the Authorization Server metadata for `server_url` (RFC 9728 + 8414).
///
/// Steps:
/// 1. GET `<origin>/.well-known/oauth-protected-resource` (RFC 9728 §5).
/// 2. If it succeeds and contains `authorization_servers[0]`, use that as the
///    AS issuer; otherwise fall back to `<origin>` of `server_url`.
/// 3. GET `<issuer>/.well-known/oauth-authorization-server` and parse it.
pub async fn discover(http: &reqwest::Client, server_url: &str) -> anyhow::Result<AsMetadata> {
    // Step 1 — try protected-resource metadata.
    let pr_url = pkce::protected_resource_url(server_url);
    let issuer: String = match http.get(&pr_url).send().await {
        Ok(resp) if resp.status().is_success() => {
            #[derive(Deserialize)]
            struct PrMeta {
                #[serde(default)]
                authorization_servers: Vec<String>,
            }
            let pr: PrMeta = resp
                .json()
                .await
                .context("invalid protected-resource metadata JSON")?;
            if let Some(first) = pr.authorization_servers.into_iter().next() {
                first
            } else {
                // Document present but no authorization_servers list — fall back.
                pkce::origin_of(server_url).to_string()
            }
        }
        // Any error (4xx, 5xx, network) → fall back to origin.
        _ => pkce::origin_of(server_url).to_string(),
    };

    // Step 2 — fetch AS metadata from the resolved issuer.
    let as_url = pkce::as_metadata_url(&issuer);
    let body = http
        .get(&as_url)
        .send()
        .await
        .context("AS metadata request failed")?
        .text()
        .await
        .context("AS metadata response unreadable")?;

    parse_as_metadata(&body)
}

// ── Dynamic Client Registration (RFC 7591) ────────────────────────────────────

/// Register a public client at `registration_endpoint` and return the issued
/// `client_id`.
///
/// Sends a minimal RFC 7591 registration request:
/// - `token_endpoint_auth_method: "none"` (public client)
/// - `grant_types: ["authorization_code", "refresh_token"]`
/// - `response_types: ["code"]`
pub async fn register_client(
    http: &reqwest::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
) -> anyhow::Result<String> {
    #[derive(Deserialize)]
    struct Reg {
        client_id: String,
    }

    let reg: Reg = http
        .post(registration_endpoint)
        .json(&serde_json::json!({
            "client_name": "MUR",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none"
        }))
        .send()
        .await
        .context("dynamic registration request failed")?
        .json()
        .await
        .context("dynamic registration response missing client_id")?;

    Ok(reg.client_id)
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_as_metadata() {
        let j = r#"{"authorization_endpoint":"https://a/x","token_endpoint":"https://a/t","registration_endpoint":"https://a/r"}"#;
        let m = parse_as_metadata(j).unwrap();
        assert_eq!(m.authorization_endpoint, "https://a/x");
        assert_eq!(m.token_endpoint, "https://a/t");
        assert_eq!(m.registration_endpoint.as_deref(), Some("https://a/r"));
    }

    #[test]
    fn parses_as_metadata_no_registration() {
        let j = r#"{"authorization_endpoint":"https://a/x","token_endpoint":"https://a/t"}"#;
        let m = parse_as_metadata(j).unwrap();
        assert_eq!(m.token_endpoint, "https://a/t");
        assert!(m.registration_endpoint.is_none());
    }
}
