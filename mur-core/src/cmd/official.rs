//! `mur official` — browse + install from the official MUR catalog.
use anyhow::{Context, Result, bail};
use mur_common::official::{LicenseCheck, check_license};
use mur_common::skill::publisher_trust::MUR_OFFICIAL_LICENSE_KEY_FP;

use crate::official::client::{download_item, fetch_catalog};
use crate::official::store::save_license;

pub(crate) async fn cmd_official_list() -> Result<()> {
    let base = crate::auth::server_url();
    let items = fetch_catalog(&reqwest::Client::new(), &base).await?;
    if items.is_empty() {
        println!("No official items published yet.");
        return Ok(());
    }
    println!("{:<32} {:<6} {:<8} DESCRIPTION", "ID", "TIER", "VERSION");
    for i in &items {
        println!(
            "{:<32} {:<6} {:<8} {}",
            i.id, i.tier, i.version, i.description
        );
    }
    if crate::auth::load_tokens().is_none() {
        println!("\nLog in with `mur login` to install (pro items need a MUR Pro subscription).");
    }
    Ok(())
}

pub(crate) async fn cmd_official_install(id: &str) -> Result<()> {
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
    println!("Installed official item {id}");
    Ok(())
}
