//! `mur agent secret` — set / delete / list per-agent keychain secrets.

use anyhow::{Context, Result};

use super::load_profile_for_edit;

pub(crate) const SECRET_SERVICE: &str = "mur-agent";

/// Clears the "keychain prompt pending" markers for an agent (#1980), so its
/// next start probes those secrets again. Best-effort; a stale marker only
/// means one secret stays skipped until the next user action.
pub(crate) fn clear_pending_keychain(agent_home: &std::path::Path) {
    let n = mur_common::secret::keychain_pending::clear_all(agent_home);
    if n > 0 {
        tracing::debug!(cleared = n, "cleared pending keychain markers");
    }
}

/// What `mur agent status` says about secrets skipped at start because an
/// earlier keychain prompt was never answered (#1980). Without it the only
/// trace is the runtime's stderr log. `None` when nothing is pending.
pub(crate) fn keychain_pending_line(agent_home: &std::path::Path) -> Option<String> {
    let names = mur_common::secret::keychain_pending::list(agent_home);
    (!names.is_empty()).then(|| {
        format!(
            "Secrets: waiting for keychain approval: {} — in the macOS prompt choose \
             \"Always Allow\" (not \"Allow\"), then `mur agent restart`",
            names.join(", ")
        )
    })
}

/// Same, for one secret of a named agent. Silent when the home is unresolvable.
pub(crate) fn clear_pending_keychain_for(agent: &str, key: &str) {
    if let Ok(home) = super::resolve_mur_home() {
        mur_common::secret::keychain_pending::clear(&home.join("agents").join(agent), key);
    }
}

pub async fn cmd_secret_set(agent: &str, key: &str, value: Option<&str>) -> Result<()> {
    use mur_common::secret::keychain_set;
    let val: String = match value {
        Some(v) => v.to_string(),
        None => {
            eprint!("Enter value for {key} (input hidden): ");
            rpassword::read_password().context("read hidden value")?
        }
    };
    let acct = format!("{agent}/{key}");
    keychain_set(SECRET_SERVICE, &acct, &val)
        .await
        .with_context(|| format!("set {SECRET_SERVICE}/{acct}"))?;
    clear_pending_keychain_for(agent, key);
    println!("Wrote {SECRET_SERVICE}/{acct}");
    Ok(())
}

pub async fn cmd_secret_delete(agent: &str, key: &str) -> Result<()> {
    use mur_common::secret::keychain_delete;
    let acct = format!("{agent}/{key}");
    keychain_delete(SECRET_SERVICE, &acct)
        .await
        .with_context(|| format!("delete {SECRET_SERVICE}/{acct}"))?;
    clear_pending_keychain_for(agent, key);
    println!("Deleted {SECRET_SERVICE}/{acct}");
    Ok(())
}

pub async fn cmd_secret_list(agent: &str) -> Result<()> {
    use mur_common::model::ModelRegistry;
    let (_path, profile) = load_profile_for_edit(agent)?;
    match profile.model_ref.as_deref() {
        Some(name) => {
            let reg_path = ModelRegistry::default_path()?;
            let reg = ModelRegistry::load_from(&reg_path)
                .with_context(|| format!("load registry {}", reg_path.display()))?;
            match reg.models.get(name) {
                Some(entry) => match &entry.secret {
                    Some(s) => {
                        let ok = s.check().await;
                        println!(
                            "{} ({}) — {}",
                            s,
                            name,
                            if ok { "present" } else { "missing" }
                        );
                    }
                    None => println!("{name}: no secret configured"),
                },
                None => println!("model_ref '{name}' not found in registry"),
            }
        }
        None => println!("agent uses inline model — no registry secret"),
    }
    Ok(())
}

#[cfg(test)]
mod pending_tests {
    use super::*;

    #[test]
    fn status_line_names_pending_secrets_and_the_fix() {
        let home = tempfile::tempdir().unwrap();
        assert!(keychain_pending_line(home.path()).is_none());
        mur_common::secret::keychain_pending::mark(home.path(), "GOOGLE_MAPS_KEY").unwrap();
        let line = keychain_pending_line(home.path()).unwrap();
        assert!(line.contains("GOOGLE_MAPS_KEY"), "{line}");
        assert!(line.contains("Always Allow"), "{line}");
        clear_pending_keychain(home.path());
        assert!(keychain_pending_line(home.path()).is_none());
    }
}
