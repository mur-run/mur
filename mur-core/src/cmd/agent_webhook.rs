//! Track C5 / M5.1 — `mur agent webhook ...` CLI verbs.
//!
//! Mutates the agent's `profile.yaml` (`transport.webhook` block) and
//! the OS keychain entry holding the HMAC secret. The runtime uses
//! the same shape (M5.3): when `transport.webhook.enabled = true`,
//! the supervisor starts an Axum listener and validates the
//! `X-Mur-Signature` HMAC against `keychain_get(SECRET_SERVICE,
//! <agent>/WEBHOOK_HMAC)`.
//!
//! All four verbs are sync except `secret-set` which goes async to
//! await `keychain_set`. `enable` / `disable` / `show` write a
//! single YAML value through the existing `load_profile_for_edit`
//! helper so the file stays atomic.

use anyhow::{Context, Result, ensure};
use mur_common::agent::WebhookTransportConfig;

use super::agent::load_profile_for_edit;

/// Same service name the rest of the agent secrets use; key is
/// always the literal `WEBHOOK_HMAC` so callers don't have to
/// remember another knob.
const SECRET_SERVICE: &str = "mur-agent";
const SECRET_KEY: &str = "WEBHOOK_HMAC";

/// Default port we suggest when `--port` isn't passed. Mirrors
/// `WebhookTransportConfig::default()`'s port; kept in sync via the
/// same constant in `mur-common`.
const DEFAULT_PORT: u16 = 6789;

pub fn cmd_webhook_enable(agent: &str, bind: Option<String>, port: Option<u16>) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    let acct = format!("{agent}/{SECRET_KEY}");
    let secret_ref = format!("{SECRET_SERVICE}:{acct}");

    let cfg = WebhookTransportConfig {
        enabled: true,
        bind: bind.unwrap_or_else(|| "127.0.0.1".to_string()),
        port: port.unwrap_or(DEFAULT_PORT),
        hmac_secret_ref: secret_ref.clone(),
    };
    profile.transport.webhook = cfg.clone();
    write_profile_atomic(&path, &profile)?;
    println!(
        "Enabled webhook for {agent}: http://{}:{}/agents/{agent}/webhook",
        cfg.bind, cfg.port,
    );
    println!("HMAC secret ref: {secret_ref}");
    println!(
        "Run `mur agent webhook secret-set {agent}` to write the HMAC key, then restart the agent."
    );
    Ok(())
}

pub fn cmd_webhook_disable(agent: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(agent)?;
    profile.transport.webhook.enabled = false;
    // Leave bind/port/hmac_secret_ref alone so re-enabling doesn't
    // need a fresh secret-set.
    write_profile_atomic(&path, &profile)?;
    println!("Disabled webhook for {agent}");
    Ok(())
}

pub fn cmd_webhook_show(agent: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(agent)?;
    let yaml =
        serde_yaml_ng::to_string(&profile.transport.webhook).context("serialize webhook config")?;
    print!("{yaml}");
    Ok(())
}

pub async fn cmd_webhook_secret_set(agent: &str, value: Option<&str>) -> Result<()> {
    use mur_common::secret::keychain_set;
    let val: String = match value {
        Some(v) => v.to_string(),
        None => {
            eprint!("Enter HMAC secret for {agent} (input hidden): ");
            rpassword::read_password().context("read hidden value")?
        }
    };
    ensure!(!val.is_empty(), "HMAC secret must not be empty");
    let acct = format!("{agent}/{SECRET_KEY}");
    keychain_set(SECRET_SERVICE, &acct, &val)
        .await
        .with_context(|| format!("set {SECRET_SERVICE}/{acct}"))?;
    println!("Wrote {SECRET_SERVICE}/{acct}");
    Ok(())
}

fn write_profile_atomic(
    path: &std::path::Path,
    profile: &mur_common::agent::AgentProfile,
) -> Result<()> {
    let yaml = serde_yaml_ng::to_string(profile).context("serialize profile.yaml")?;
    let tmp = path.with_extension("yaml.tmp");
    std::fs::write(&tmp, yaml).with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_port_matches_common_default() {
        // Drift guard: `WebhookTransportConfig::default().port` and
        // this module's `DEFAULT_PORT` must agree so the CLI's
        // implicit default lines up with the runtime's view.
        assert_eq!(WebhookTransportConfig::default().port, DEFAULT_PORT);
    }

    #[test]
    fn default_bind_matches_common_default() {
        assert_eq!(
            WebhookTransportConfig::default().bind,
            "127.0.0.1",
            "default bind must stay localhost — public exposure is opt-in"
        );
    }
}
