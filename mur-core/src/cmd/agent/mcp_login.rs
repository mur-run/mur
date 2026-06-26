//! `mur agent mcp login <agent> <server>` — run the OAuth 2.1
//! authorization-code + PKCE flow for a remote MCP server and persist the
//! resulting tokens.
//!
//! The remote server entry must already exist on the agent (added via
//! `mur agent mcp add-remote`). This command:
//!   1. discovers the authorization server (RFC 9728 / 8414),
//!   2. dynamically registers a public client (RFC 7591),
//!   3. drives the browser auth-code + PKCE flow with a localhost callback,
//!   4. stores the access (and refresh) token in the OS keychain, and
//!   5. records an `McpAuth::Oauth` entry on the profile referencing them.

use anyhow::{Context as _, Result, bail};
use mur_common::agent::{McpAuth, OauthAuth};
use mur_common::secret::{SecretRef, keychain_set};

use super::{load_profile_for_edit, save_profile};

/// Localhost port the OAuth redirect URI listens on. Override with
/// `MUR_OAUTH_REDIRECT_PORT` for environments where this port is taken.
/// The value is also baked into the `redirect_uri` registered with the AS,
/// so it must match what the browser is redirected back to.
const OAUTH_REDIRECT_PORT: u16 = 49213;

/// Resolve the redirect port (env override → documented default).
fn oauth_redirect_port() -> u16 {
    std::env::var("MUR_OAUTH_REDIRECT_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(OAUTH_REDIRECT_PORT)
}

pub async fn cmd_mcp_login(agent: &str, name: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;

    // Locate the entry and clone the URL (immutable borrow released before the
    // long async flow, so we don't hold a mutable borrow across `.await`).
    let url = {
        let entry = profile
            .mcp_servers
            .iter()
            .find(|m| m.name == name)
            .ok_or_else(|| anyhow::anyhow!("no MCP server '{name}' on '{agent}'"))?;
        entry
            .url
            .clone()
            .ok_or_else(|| anyhow::anyhow!("'{name}' is not a remote (url) MCP server"))?
    };

    let http = reqwest::Client::new();
    let meta = mur_agent_runtime::oauth::discover(&http, &url)
        .await
        .context("discover authorization server")?;

    let port = oauth_redirect_port();
    let redirect_uri = format!("http://127.0.0.1:{port}/callback");

    let client_id = match &meta.registration_endpoint {
        Some(reg) => mur_agent_runtime::oauth::register_client(&http, reg, &redirect_uri)
            .await
            .context("dynamic client registration")?,
        None => bail!(
            "the authorization server for '{name}' advertises no dynamic registration endpoint; \
             pre-registered client_id config is not yet supported"
        ),
    };

    let tokens =
        mur_agent_runtime::oauth::run_authorization_flow(&http, &meta, &client_id, &url, port)
            .await
            .context("authorization-code flow")?;

    // Store tokens in the keychain; reference them from the profile entry.
    let service = format!("mur-mcp-{agent}");
    let access_account = format!("{name}.access");
    keychain_set(&service, &access_account, &tokens.access_token)
        .await
        .context("store access token in keychain")?;

    let refresh_ref = match &tokens.refresh_token {
        Some(rt) => {
            let refresh_account = format!("{name}.refresh");
            keychain_set(&service, &refresh_account, rt)
                .await
                .context("store refresh token in keychain")?;
            Some(SecretRef::Keychain {
                service: service.clone(),
                account: refresh_account,
            })
        }
        None => None,
    };

    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);

    // Re-borrow mutably now that the async work is done.
    let entry = profile
        .mcp_servers
        .iter_mut()
        .find(|m| m.name == name)
        .ok_or_else(|| anyhow::anyhow!("MCP server '{name}' vanished during login"))?;
    entry.auth = Some(McpAuth::Oauth(OauthAuth {
        token_endpoint: meta.token_endpoint,
        client_id,
        access_token: SecretRef::Keychain {
            service,
            account: access_account,
        },
        refresh_token: refresh_ref,
        expires_at: now_epoch.saturating_add(tokens.expires_in),
    }));

    save_profile(&path, &mut profile)?;
    println!("Authorized '{name}' for agent '{agent}'. Tokens stored in the keychain.");
    Ok(())
}
