//! Claude Subscription provider — the Hub side. Sibling of
//! `chatgpt_subscription`; shares its views, gateway lifecycle, registry
//! rules, and bounded child runner. Claude Code owns the login; this module
//! never reads the keychain blob or the credentials file.

pub mod account;
pub mod catalog;

use crate::chatgpt_subscription::process::LoginResult;
use crate::chatgpt_subscription::registry::{add_subscription_models, disconnect_subscription};
use crate::chatgpt_subscription::{
    SubscriptionAccountView, SubscriptionModelPick, SubscriptionModelView,
};
use mur_common::model::ModelRegistry;
use std::path::PathBuf;

/// The gateway's Anthropic route; the runtime appends `/messages`.
pub const CLAUDE_GATEWAY_BASE: &str = "http://127.0.0.1:8088/v1";
const CLAUDE_PROVIDER: &str = "claude";
const CLAUDE_BIN: &str = "claude";
const CLI_MISSING: &str = "claude CLI not found on PATH";

/// The `claude` the user's shell would run — same discipline as `codex`.
pub fn resolve_claude() -> Option<PathBuf> {
    #[cfg(unix)]
    if let Some(p) = crate::cli_tools::shell_which(CLAUDE_BIN) {
        return Some(p);
    }
    let home = dirs::home_dir()?;
    [
        home.join(".local/bin").join(CLAUDE_BIN),
        PathBuf::from("/opt/homebrew/bin").join(CLAUDE_BIN),
        PathBuf::from("/usr/local/bin").join(CLAUDE_BIN),
        home.join(".npm-global/bin").join(CLAUDE_BIN),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

async fn claude_or_err() -> Result<PathBuf, String> {
    // `shell_which` runs a login shell; keep it off the async executor.
    tokio::task::spawn_blocking(resolve_claude)
        .await
        .ok()
        .flatten()
        .ok_or_else(|| CLI_MISSING.to_string())
}

fn mur_home() -> PathBuf {
    ModelRegistry::default_path()
        .ok()
        .and_then(|p| p.parent().map(|x| x.to_path_buf()))
        .unwrap_or_default()
}

#[tauri::command]
pub async fn claude_account_read() -> Result<SubscriptionAccountView, String> {
    let Ok(claude) = claude_or_err().await else {
        return Ok(SubscriptionAccountView::default());
    };
    account::read_account(&claude).await
}

#[tauri::command]
pub async fn claude_login() -> Result<LoginResult, String> {
    Ok(account::login(&claude_or_err().await?).await)
}

#[tauri::command]
pub async fn claude_logout(confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err(crate::chatgpt_subscription::process::LOGOUT_CONFIRMATION_REQUIRED.into());
    }
    account::logout(&claude_or_err().await?, true).await
}

#[tauri::command]
pub async fn claude_models_list() -> Result<Vec<SubscriptionModelView>, String> {
    let home = mur_home();
    // `load_or_fetch` may block on the network; keep it off the executor.
    tokio::task::spawn_blocking(move || catalog::catalog_models(&home))
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| {
            "the models.dev catalog is not reachable and no cached copy exists".to_string()
        })
}

#[tauri::command]
pub fn claude_models_add(picks: Vec<SubscriptionModelPick>) -> Result<(), String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    add_subscription_models(&mut reg, CLAUDE_PROVIDER, CLAUDE_GATEWAY_BASE, &picks)?;
    reg.save_to(&path).map_err(|e| e.to_string())
}

/// Registry entries only. The Claude Code login and the gateway are
/// untouched — every other Claude Code client keeps working.
#[tauri::command]
pub fn claude_disconnect() -> Result<u32, String> {
    let path = ModelRegistry::default_path().map_err(|e| e.to_string())?;
    let mut reg = ModelRegistry::load_from(&path).map_err(|e| e.to_string())?;
    let removed = disconnect_subscription(&mut reg, CLAUDE_PROVIDER);
    if removed > 0 {
        reg.save_to(&path).map_err(|e| e.to_string())?;
    }
    Ok(removed)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::Write;
    use std::os::unix::fs::PermissionsExt;

    /// A fake `claude` that records argv and answers `auth status` with the
    /// JSON `body`; `auth login` / `auth logout` exit with `login_exit`.
    fn fake_claude(
        dir: &tempfile::TempDir,
        status_json: &str,
        login_exit: u8,
    ) -> (PathBuf, PathBuf) {
        let marker = dir.path().join("invoked");
        let bin = dir.path().join("claude");
        let src = format!(
            "#!/bin/sh\necho \"$@\" >> '{}'\ncase \"$1 $2\" in\n  'auth status') printf '%s\\n' '{}';;\n  'auth login'|'auth logout') exit {};;\nesac\n",
            marker.display(),
            status_json,
            login_exit
        );
        std::fs::File::create(&bin)
            .unwrap()
            .write_all(src.as_bytes())
            .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        (bin, marker)
    }

    #[tokio::test]
    async fn status_login_and_logout_believe_the_account() {
        let _serial = crate::chatgpt_subscription::FAKE_BIN_LOCK.lock().await;
        let dir = tempfile::tempdir().unwrap();

        // Console login: `login` exits 0 but the account is not a subscription.
        let (bin, marker) = fake_claude(&dir, r#"{"loggedIn":true,"authMethod":"console"}"#, 0);
        let view = account::read_account(&bin).await.unwrap();
        assert!(view.cli_present && !view.logged_in);
        let r = account::login(&bin).await;
        assert!(!r.authenticated);
        assert!(r.error.unwrap().contains("console"));
        let argv = std::fs::read_to_string(&marker).unwrap();
        assert!(argv.contains("auth status --json"), "{argv}");
        assert!(argv.contains("auth login --claudeai"), "{argv}");

        // Subscription login succeeds only because status says so.
        let dir = tempfile::tempdir().unwrap();
        let (bin, _) = fake_claude(
            &dir,
            r#"{"loggedIn":true,"authMethod":"claude.ai","email":"u@example.com"}"#,
            0,
        );
        assert!(account::login(&bin).await.authenticated);
        // Logout ran, but status still says signed in → error, not success.
        assert!(account::logout(&bin, true).await.is_err());

        // No confirmation → nothing spawned.
        let dir = tempfile::tempdir().unwrap();
        let (bin, marker) = fake_claude(&dir, r#"{"loggedIn":false}"#, 0);
        assert_eq!(
            account::logout(&bin, false).await.err().unwrap(),
            crate::chatgpt_subscription::process::LOGOUT_CONFIRMATION_REQUIRED
        );
        assert!(
            !marker.exists(),
            "a process was spawned without confirmation"
        );
        // Confirmed logout with a signed-out status is a clean success.
        assert!(account::logout(&bin, true).await.is_ok());

        // Missing binary is a spawn error, not a panic.
        assert!(
            account::read_account(std::path::Path::new("/nonexistent/claude"))
                .await
                .is_err()
        );
    }
}
