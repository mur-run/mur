//! Write a browser session's current storage state back to its profile.
//!
//! `mur browser auth` used to be the only writer of `state.json.age`, so
//! every replay and live session started from the login-time snapshot and
//! threw away whatever the site rotated afterwards (refreshed session
//! cookies, renewed web-storage tokens). On a site that rotates its token the
//! saved profile then went stale long before the account's session did.
//!
//! The browser exports its state into a private scratch directory under the
//! browser root (the sealed live server can write there and nowhere else),
//! and [`refresh_profile`] re-encrypts it — but only when the export still
//! carries every login key of the saved profile ([`session::retains_login`]);
//! leftover preferences or consent cookies are not proof of a login. A
//! logged-out or blank context must never overwrite a working login.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use std::time::Instant;

use crate::state::{StateKeyStore, Version};
use crate::{abandon, auth, paths, pending, session, split};

/// File name the browser writes its exported state to inside a scratch dir.
pub const EXPORT_FILE: &str = "storage-state.json";

/// How long a session-end export may take before it is abandoned. Together
/// with [`WRITEBACK_BUDGET`] it fits the runtime's MCP stop slice
/// (`mur-agent-runtime`'s `MCP_STOP_SLICE`, minus its `TERM_GRACE`), so an
/// export that finishes in time is never cut off by SIGTERM before it is
/// written back; `mur-core` asserts the sum.
pub const EXPORT_TIMEOUT: Duration = Duration::from_secs(6);

/// The whole write-back after the export — every profile's lock wait,
/// encryption and metadata write — shares this one budget; a profile whose
/// lock is not free before it runs out is abandoned, never waited out.
pub const WRITEBACK_BUDGET: Duration = Duration::from_secs(3);

/// How long a write-back outside an agent stop (replay) waits for a lock.
pub const WRITEBACK_LOCK_WAIT: Duration = Duration::from_secs(2);

/// What happened to a profile after a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The profile now holds the session's latest state.
    Refreshed,
    /// Left untouched, with the reason (no export, logged out, …).
    Skipped(String),
}

/// `async (page) => …` snippet for `browser_run_code_unsafe` that writes the
/// context's storage state to `state_path`. The path is serialised as a JS
/// string literal, never interpolated raw.
pub fn export_code(state_path: &Path) -> Result<String> {
    let path = serde_json::to_string(&state_path.to_string_lossy())?;
    Ok(format!(
        "async (page) => {{ await page.context().storageState({{ path: {path} }}); return 'storage state saved'; }}"
    ))
}

/// Arguments for the `browser_run_code_unsafe` export call.
pub fn export_arguments(state_path: &Path) -> Result<serde_json::Value> {
    Ok(json!({ "code": export_code(state_path)? }))
}

/// Ask a live Playwright MCP server to export its state to `path`. Best
/// effort and bounded: a failure only means this session is not written back.
pub async fn export_via(down: &crate::proxy::Downstream, path: &Path) {
    let result = async {
        let call = down.call_tool("browser_run_code_unsafe", export_arguments(path)?);
        let resp = tokio::time::timeout(EXPORT_TIMEOUT, call)
            .await
            .context("storage-state export timed out")??;
        crate::proxy::ensure_tool_succeeded("browser_run_code_unsafe", &resp).map(|_| ())
    }
    .await;
    if let Err(error) = result {
        tracing::info!(error = %format!("{error:#}"), "session-end storage-state export skipped");
    }
}

/// A private (0700) scratch directory under the browser root, removed on
/// drop, for one session's state export.
pub fn scratch_dir(mur_home: &Path) -> Result<tempfile::TempDir> {
    let parent = paths::browser_root(mur_home).join("tmp");
    std::fs::create_dir_all(&parent)
        .with_context(|| format!("create browser scratch dir {}", parent.display()))?;
    tempfile::Builder::new()
        .prefix("mur-browser-refresh-")
        .tempdir_in(&parent)
        .with_context(|| format!("create private export dir in {}", parent.display()))
}

/// Where the browser should write its export inside `dir`.
pub fn export_path(dir: &Path) -> PathBuf {
    dir.join(EXPORT_FILE)
}

/// Re-encrypt `site`'s profile from the export at `export`, if it is a
/// usable logged-in state. `last_auth` (the human login) is kept; the session
/// expiry and `last_refresh` are updated. The plaintext export is removed
/// whatever the outcome.
///
/// `loaded` is what the session loaded (its watermark and pending marker),
/// when it loaded through [`crate::pending::load`]: the write-back is then
/// refused if the profile changed since, and the marker is resolved. The
/// lock wait ends at `deadline`.
pub fn refresh_profile(
    mur_home: &Path,
    site: &str,
    loaded: Option<pending::Loaded>,
    export: &Path,
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
    deadline: Instant,
) -> Result<Outcome> {
    let result = refresh_inner(mur_home, site, loaded, export, now, store, deadline);
    let _ = std::fs::remove_file(export);
    result
}

#[allow(clippy::too_many_arguments)]
fn refresh_inner(
    mur_home: &Path,
    site: &str,
    loaded: Option<pending::Loaded>,
    export: &Path,
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
    deadline: Instant,
) -> Result<Outcome> {
    if !export.is_file() {
        return Ok(Outcome::Skipped("the browser exported no state".into()));
    }
    let state = std::fs::read(export)
        .with_context(|| format!("read exported browser state {}", export.display()))?;
    locked_write_back(mur_home, site, loaded, &state, now, store, deadline)
}

/// Take `site`'s lock by `deadline` and write `state` back under it. A lock
/// timeout keeps the session's marker, tagged, for the next use to report.
#[allow(clippy::too_many_arguments)]
fn locked_write_back(
    mur_home: &Path,
    site: &str,
    loaded: Option<pending::Loaded>,
    state: &[u8],
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
    deadline: Instant,
) -> Result<Outcome> {
    let Some(held) = abandon::lock(mur_home, site, deadline)? else {
        if let Some(l) = loaded {
            l.marker.keep(pending::Kept::LockTimeout);
        }
        return Ok(Outcome::Skipped(
            "the profile was locked by another writer".into(),
        ));
    };
    let result = write_back(
        mur_home,
        site,
        loaded.as_ref().map(|l| &l.version),
        state,
        now,
        store,
        &held,
    );
    // Resolved while the lock is still held (spec "Scope").
    match (&result, loaded) {
        (Err(_), Some(l)) => l.marker.keep(pending::Kept::Interrupted),
        (_, l) => drop(l),
    }
    drop(held);
    result
}

/// Write `state` back to `site` under its held lock, if it still holds the
/// saved login and the profile is still the `watermark` the session loaded.
fn write_back(
    mur_home: &Path,
    site: &str,
    watermark: Option<&Version>,
    state: &[u8],
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
    held: &abandon::ProfileLock,
) -> Result<Outcome> {
    let meta_path = paths::profile_meta(mur_home, site);
    let yaml = std::fs::read_to_string(&meta_path)
        .with_context(|| format!("read browser profile metadata {}", meta_path.display()))?;
    let mut meta: auth::ProfileMeta = serde_yaml::from_str(&yaml)
        .with_context(|| format!("parse browser profile metadata {}", meta_path.display()))?;
    auth::validate_storage_state(state)?;
    let state_path = paths::profile_state(mur_home, site);
    let (saved, current) = crate::state::read_versioned(&state_path, store)
        .context("read the saved profile state to compare logins")?;
    if watermark.is_some_and(|w| *w != current) {
        let entry = abandon::Abandoned {
            at: now,
            reason: abandon::Reason::NewerCopy,
            host: None,
            other: None,
        };
        abandon::record_locked(mur_home, site, held, entry)?;
        return Ok(Outcome::Skipped(
            "the profile changed since the session loaded it; the newer copy was kept".into(),
        ));
    }
    let saved_at = meta.last_refresh.unwrap_or(meta.last_auth);
    if !session::retains_login(&saved, saved_at, state, &meta.allow_domains, now) {
        return Ok(Outcome::Skipped(
            "the session no longer holds the saved login (logged out?)".into(),
        ));
    }
    crate::state::write_state(&state_path, state, store)?;
    meta.earliest_cookie_expires = session::session_cookie_expiry(state, &meta.allow_domains, now)?;
    meta.last_refresh = Some(now);
    auth::write_meta(&meta_path, &meta)?;
    Ok(Outcome::Refreshed)
}

/// Write a merged session's export back to each of its profiles (#2360).
///
/// The export is split per profile by host ([`split::split_state`]). Each
/// part goes through the same checks as a single-profile write-back, one
/// profile at a time, each under its own lock (never two at once). Every
/// item that matched several profiles was written to none; each profile it
/// matched gets a `writeback_abandoned` record naming the host and the other
/// profile — whether or not the rest of its write-back succeeded — so the
/// lost login is reported on next use. The plaintext export is removed
/// whatever the outcome.
///
/// `loaded` holds what the session loaded per profile (see
/// [`refresh_profile`]); every lock wait ends at `deadline`, so a busy
/// first profile cannot eat the time of the next.
pub fn refresh_profiles(
    mur_home: &Path,
    targets: &[split::Target<'_>],
    loaded: Vec<pending::Loaded>,
    export: &Path,
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
    deadline: Instant,
) -> Vec<(String, Result<Outcome>)> {
    let mut loaded: BTreeMap<String, pending::Loaded> =
        loaded.into_iter().map(|l| (l.site.clone(), l)).collect();
    let result =
        refresh_profiles_inner(mur_home, targets, &mut loaded, export, now, store, deadline);
    let _ = std::fs::remove_file(export);
    match result {
        Ok(each) => each,
        Err(error) => {
            // Nothing was written back: the next use must hear about it.
            for (_, l) in loaded {
                l.marker.keep(pending::Kept::Interrupted);
            }
            let msg = format!("{error:#}");
            targets
                .iter()
                .map(|t| (t.site.to_owned(), Err(anyhow::anyhow!(msg.clone()))))
                .collect()
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn refresh_profiles_inner(
    mur_home: &Path,
    targets: &[split::Target<'_>],
    loaded: &mut BTreeMap<String, pending::Loaded>,
    export: &Path,
    now: DateTime<Utc>,
    store: &impl StateKeyStore,
    deadline: Instant,
) -> Result<Vec<(String, Result<Outcome>)>> {
    if !export.is_file() {
        return Ok(targets
            .iter()
            .map(|t| {
                let why = Outcome::Skipped("the browser exported no state".into());
                (t.site.to_owned(), Ok(why))
            })
            .collect());
    }
    let state = std::fs::read(export)
        .with_context(|| format!("read exported browser state {}", export.display()))?;
    let split = split::split_state(&state, targets)?;
    drop(state);
    for o in &split.overlaps {
        tracing::warn!(
            host = %o.host,
            kind = o.kind.as_str(),
            name = o.name.as_deref().unwrap_or(""),
            profiles = %o.sites.join(","),
            "session item matched several browser profiles; written to none"
        );
    }
    if split.unmatched > 0 {
        tracing::info!(
            count = split.unmatched,
            "session items matched no browser profile; dropped"
        );
    }
    let mut out = Vec::with_capacity(targets.len());
    for t in targets {
        let site = t.site;
        let mine = loaded.remove(site);
        let Some(held) = abandon::lock(mur_home, site, deadline)? else {
            if let Some(l) = mine {
                l.marker.keep(pending::Kept::LockTimeout);
            }
            out.push((
                site.to_owned(),
                Ok(Outcome::Skipped(
                    "the profile was locked by another writer".into(),
                )),
            ));
            continue;
        };
        let watermark = mine.as_ref().map(|l| &l.version);
        let outcome = write_back(
            mur_home,
            site,
            watermark,
            &split.parts[site],
            now,
            store,
            &held,
        );
        if let Some(o) = split.overlaps_for(site).next() {
            let entry = abandon::Abandoned {
                at: now,
                reason: abandon::Reason::Overlap,
                host: Some(o.host.clone()),
                other: o.sites.iter().find(|s| *s != site).cloned(),
            };
            if let Err(error) = abandon::record_locked(mur_home, site, &held, entry) {
                tracing::warn!(site, error = %format!("{error:#}"), "could not record a dropped browser login");
            }
        }
        match (&outcome, mine) {
            (Err(_), Some(l)) => l.marker.keep(pending::Kept::Interrupted),
            (_, l) => drop(l),
        }
        drop(held);
        out.push((site.to_owned(), outcome));
    }
    Ok(out)
}

/// Log a refresh result; a failed refresh never fails the session itself.
pub fn report(site: &str, result: Result<Outcome>) {
    match result {
        Ok(Outcome::Refreshed) => tracing::info!(site, "browser profile refreshed"),
        Ok(Outcome::Skipped(why)) => {
            tracing::info!(site, reason = %why, "browser profile not refreshed")
        }
        Err(error) => {
            tracing::warn!(site, error = %format!("{error:#}"), "browser profile refresh failed")
        }
    }
}

#[cfg(test)]
#[path = "refresh_tests.rs"]
mod tests;
