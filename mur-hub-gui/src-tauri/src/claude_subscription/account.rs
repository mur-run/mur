//! `claude auth status / login / logout`, wrapped the way `codex` is.
//!
//! `claude auth status --json` is the account control plane. Only three
//! fields survive parsing — `loggedIn`, `authMethod`, `email` — the rest
//! (`orgId`, paths, flags) is dropped before it can reach a view or a log.
//! A subscription is `loggedIn && authMethod == "claude.ai"`; a Console
//! login (`console`) is API billing and renders as "signed in, but not this
//! provider", exactly like Codex `apiKey`.

use crate::chatgpt_subscription::SubscriptionAccountView;
use crate::chatgpt_subscription::process::{
    LOGOUT_CONFIRMATION_REQUIRED, LoginResult, run_bounded,
};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

const STATUS_TIMEOUT: Duration = Duration::from_secs(30);
const LOGIN_TIMEOUT: Duration = Duration::from_secs(300);
const SUBSCRIPTION_AUTH_METHOD: &str = "claude.ai";

/// Two Hub windows must not open two browser login flows.
static LOGIN_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The JSON object inside possibly-noisy combined output (stderr banners,
/// update notices). Anything unparseable is "signed out", not an error:
/// the user can act on that, and the raw text never leaves this function.
pub fn parse_auth_status(raw: &str) -> SubscriptionAccountView {
    let present = SubscriptionAccountView {
        cli_present: true,
        ..Default::default()
    };
    let Some(json) = raw.find('{').zip(raw.rfind('}')).map(|(a, b)| &raw[a..=b]) else {
        return present;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return present;
    };
    let logged_in_any = v["loggedIn"].as_bool().unwrap_or(false);
    let method = v["authMethod"].as_str().map(str::to_string);
    let subscription = logged_in_any && method.as_deref() == Some(SUBSCRIPTION_AUTH_METHOD);
    SubscriptionAccountView {
        cli_present: true,
        logged_in: subscription,
        auth_mode: if logged_in_any { method } else { None },
        email: if subscription {
            v["email"].as_str().map(str::to_string)
        } else {
            None
        },
        plan_type: None,
    }
}

pub async fn read_account(claude: &Path) -> Result<SubscriptionAccountView, String> {
    let mut cmd = Command::new(claude);
    cmd.args(["auth", "status", "--json"]);
    // Exit code deliberately ignored: a signed-out status may exit non-zero
    // and still carry the JSON that says so.
    let (_ok, out) = run_bounded(cmd, STATUS_TIMEOUT).await?;
    Ok(parse_auth_status(&out))
}

/// `claude auth login --claudeai`, then ask the account — exit code zero
/// alone is not success, and a Console login is not this provider.
pub async fn login(claude: &Path) -> LoginResult {
    let _one_at_a_time = LOGIN_LOCK.lock().await;
    let failed = |error: String| LoginResult {
        authenticated: false,
        error: Some(error),
    };
    let mut cmd = Command::new(claude);
    cmd.args(["auth", "login", "--claudeai"]);
    let output = match run_bounded(cmd, LOGIN_TIMEOUT).await {
        Ok((true, out)) => out,
        Ok((false, out)) => return failed(format!("claude auth login failed: {}", out.trim())),
        Err(e) => return failed(format!("claude auth login: {e}")),
    };
    match read_account(claude).await {
        Ok(a) if a.logged_in => LoginResult {
            authenticated: true,
            error: None,
        },
        Ok(a) => failed(format!(
            "claude auth login finished but no Claude subscription is signed in (auth: {}). {}",
            a.auth_mode.as_deref().unwrap_or("none"),
            output.trim()
        )),
        Err(e) => failed(e),
    }
}

/// Global sign-out. Refuses without `confirmed` — nothing is spawned.
pub async fn logout(claude: &Path, confirmed: bool) -> Result<(), String> {
    if !confirmed {
        return Err(LOGOUT_CONFIRMATION_REQUIRED.into());
    }
    let mut cmd = Command::new(claude);
    cmd.args(["auth", "logout"]);
    let (ok, out) = run_bounded(cmd, STATUS_TIMEOUT).await?;
    if !ok {
        return Err(format!("claude auth logout failed: {}", out.trim()));
    }
    match read_account(claude).await {
        Ok(a) if a.logged_in => {
            Err("claude auth logout ran but a subscription is still signed in".into())
        }
        Ok(_) => Ok(()),
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_claude_ai_login_is_a_subscription_and_nothing_else_survives() {
        let sub = parse_auth_status(
            r#"{"loggedIn":true,"authMethod":"claude.ai","apiProvider":"firstParty","email":"u@example.com","orgId":"org-SECRET","projectsDirectory":"/Users/x/.claude/projects"}"#,
        );
        assert_eq!(
            sub,
            SubscriptionAccountView {
                cli_present: true,
                logged_in: true,
                auth_mode: Some("claude.ai".into()),
                email: Some("u@example.com".into()),
                plan_type: None,
            }
        );
        assert!(
            !format!("{sub:?}").contains("SECRET"),
            "orgId leaked into the view"
        );

        let console = parse_auth_status(
            r#"{"loggedIn":true,"authMethod":"console","email":"api@example.com"}"#,
        );
        assert!(
            !console.logged_in,
            "a Console login is API billing, not a subscription"
        );
        assert_eq!(console.auth_mode.as_deref(), Some("console"));
        assert_eq!(
            console.email, None,
            "a non-subscription identity is not this provider's"
        );

        let out = parse_auth_status(r#"{"loggedIn":false}"#);
        assert!(!out.logged_in);
        assert_eq!(out.auth_mode, None);

        let noisy = parse_auth_status(
            "Update available: 2.2.0\n{\"loggedIn\":true,\"authMethod\":\"claude.ai\"}\n",
        );
        assert!(noisy.logged_in);

        let garbage = parse_auth_status("not json at all");
        assert!(garbage.cli_present && !garbage.logged_in);
        let unknown_method = parse_auth_status(r#"{"loggedIn":true,"authMethod":"something-new"}"#);
        assert!(
            !unknown_method.logged_in,
            "an unknown method is never assumed to be a subscription"
        );
    }
}
