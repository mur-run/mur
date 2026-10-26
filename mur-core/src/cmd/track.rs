//! `mur track` — inspect and clean up turn tracks (`<project>/.worktrees/turn-*`).
//!
//! The runtime reclaims stale tracks on its own whenever it opens a new one
//! (`task_runner::track_reclaim`). `prune` is the same sweep on demand, for a
//! project no agent has touched since its tracks went stale. It goes through
//! the same `mur_track::turn::reclaim` rules, so a track whose owner process
//! is still alive is never a candidate.

use anyhow::{Context, Result};
use clap::Subcommand;
use mur_track::turn::reclaim::{StaleReason, StaleTrack, project_root, reclaim, stale_tracks};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// `<mur_home>/artifacts/<this>/<project>/` holds the patches `prune` saves.
pub const PRUNE_ARTIFACT_DIR: &str = "track-prune";
const ARTIFACTS_DIR: &str = "artifacts";
const CONFIG_FILE: &str = "config.yaml";

#[derive(Debug, Subcommand)]
pub enum TrackAction {
    /// Reclaim stale turn tracks: released, owner process gone, or (no owner
    /// record) older than 24h. Each diff is saved as a patch first.
    Prune {
        /// Repository to sweep (any directory inside it). Default: current directory.
        #[arg(long)]
        project: Option<PathBuf>,
        /// List what would be reclaimed; change nothing.
        #[arg(long)]
        dry_run: bool,
        /// Grace period, e.g. `0`, `10m`, `2h`. Default: `turn_track.stale_grace_mins`.
        #[arg(long)]
        grace: Option<String>,
        /// Machine-readable output: one JSON array on stdout.
        #[arg(long)]
        json: bool,
    },
}

pub fn run(mur_home: &Path, action: TrackAction) -> Result<()> {
    match action {
        TrackAction::Prune {
            project,
            dry_run,
            grace,
            json,
        } => {
            let dir = match project {
                Some(p) => p,
                None => std::env::current_dir().context("current directory")?,
            };
            let grace = match grace {
                Some(g) => parse_grace(&g)?,
                None => configured_grace(mur_home),
            };
            let opts = PruneOpts {
                dry_run,
                grace,
                json,
            };
            prune(mur_home, &dir, &opts, &mut std::io::stdout())
        }
    }
}

pub(crate) struct PruneOpts {
    pub dry_run: bool,
    pub grace: Duration,
    pub json: bool,
}

fn parse_grace(s: &str) -> Result<Duration> {
    mur_common::limits::parse_duration(s).ok_or_else(|| {
        anyhow::anyhow!(
            "unrecognised duration `{s}` — use a number of seconds or a unit \
             suffix, e.g. `0`, `10m`, `2h`"
        )
    })
}

fn configured_grace(mur_home: &Path) -> Duration {
    let mins = mur_common::config::Config::load_or_default(&mur_home.join(CONFIG_FILE))
        .turn_track
        .stale_grace_mins;
    Duration::from_secs(mins.saturating_mul(60))
}

/// Where patches for `project` go: one directory per repository, named after
/// its checkout, so two projects' `turn-*` names never collide.
pub(crate) fn patch_dir(mur_home: &Path, project: &Path) -> PathBuf {
    let name = project
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "root".into());
    mur_home
        .join(ARTIFACTS_DIR)
        .join(PRUNE_ARTIFACT_DIR)
        .join(name)
}

fn reason_label(r: StaleReason) -> &'static str {
    match r {
        StaleReason::Released => "released",
        StaleReason::OwnerDead => "owner_dead",
        StaleReason::Legacy => "legacy",
    }
}

#[derive(serde::Serialize)]
struct Row {
    name: String,
    path: PathBuf,
    reason: &'static str,
    /// `reclaimed`, `would_reclaim`, or `failed`.
    status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    patch: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

pub(crate) fn prune(
    mur_home: &Path,
    dir: &Path,
    opts: &PruneOpts,
    out: &mut dyn Write,
) -> Result<()> {
    let project = project_root(dir)?;
    let stale = stale_tracks(&project, opts.grace)?;
    let patches = patch_dir(mur_home, &project);
    let rows: Vec<Row> = stale
        .iter()
        .map(|t| one(&project, t, &patches, opts.dry_run))
        .collect();

    if opts.json {
        serde_json::to_writer_pretty(&mut *out, &rows)?;
        writeln!(out)?;
    } else {
        print_table(out, &project, &rows, opts.dry_run)?;
    }
    if rows.iter().any(|r| r.status == "failed") {
        anyhow::bail!("some turn tracks could not be reclaimed");
    }
    Ok(())
}

fn one(project: &Path, t: &StaleTrack, patches: &Path, dry_run: bool) -> Row {
    let mut row = Row {
        name: t.name.clone(),
        path: t.path.clone(),
        reason: reason_label(t.reason),
        status: "would_reclaim",
        patch: None,
        error: None,
    };
    if dry_run {
        return row;
    }
    let patch = patches.join(format!("{}.patch", t.name));
    match reclaim(project, t, &patch) {
        Ok(()) => {
            row.status = "reclaimed";
            // An empty diff is nothing to point the user at.
            if std::fs::metadata(&patch).map(|m| m.len()).unwrap_or(0) > 0 {
                row.patch = Some(patch);
            }
        }
        Err(e) => {
            row.status = "failed";
            row.error = Some(format!("{e:#}"));
        }
    }
    row
}

fn print_table(out: &mut dyn Write, project: &Path, rows: &[Row], dry_run: bool) -> Result<()> {
    if rows.is_empty() {
        writeln!(out, "No stale turn tracks in {}.", project.display())?;
        return Ok(());
    }
    let verb = if dry_run {
        "Would reclaim"
    } else {
        "Reclaimed"
    };
    let done = rows.iter().filter(|r| r.status != "failed").count();
    writeln!(out, "{verb} {done} turn track(s) in {}:", project.display())?;
    for r in rows {
        write!(out, "  {:<12} {}", r.reason, r.name)?;
        match (&r.patch, &r.error) {
            (_, Some(e)) => writeln!(out, "  FAILED: {e}")?,
            (Some(p), _) => writeln!(out, "  → git apply --3way {}", p.display())?,
            _ => writeln!(out)?,
        }
    }
    if dry_run {
        writeln!(out, "Run again without --dry-run to reclaim them.")?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "track_tests.rs"]
mod tests;
