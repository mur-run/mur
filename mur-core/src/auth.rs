//! Device code authentication flow for mur community features.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

/// Stored authentication tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: u64,
}

/// Response from POST /api/v1/auth/device/code
#[derive(Debug, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    pub interval: u64,
}

/// Response from POST /api/v1/auth/device/token
#[derive(Debug, Deserialize)]
pub struct DeviceTokenResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub token_type: String,
    pub expires_in: u64,
}

/// Error response from the server.
#[derive(Debug, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default)]
    pub code: Option<String>,
}

fn auth_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("~"))
        .join(".mur")
        .join("auth.json")
}

/// Load stored auth tokens, if any.
pub fn load_tokens() -> Option<AuthTokens> {
    let path = auth_path();
    if !path.exists() {
        return None;
    }
    let content = fs::read_to_string(&path).ok()?;
    serde_json::from_str(&content).ok()
}

/// Save auth tokens to ~/.mur/auth.json.
pub fn save_tokens(tokens: &AuthTokens) -> Result<()> {
    let path = auth_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let json = serde_json::to_string_pretty(tokens)?;
    fs::write(&path, json)?;
    Ok(())
}

/// Remove stored auth tokens.
pub fn clear_tokens() -> Result<()> {
    let path = auth_path();
    if path.exists() {
        fs::remove_file(&path)?;
    }
    Ok(())
}

/// Get the server URL from env var or config.
pub fn server_url() -> String {
    if let Ok(url) = std::env::var("MUR_SERVER_URL") {
        return url;
    }
    // Try loading from config
    if let Ok(config) = crate::store::config::load_config() {
        return config.server.url;
    }
    "https://mur-server.fly.dev".to_string()
}

/// Run the device code authentication flow.
/// Returns the tokens on success.
pub async fn device_code_flow(client: &reqwest::Client) -> Result<AuthTokens> {
    let base = server_url();

    // Step 1: Request device code
    let resp = client
        .post(format!("{}/api/v1/auth/device/code", base))
        .send()
        .await
        .context("Failed to connect to mur server")?;

    if !resp.status().is_success() {
        let err: ErrorResponse = resp.json().await.unwrap_or(ErrorResponse {
            error: "unknown error".to_string(),
            code: None,
        });
        anyhow::bail!("Failed to get device code: {}", err.error);
    }

    let device: DeviceCodeResponse = resp.json().await.context("Invalid device code response")?;

    println!();
    println!("  Open this URL in your browser:");
    println!("  {}", device.verification_uri);
    println!();
    println!("  Enter code: {}", device.user_code);
    println!();

    // Try to open the URL in the browser
    let _ = open_url(&device.verification_uri);

    // Step 2: Poll for token
    let interval = std::time::Duration::from_secs(device.interval.max(5));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(device.expires_in);

    print!("  Waiting for authorization...");
    let _ = std::io::Write::flush(&mut std::io::stdout());

    loop {
        if std::time::Instant::now() > deadline {
            println!();
            anyhow::bail!("Device code expired. Please try again.");
        }

        tokio::time::sleep(interval).await;

        let resp = client
            .post(format!("{}/api/v1/auth/device/token", base))
            .json(&serde_json::json!({
                "device_code": device.device_code,
            }))
            .send()
            .await;

        let resp = match resp {
            Ok(r) => r,
            Err(_) => continue,
        };

        if resp.status().is_success() {
            let token_resp: DeviceTokenResponse =
                resp.json().await.context("Invalid token response")?;
            println!(" done!");
            let tokens = AuthTokens {
                access_token: token_resp.access_token,
                refresh_token: token_resp.refresh_token,
                token_type: token_resp.token_type,
                expires_in: token_resp.expires_in,
            };
            save_tokens(&tokens)?;
            return Ok(tokens);
        }

        // Check if it's authorization_pending (expected) or a real error
        if let Ok(err) = resp.json::<ErrorResponse>().await {
            if err.error == "expired_token" {
                println!();
                anyhow::bail!("Device code expired. Please try again.");
            }
            // authorization_pending — keep polling
        }

        print!(".");
        let _ = std::io::Write::flush(&mut std::io::stdout());
    }
}

/// Build a reqwest client with auth header if tokens are available.
pub fn authenticated_client() -> Result<(reqwest::Client, Option<AuthTokens>)> {
    let tokens = load_tokens();
    let client = reqwest::Client::new();
    Ok((client, tokens))
}

/// Make an authenticated request, refreshing the token on 401 if needed.
/// Returns the response.
pub async fn auth_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
) -> Result<reqwest::RequestBuilder> {
    let tokens =
        load_tokens().ok_or_else(|| anyhow::anyhow!("Not logged in. Run `mur login` first."))?;

    Ok(client
        .request(method, url)
        .header("Authorization", format!("Bearer {}", tokens.access_token)))
}

fn open_url(url: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open").arg(url).spawn()?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open").arg(url).spawn()?;
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", url])
            .spawn()?;
    }
    Ok(())
}
