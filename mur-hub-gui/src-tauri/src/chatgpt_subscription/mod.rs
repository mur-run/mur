//! ChatGPT Subscription provider — the Hub side.
//!
//! Codex owns the login and the credential file; this module never reads
//! `~/.codex/auth.json`. Account and model facts come from a short-lived
//! `codex app-server` session (see [`app_server`]), and every view here is
//! display-safe by construction: no token, no account id.

pub mod app_server;
pub mod process;
pub mod registry;

pub use app_server::{ControlError, list_models, read_account};
pub use process::{CHATGPT_GATEWAY_BASE, GatewayStatusView, LoginResult};

use serde::Serialize;
use std::path::PathBuf;

/// What the panel needs to know about the Codex login. `cli_present: false`
/// is the whole story when `codex` is not installed; the other fields are
/// only meaningful once it is.
#[derive(Debug, Clone, Serialize, PartialEq, Eq, Default)]
pub struct ChatGptAccountView {
    pub cli_present: bool,
    /// True only for a ChatGPT (subscription) login. An API-key login is
    /// `false` here — it is a different bill, not a lesser subscription.
    pub logged_in: bool,
    /// Raw `account.type` (`chatgpt` / `apiKey`), for diagnostics.
    pub auth_mode: Option<String>,
    pub email: Option<String>,
    pub plan_type: Option<String>,
}

/// One entry from `model/list`, trimmed to what a picker row shows.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ChatGptModelView {
    pub id: String,
    pub display_name: String,
    pub is_default: bool,
    pub reasoning_efforts: Vec<String>,
    pub input_modalities: Vec<String>,
}

/// The `codex` the user's shell would run — same discipline as `mur`
/// (`cli_tools::shell_which`), then the usual install dirs.
pub fn resolve_codex() -> Option<PathBuf> {
    #[cfg(unix)]
    if let Some(p) = crate::cli_tools::shell_which("codex") {
        return Some(p);
    }
    let home = dirs::home_dir()?;
    [
        PathBuf::from("/opt/homebrew/bin/codex"),
        PathBuf::from("/usr/local/bin/codex"),
        home.join(".local/bin/codex"),
        home.join(".npm-global/bin/codex"),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

pub(crate) async fn resolve_codex_async() -> Option<PathBuf> {
    // `shell_which` runs a login shell; keep it off the async executor.
    tokio::task::spawn_blocking(resolve_codex)
        .await
        .ok()
        .flatten()
}

#[tauri::command]
pub async fn chatgpt_account_read() -> Result<ChatGptAccountView, String> {
    let Some(codex) = resolve_codex_async().await else {
        return Ok(ChatGptAccountView::default());
    };
    read_account(&codex).await.map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn chatgpt_models_list() -> Result<Vec<ChatGptModelView>, String> {
    let codex = resolve_codex_async()
        .await
        .ok_or_else(|| ControlError::CliMissing.to_string())?;
    list_models(&codex).await.map_err(|e| e.to_string())
}

/// Tests that write a fake executable and spawn it must not interleave: on
/// Linux a fork inherits another thread's still-open write fd, and exec of
/// that file fails with ETXTBSY (`Text file busy`). Hold this for the whole
/// test body, writer and spawner alike.
#[cfg(test)]
pub(crate) static FAKE_BIN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
