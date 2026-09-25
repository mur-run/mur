//! `mur browser replay`: load a recorded run, resolve its profile, drive
//! `mur_browser::replay`, write verified heals back, and write the report.

use std::{fs, path::Path};

use anyhow::{Result, bail};
use mur_browser::{
    auth::ProfileMeta,
    heal, paths,
    recorder::{Run, from_yaml, to_yaml},
    replay::ReplayReport,
    state::KeychainStateKeyStore,
};

use super::mur_home;

/// Replay a recorded run headlessly (spec §6, L1/L2 only).
///
/// Navigation is checked against the profile's `allow_domains` before
/// anything is spawned. With a profile, its encrypted state is decrypted into
/// an owner-only temp file that is deleted when replay returns. The report is
/// written as YAML to `runs/<name>/report.md` and summarised on stdout.
/// With `heal`, verified heals are written back to `actions.yaml` (D3).
pub async fn replay(
    name: &str,
    profile: Option<&str>,
    heal: bool,
    max_heal_ratio: f32,
    dry_run: bool,
) -> Result<()> {
    paths::validate_name(name)?;
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
    let opts = mur_browser::replay::ReplayOptions {
        heal,
        max_heal_ratio,
    };
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
                mur_browser::replay::replay_live(&run, &allow, opts, Some(file.path())).await?
            }
            None => mur_browser::replay::replay_live(&run, &allow, opts, None).await?,
        }
    };

    if dry_run {
        println!(
            "dry-run {name}: {} steps, navigation allowed by profile; nothing launched",
            report.total
        );
        return Ok(());
    }
    finish(&home, name, run, report)
}

/// Parse `--max-heal-ratio`: a share in `0.0..=1.0` (D4).
pub fn parse_heal_ratio(raw: &str) -> std::result::Result<f32, String> {
    let ratio: f32 = raw
        .parse()
        .map_err(|_| format!("{raw:?} is not a number"))?;
    if (0.0..=1.0).contains(&ratio) {
        Ok(ratio)
    } else {
        Err(format!("{ratio} is outside 0.0..=1.0"))
    }
}

/// After a live replay: write back verified heals, then the report, then the
/// summary, and only then decide the exit status — so a red run (or a failed
/// write-back) always leaves its report on disk.
fn finish(home: &Path, name: &str, mut run: Run, mut report: ReplayReport) -> Result<()> {
    // Spec D3: write back only when the whole run is trustworthy.
    let may_write = report.failed == 0 && report.budget_exceeded.is_none();
    let mut write_error = None;
    let applied = if may_write {
        heal::apply_verified(&mut run, &report.heals)
    } else {
        0
    };
    if applied > 0 {
        match write_actions_atomic(&paths::run_actions(home, name), &run) {
            // Filled only after the rename landed (spec 輸出).
            Ok(()) => report.written_back = applied,
            Err(error) => write_error = Some(error),
        }
    }

    tracing::info!(
        run = name,
        total = report.total,
        passed = report.passed,
        failed = report.failed,
        healed = report.healed,
        written_back = report.written_back,
        "browser replay finished"
    );
    let out = paths::run_report(home, name);
    fs::write(&out, serde_yaml::to_string(&report)?)
        .map_err(|error| anyhow::anyhow!("write replay report {}: {error}", out.display()))?;
    println!("{}", report.summary());
    if let Some(error) = write_error {
        return Err(error.context(format!(
            "replay {name:?}: writing verified heals back to actions.yaml failed (the report was written)"
        )));
    }
    if report.failed > 0 {
        bail!("replay {name:?} failed");
    }
    if let Some(over) = &report.budget_exceeded {
        bail!("replay {name:?}: {over}");
    }
    Ok(())
}

/// Replace `actions.yaml` via a temp file in the same directory + rename, so
/// a crash never leaves a half-written recording.
fn write_actions_atomic(path: &Path, run: &Run) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("actions path has no parent: {}", path.display()))?;
    let mut temp = tempfile::Builder::new()
        .prefix(".actions-")
        .suffix(".yaml.tmp")
        .tempfile_in(parent)?;
    std::io::Write::write_all(&mut temp, to_yaml(run)?.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(path)
        .map_err(|error| anyhow::anyhow!("rename into {}: {}", path.display(), error.error))?;
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

#[cfg(test)]
#[path = "replay_tests.rs"]
mod tests;
