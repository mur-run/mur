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
        .error_for_status()
        .context("AS metadata endpoint returned an error status")?
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

// ── Authorization-code + PKCE flow ─────────────────────────────────────────────

/// Token endpoint response (RFC 6749 §5.1). Only the fields MUR persists.
#[derive(Debug, Clone, Deserialize)]
pub struct Tokens {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    /// Seconds until the access token expires (0 = server didn't say).
    #[serde(default)]
    pub expires_in: u64,
}

/// Parse a token-endpoint JSON response (pure, no I/O).
pub fn parse_tokens(json: &str) -> anyhow::Result<Tokens> {
    serde_json::from_str(json).context("invalid token-endpoint JSON")
}

/// Percent-encode every byte of `s` EXCEPT the RFC 3986 unreserved set
/// (`A-Z a-z 0-9 - . _ ~`). Used for OAuth authorization-URL query values so
/// MUR carries no `urlencoding`/`url` dependency (which would churn Cargo.lock
/// and break the workspace-excluded GUI crate's CI).
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s.as_bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push('%');
            out.push(
                char::from_digit((b >> 4) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
            out.push(
                char::from_digit((b & 0xf) as u32, 16)
                    .unwrap()
                    .to_ascii_uppercase(),
            );
        }
    }
    out
}

/// Decode a percent-encoded string (best-effort; bad escapes pass through).
fn pct_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(h), Some(l)) = (hi, lo) {
                out.push((h * 16 + l) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Run the OAuth 2.1 authorization-code + PKCE flow against `meta`.
///
/// Seeds a fresh 32-byte PKCE verifier and a separate 32-byte CSRF `state`
/// token, builds the authorization URL (with the `resource` indicator set to
/// `server_url`, RFC 8707), opens the system browser best-effort (and always
/// prints the URL), waits for the localhost `?code=` callback (verifying
/// `state`), then exchanges the code for tokens.
pub async fn run_authorization_flow(
    http: &reqwest::Client,
    meta: &AsMetadata,
    client_id: &str,
    server_url: &str,
    redirect_port: u16,
) -> anyhow::Result<Tokens> {
    use rand::RngCore as _;

    // 1. PKCE verifier + challenge (Task 7 helpers).
    let mut seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut seed);
    let verifier = pkce::code_verifier(&seed);
    let challenge = pkce::code_challenge(&verifier);

    // 1b. CSRF state — fresh 32-byte seed, same encoding as the verifier.
    let mut state_seed = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut state_seed);
    let state = pkce::code_verifier(&state_seed);

    let redirect_uri = format!("http://127.0.0.1:{redirect_port}/callback");

    // 2. Build the authorization URL.
    let sep = if meta.authorization_endpoint.contains('?') {
        '&'
    } else {
        '?'
    };
    let auth_url = format!(
        "{}{}response_type=code&client_id={}&redirect_uri={}&code_challenge={}&code_challenge_method=S256&resource={}&scope={}&state={}",
        meta.authorization_endpoint,
        sep,
        pct(client_id),
        pct(&redirect_uri),
        pct(&challenge),
        pct(server_url),
        pct("openid offline_access"),
        pct(&state),
    );

    // 3. Best-effort browser launch; always print so the user can paste.
    println!("Open this URL to authorize MUR:\n  {auth_url}");
    // Fire-and-forget, but `std::process::Child` has no background reaper
    // (unlike `tokio::process::Child`): an un-waited launcher process
    // becomes a permanent zombie once it exits. Reap it on a detached
    // thread instead of leaking (dogfood issue 11).
    if let Ok(child) = std::process::Command::new(if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    })
    .arg(&auth_url)
    .spawn()
    {
        std::thread::spawn(move || {
            let mut child = child;
            let _ = child.wait();
        });
    }

    // 4. Wait for the localhost redirect (state verified inside), then exchange.
    let code = wait_for_code(redirect_port, &state).await?;
    exchange_code(
        http,
        &meta.token_endpoint,
        client_id,
        &code,
        &verifier,
        &redirect_uri,
        server_url,
    )
    .await
}

/// Bind `127.0.0.1:port`, loop until a request carrying `?code=` or `?error=`
/// arrives (ignoring stray probes up to a bounded limit), verify `state`
/// matches `expected_state`, and return the authorization code.
async fn wait_for_code(port: u16, expected_state: &str) -> anyhow::Result<String> {
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    const MAX_ATTEMPTS: usize = 20;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("bind OAuth callback on 127.0.0.1:{port}"))?;

    for _ in 0..MAX_ATTEMPTS {
        let (mut sock, _) = listener
            .accept()
            .await
            .context("accept OAuth callback connection")?;

        let mut buf = [0u8; 4096];
        let n = sock
            .read(&mut buf)
            .await
            .context("read OAuth callback request")?;
        let req = String::from_utf8_lossy(&buf[..n]);

        // First request line: `GET /callback?code=XYZ&state=... HTTP/1.1`
        let target = req
            .lines()
            .next()
            .and_then(|l| l.split_whitespace().nth(1))
            .unwrap_or("");

        // Fix 2: surface authorization-server errors before checking for code.
        if target.contains("error=") {
            let error_val = target
                .split_once("error=")
                .and_then(|(_, q)| q.split('&').next())
                .map(pct_decode)
                .unwrap_or_default();
            let desc = target
                .split_once("error_description=")
                .and_then(|(_, q)| q.split('&').next())
                .map(pct_decode);
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
                      <h2>MUR: authorization failed. You can close this tab.</h2>",
                )
                .await;
            let _ = sock.flush().await;
            if let Some(d) = desc {
                anyhow::bail!("authorization server returned an error: {error_val} — {d}")
            } else {
                anyhow::bail!("authorization server returned an error: {error_val}")
            }
        }

        // Fix 3: if no `code=`, this is a stray probe — respond 200 and loop.
        if !target.contains("code=") {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
                      <h2>MUR: waiting for authorization callback.</h2>",
                )
                .await;
            let _ = sock.flush().await;
            continue;
        }

        // Fix 1: verify CSRF state before accepting the code.
        let returned_state = target
            .split_once("state=")
            .and_then(|(_, q)| q.split('&').next())
            .map(pct_decode)
            .unwrap_or_default();
        if returned_state != expected_state {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 400 Bad Request\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
                      <h2>MUR: state mismatch. You can close this tab.</h2>",
                )
                .await;
            let _ = sock.flush().await;
            anyhow::bail!("OAuth state mismatch — possible CSRF; aborting");
        }

        let code = target
            .split_once("code=")
            .and_then(|(_, q)| q.split('&').next())
            .map(pct_decode)
            .ok_or_else(|| anyhow::anyhow!("authorization redirect had no ?code="))?;

        let _ = sock
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nConnection: close\r\n\r\n\
                  <h2>MUR: authorized. You can close this tab.</h2>",
            )
            .await;
        let _ = sock.flush().await;

        return Ok(code);
    }

    anyhow::bail!(
        "OAuth callback listener exhausted {MAX_ATTEMPTS} connections without receiving a valid authorization code"
    )
}

/// Exchange an authorization `code` for tokens (RFC 6749 §4.1.3 + PKCE §4.5
/// + resource indicator RFC 8707).
async fn exchange_code(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    code: &str,
    verifier: &str,
    redirect_uri: &str,
    resource: &str,
) -> anyhow::Result<Tokens> {
    let body = http
        .post(token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
            ("client_id", client_id),
            ("code_verifier", verifier),
            ("resource", resource),
        ])
        .send()
        .await
        .context("token exchange request failed")?
        .error_for_status()
        .context("token endpoint returned an error status")?
        .text()
        .await
        .context("token exchange response unreadable")?;
    parse_tokens(&body)
}

/// Refresh an access token using a stored refresh token (RFC 6749 §6).
pub async fn refresh(
    http: &reqwest::Client,
    token_endpoint: &str,
    client_id: &str,
    refresh_token: &str,
) -> anyhow::Result<Tokens> {
    let body = http
        .post(token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", client_id),
        ])
        .send()
        .await
        .context("token refresh request failed")?
        .error_for_status()
        .context("token endpoint returned an error status on refresh")?
        .text()
        .await
        .context("token refresh response unreadable")?;
    parse_tokens(&body)
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

    #[test]
    fn parses_token_response() {
        let j =
            r#"{"access_token":"AT","refresh_token":"RT","expires_in":3600,"token_type":"Bearer"}"#;
        let t = parse_tokens(j).unwrap();
        assert_eq!(t.access_token, "AT");
        assert_eq!(t.refresh_token.as_deref(), Some("RT"));
        assert_eq!(t.expires_in, 3600);
    }

    #[test]
    fn parses_token_response_no_refresh() {
        let j = r#"{"access_token":"AT","token_type":"Bearer"}"#;
        let t = parse_tokens(j).unwrap();
        assert_eq!(t.access_token, "AT");
        assert!(t.refresh_token.is_none());
        assert_eq!(t.expires_in, 0);
    }

    #[test]
    fn pct_encodes_reserved_only() {
        // Unreserved set passes through untouched.
        assert_eq!(pct("aZ09-._~"), "aZ09-._~");
        // Everything else is percent-encoded.
        assert_eq!(pct("a b/c?d=e&f"), "a%20b%2Fc%3Fd%3De%26f");
        assert_eq!(pct("https://x/y"), "https%3A%2F%2Fx%2Fy");
    }
}
