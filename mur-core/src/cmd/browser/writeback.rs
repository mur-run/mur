//! Session-end write-back of a live session's login state (#2360).
//!
//! One profile: the export goes back whole (`refresh_profile`). Several: it
//! is split per profile by host (`refresh_profiles`), and anything two
//! profiles both cover is dropped from both and reported on next use
//! (`mur_browser::abandon`), instead of the merged session not being written
//! back at all.

use std::path::Path;

use anyhow::Result;
use mur_browser::{abandon, paths, refresh, split, state::KeychainStateKeyStore};

use super::bind_plan::profile_domains;
use super::read_profile_meta;

/// Profiles a session will write back to, with the domains its split uses.
/// A profile whose metadata names no usable domain cannot be split into and
/// is left out (logged); the bind-time check normally refuses such a profile.
pub(super) fn targets(mur_home: &Path, profiles: &[String]) -> Vec<(String, Vec<String>)> {
    profiles
        .iter()
        .filter_map(|site| {
            let domains = read_profile_meta(&paths::profile_meta(mur_home, site))
                .ok()
                .and_then(|m| profile_domains(&m));
            if domains.is_none() {
                tracing::warn!(
                    site,
                    "browser profile names no domain; its session is not written back"
                );
            }
            Some((site.clone(), domains?))
        })
        .collect()
}

/// Write `export` back to every target, logging each result.
pub(super) fn write_back(mur_home: &Path, targets: &[(String, Vec<String>)], export: &Path) {
    let now = chrono::Utc::now();
    match targets {
        [] => {
            let _ = std::fs::remove_file(export);
        }
        [(site, _)] => refresh::report(
            site,
            refresh::refresh_profile(mur_home, site, export, now, &KeychainStateKeyStore),
        ),
        many => {
            let t: Vec<split::Target<'_>> = many
                .iter()
                .map(|(site, domains)| split::Target { site, domains })
                .collect();
            for (site, result) in
                refresh::refresh_profiles(mur_home, &t, export, now, &KeychainStateKeyStore)
            {
                refresh::report(&site, result);
            }
        }
    }
}

/// Pending notices for `profiles` (show-once): each is logged at session
/// start so it reaches the agent log, and returned for the caller.
pub(super) fn startup_notices(mur_home: &Path, profiles: &[String]) -> Vec<String> {
    let notices = abandon::take_notices(mur_home, profiles);
    for n in &notices {
        tracing::warn!("{n}");
    }
    notices
}

/// `mur browser status` suffix for a profile with an unshown notice.
pub(super) fn status_suffix(mur_home: &Path, site: &str) -> Result<Option<&'static str>> {
    Ok(abandon::pending(mur_home, site)?
        .map(|_| " — last session's login not saved; shown on next use"))
}
