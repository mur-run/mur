//! Install one item from the official catalog.
//!
//! Lives here rather than in `cmd/` because both front-ends need it: the CLI
//! (`mur official install`) and the Hub's agent wizard, which browses the same
//! catalog. The function does the whole trust chain — download, verify the
//! account-bound license fail-closed, persist it, then hand the bundle to the
//! ordinary import paths (which re-verify signatures against that license) —
//! and prints nothing, so callers own their own output.

use anyhow::{Context, Result, bail};
use mur_common::official::{LicenseCheck, check_license};
use mur_common::skill::publisher_trust::MUR_OFFICIAL_LICENSE_KEY_FP;

use crate::official::client::download_item;
use crate::official::store::save_license;

/// Download and install `id` (`agents/<name>` or `fleets/<name>`).
pub async fn install_item(id: &str) -> Result<()> {
    // 1. Identity first — everything downstream binds to it.
    let tokens = crate::auth::load_tokens().context("not logged in — run `mur login` first")?;
    let user_id = tokens
        .user_id
        .clone()
        .context("stored login has no account id — run `mur auth logout` then `mur login`")?;

    // 2. Download bundle + license.
    let base = crate::auth::server_url();
    let client = reqwest::Client::new();
    let (bytes, license) = download_item(&client, &base, &tokens.access_token, id).await?;

    // 3. Verify the license fail-closed BEFORE anything touches disk state.
    match check_license(&license, id, &user_id, MUR_OFFICIAL_LICENSE_KEY_FP) {
        LicenseCheck::Ok => {}
        other => bail!("server returned an invalid license ({other:?}) — refusing install"),
    }

    // 4. Persist license, then hand the bundle to the existing import paths
    //    (which re-verify signatures + the official gate against this license).
    let mur_home = crate::paths::mur_root(None);
    save_license(&mur_home, &license)?;
    let dir = tempfile::tempdir().context("temp dir")?;
    match id.split_once('/') {
        Some(("fleets", name)) => {
            let p = dir.path().join(format!("{name}.fleet"));
            std::fs::write(&p, &bytes).context("write bundle")?;
            crate::cmd::fleet::import::cmd_fleet_import(
                &mur_home,
                &p,
                crate::cmd::fleet::import::ImportOpts::default(),
            )?;
        }
        Some(("agents", name)) => {
            let p = dir.path().join(format!("{name}.muragent"));
            std::fs::write(&p, &bytes).context("write package")?;
            crate::cmd::agent::install::cmd_install(&p, None, None)?;
        }
        _ => bail!("unknown catalog id '{id}' — expected agents/<name> or fleets/<name>"),
    }
    Ok(())
}

/// The agent name an `agents/<name>` catalog id installs as, if it is one.
/// Fleet ids have no single agent name, so they yield `None`.
pub fn installed_agent_name(id: &str) -> Option<&str> {
    match id.split_once('/') {
        Some(("agents", name)) if !name.is_empty() => Some(name),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_ids_yield_a_name_others_do_not() {
        assert_eq!(
            installed_agent_name("agents/researcher"),
            Some("researcher")
        );
        assert_eq!(installed_agent_name("fleets/newsroom"), None);
        assert_eq!(installed_agent_name("agents/"), None);
        assert_eq!(installed_agent_name("researcher"), None);
    }
}
