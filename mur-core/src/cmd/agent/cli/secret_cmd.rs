//! murmur `/secret` — hand the running agent a credential without the value
//! ever touching the chat.
//!
//! Two writes, reported separately. The keychain plus `profile.secrets` make
//! it durable across restarts; `secret/set` over the unix socket makes it live
//! in the already-sealed runtime NOW (the runtime resolves secrets before the
//! sandbox closes, so a keychain write alone reaches it only after a restart).
//!
//! A keychain failure stops before the dial: nothing should "work for now" and
//! then vanish on restart, which reads as a deletion nobody performed.
//!
//! Keychain ✓ with the agent unreachable prints two distinct lines. Collapsing
//! them into one ✓ is exactly how a user ends up pasting the token into the
//! chat — the agent says it has no such variable, and the tick said otherwise.

use anyhow::{Context, Result};

use super::app::{App, RenderMode};
use crate::a2a_dial::{DialMode, dial_method};
use crate::cmd::agent::secret::SECRET_SERVICE;
use crate::cmd::agent::{load_profile_for_edit, save_profile};

/// Same floor as `mur_agent_runtime::secrets::MIN_LEN`. Duplicated rather than
/// shared because `mur-core` must not depend on the runtime crate; the runtime
/// re-checks on its own side, so the two can never silently disagree in the
/// permissive direction.
pub const MIN_LEN: usize = 8;

/// `[A-Z_][A-Z0-9_]*` — mirrors `mur_agent_runtime::secrets::valid_name`.
pub fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    match chars.next() {
        Some(c) if c.is_ascii_uppercase() || c == '_' => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_')
}

pub fn validate(name: &str, value: &str) -> std::result::Result<(), String> {
    if !valid_name(name) {
        return Err(format!("secret name '{name}' must match [A-Z_][A-Z0-9_]*"));
    }
    if value.is_empty() {
        return Err("cancelled (empty value)".into());
    }
    let n = value.chars().count();
    if n < MIN_LEN {
        return Err(format!(
            "value must be at least {MIN_LEN} characters (got {n})"
        ));
    }
    Ok(())
}

/// What happened, step by step. `Both` is the only state that earns a ✓ line.
pub enum StepReport {
    Both,
    /// Keychain + profile written; the dial failed with this message.
    DurableOnly(String),
}

pub fn report_line(name: &str, r: &StepReport) -> String {
    match r {
        StepReport::Both => format!("✓ {name} available as ${name}"),
        // -32601 is "method not found", which here means the agent is running
        // a runtime older than /secret — the normal window after `mur update`
        // before a restart. Say that instead of showing the raw code.
        StepReport::DurableOnly(e) if e.contains("-32601") => {
            "saved to keychain ✓ · running agent: not reached — its runtime predates /secret \
             (mur update, then restart to load)"
                .to_string()
        }
        StepReport::DurableOnly(e) => {
            format!("saved to keychain ✓ · running agent: not reached ({e}) — restart to load")
        }
    }
}

/// `/secret [KEY] [--delete]` — with no key, list what is set (names only).
/// With a key, validate the name now and hand the read to the main loop,
/// which owns the terminal.
pub fn request(app: &mut App, key: Option<String>, delete: bool) {
    let Some(key) = key else {
        list(app);
        return;
    };
    if !valid_name(&key) {
        app.push_error(format!(
            "secret name '{key}' must match [A-Z_][A-Z0-9_]* — it becomes an environment variable"
        ));
        return;
    }
    if delete {
        app.pending_secret_delete = Some(key);
        return;
    }
    // The hidden read suspends the terminal, which only the inline viewport
    // can be restored from — the same constraint `/login`'s handover has.
    if app.render_mode != RenderMode::Inline {
        app.push_error("/secret needs the inline view — close the overlay (Esc) and try again");
        return;
    }
    app.pending_secret_prompt = Some(key);
}

fn list(app: &mut App) {
    match load_profile_for_edit(&app.agent) {
        Ok((_, p)) if p.secrets.is_empty() => {
            app.push_system("no secrets set — /secret <KEY> to hand the agent one")
        }
        Ok((_, p)) => app.push_system(format!(
            "secrets (names only): {}\n/secret <KEY> to add or replace · /secret <KEY> --delete to revoke",
            p.secrets.join(", ")
        )),
        Err(e) => app.push_error(format!("read profile: {e:#}")),
    }
}

/// Step ①: keychain, then `profile.secrets`. Either failure aborts the whole
/// command — see the module doc for why a runtime-only write is worse than
/// no write at all.
async fn write_durable(agent: &str, name: &str, value: &str) -> Result<()> {
    let acct = format!("{agent}/{name}");
    mur_common::secret::keychain_set(SECRET_SERVICE, &acct, value)
        .await
        .with_context(|| format!("keychain write {SECRET_SERVICE}/{acct}"))?;
    let (path, mut profile) = load_profile_for_edit(agent)?;
    if !profile.secrets.iter().any(|n| n == name) {
        profile.secrets.push(name.to_string());
        save_profile(&path, &mut profile).context("profile write (secrets list)")?;
    }
    Ok(())
}

async fn remove_durable(agent: &str, name: &str) -> Result<()> {
    let acct = format!("{agent}/{name}");
    mur_common::secret::keychain_delete(SECRET_SERVICE, &acct)
        .await
        .with_context(|| format!("keychain delete {SECRET_SERVICE}/{acct}"))?;
    let (path, mut profile) = load_profile_for_edit(agent)?;
    let before = profile.secrets.len();
    profile.secrets.retain(|n| n != name);
    if profile.secrets.len() != before {
        save_profile(&path, &mut profile).context("profile write (secrets list)")?;
    }
    Ok(())
}

/// Step ②: tell the running agent. Any error becomes `DurableOnly` — the
/// durable half already succeeded, and saying otherwise would be a lie in the
/// direction that costs the user their credential.
async fn dial(app: &App, method: &'static str, params: serde_json::Value) -> StepReport {
    let (h, ag) = (app.home.clone(), app.agent.clone());
    match tokio::task::spawn_blocking(move || dial_method(&h, &ag, method, params, DialMode::Auto))
        .await
    {
        Ok(Ok(_)) => StepReport::Both,
        Ok(Err(e)) => StepReport::DurableOnly(e.to_string()),
        Err(e) => StepReport::DurableOnly(format!("dial task failed: {e}")),
    }
}

/// Called by the main loop with the hidden-read value. Validates, writes,
/// dials, reports. The value is dropped when this returns.
pub async fn after_hidden_input(app: &mut App, name: String, value: String) {
    if let Err(e) = validate(&name, &value) {
        app.push_error(format!("{name}: {e}"));
        return;
    }
    if let Err(e) = write_durable(&app.agent, &name, &value).await {
        app.push_error(format!("{name}: {e:#} — nothing sent to the agent"));
        return;
    }
    let r = dial(
        app,
        "secret/set",
        serde_json::json!({ "name": name, "value": value }),
    )
    .await;
    app.push_system(report_line(&name, &r));
}

pub async fn after_delete(app: &mut App, name: String) {
    if let Err(e) = remove_durable(&app.agent, &name).await {
        app.push_error(format!("{name}: {e:#}"));
        return;
    }
    let r = dial(app, "secret/delete", serde_json::json!({ "name": name })).await;
    app.push_system(match r {
        StepReport::Both => format!("✓ {name} removed"),
        StepReport::DurableOnly(e) => format!(
            "removed from keychain ✓ · running agent: not reached ({e}) — it keeps the value \
             until it restarts"
        ),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn report_both_ok_is_one_line() {
        assert_eq!(
            report_line("GITEA_TOKEN", &StepReport::Both),
            "✓ GITEA_TOKEN available as $GITEA_TOKEN"
        );
    }

    #[test]
    fn report_agent_not_reached_is_two_facts_not_one_tick() {
        let line = report_line(
            "GITEA_TOKEN",
            &StepReport::DurableOnly("connection refused".into()),
        );
        assert!(line.starts_with("saved to keychain ✓"), "{line}");
        assert!(line.contains("running agent: not reached"), "{line}");
        assert!(line.contains("restart to load"), "{line}");
        assert!(!line.contains("available as"), "{line}");
    }

    #[test]
    fn a_method_not_found_is_reported_as_an_older_runtime() {
        let line = report_line(
            "K",
            &StepReport::DurableOnly(r#"{"code":-32601,"message":"method not found"}"#.into()),
        );
        assert!(line.contains("predates /secret"), "{line}");
        assert!(!line.contains("-32601"), "{line}");
    }

    #[test]
    fn validation_happens_before_any_write() {
        assert_eq!(
            validate("gitea", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").unwrap_err(),
            "secret name 'gitea' must match [A-Z_][A-Z0-9_]*"
        );
        assert_eq!(
            validate("K", "short").unwrap_err(),
            "value must be at least 8 characters (got 5)"
        );
        assert_eq!(validate("K", "").unwrap_err(), "cancelled (empty value)");
        assert!(validate("GITEA_TOKEN", "d8b04a3cc632a5c8026cf5a810d36e292c603f99").is_ok());
    }
}
