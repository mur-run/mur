//! `mur browser replay`: load a recorded run, resolve its profile, drive
//! `mur_browser::replay`, and write the report.

use std::fs;

use anyhow::{Result, bail};
use mur_browser::{auth::ProfileMeta, paths, recorder::from_yaml, state::KeychainStateKeyStore};

use super::mur_home;

/// Replay a recorded run headlessly (spec §6, L1/L2 only).
///
/// Navigation is checked against the profile's `allow_domains` before
/// anything is spawned. With a profile, its encrypted state is decrypted into
/// an owner-only temp file that is deleted when replay returns. The report is
/// written as YAML to `runs/<name>/report.md` and summarised on stdout.
pub async fn replay(name: &str, profile: Option<&str>, heal: bool, dry_run: bool) -> Result<()> {
    paths::validate_name(name)?;
    if heal {
        bail!("mur browser replay --heal is not implemented yet (self-healing lands in Task 5)");
    }
    let home = mur_home()?;
    let path = paths::run_actions(&home, name);
    let yaml = fs::read_to_string(&path)
        .map_err(|error| anyhow::anyhow!("read browser run {}: {error}", path.display()))?;
    let run = from_yaml(&yaml)
        .map_err(|error| anyhow::anyhow!("invalid browser run {}: {error}", path.display()))?;
    // An explicit --profile wins over the one recorded with the run.
    let profile = profile
        .map(ToOwned::to_owned)
        .or_else(|| run.profile.clone());
    let allow = match &profile {
        Some(site) => {
            paths::validate_name(site)?;
            load_profile_meta(&home, site)?.allow_domains
        }
        None => Vec::new(),
    };

    tracing::info!(
        run = name,
        mode = ?run.mode,
        profile = profile.as_deref().unwrap_or(""),
        steps = run.steps.len(),
        dry_run,
        "browser replay started"
    );
    let report = if dry_run {
        mur_browser::replay::dry_run(&run, &allow)?
    } else {
        match &profile {
            Some(site) => {
                let state = mur_browser::state::read_state(
                    &paths::profile_state(&home, site),
                    &KeychainStateKeyStore,
                )?;
                // NamedTempFile is created 0600 and unlinked on drop.
                let mut file = tempfile::Builder::new()
                    .prefix("mur-browser-state-")
                    .suffix(".json")
                    .tempfile()?;
                std::io::Write::write_all(&mut file, &state)?;
                drop(state);
                mur_browser::replay::replay_live(&run, &allow, Some(file.path())).await?
            }
            None => mur_browser::replay::replay_live(&run, &allow, None).await?,
        }
    };

    tracing::info!(
        run = name,
        total = report.total,
        passed = report.passed,
        failed = report.failed,
        healed = report.healed,
        "browser replay finished"
    );
    if !dry_run {
        let out = paths::run_report(&home, name);
        fs::write(&out, serde_yaml::to_string(&report)?)
            .map_err(|error| anyhow::anyhow!("write replay report {}: {error}", out.display()))?;
    }
    if dry_run {
        println!(
            "dry-run {name}: {} steps, navigation allowed by profile; nothing launched",
            report.total
        );
        return Ok(());
    }
    println!("{}", report.summary());
    if report.failed > 0 {
        bail!("replay {name:?} failed");
    }
    Ok(())
}

fn load_profile_meta(home: &std::path::Path, site: &str) -> Result<ProfileMeta> {
    let meta_path = paths::profile_meta(home, site);
    let yaml = fs::read_to_string(&meta_path).map_err(|error| {
        anyhow::anyhow!(
            "read browser profile metadata {} (run `mur browser auth {site}` first): {error}",
            meta_path.display()
        )
    })?;
    serde_yaml::from_str(&yaml).map_err(|error| {
        anyhow::anyhow!(
            "invalid browser profile metadata {}: {error}",
            meta_path.display()
        )
    })
}
