//! "Log in again" from the card (S:§The card): a headed login whose save
//! is triggered by the card's "logged in" action (D3), published under the
//! profile lock like every other profile write (`save_profile_by`).

use std::future::Future;
use std::path::Path;
use std::time::Instant;

use anyhow::{Result, bail};
use chrono::{DateTime, Utc};
use mur_browser::auth::{Handoff, ProfileMeta, save_profile_by};
use mur_browser::paths;
use mur_browser::state::StateKeyStore;
use mur_browser_request::Site;

use super::super::bind_plan::profile_domains;
use super::super::live_entry::{norm, within};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LoginPlan {
    pub profile: String,
    pub url: String,
    /// Normalised hosts, as `profile_domains` would read them back.
    pub allow_domains: Vec<String>,
    /// A profile of this name exists and `--reauth` was given (D6).
    pub replace: bool,
}

/// The host, plus `-<port>` for a non-default port (D6), with anything a
/// profile directory name may not hold made `-`. An IPv6 host's `:` become
/// `_`, so `[::1]:8081` is `__1-8081`, not a bare `1-8081`.
pub(crate) fn default_name(site: &Site) -> String {
    let host = site.host().trim_matches(['[', ']']).replace(':', "_");
    let raw = match site.port() {
        Some(p) => format!("{host}-{p}"),
        None => host,
    };
    let safe: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                c
            } else {
                '-'
            }
        })
        .collect();
    let safe: String = safe
        .trim_matches(|c| c == '-' || c == '.')
        .chars()
        .take(64)
        .collect();
    if safe.is_empty() { "site".into() } else { safe }
}

/// Decided before the browser opens, so a bad name or an unconfirmed
/// replace never costs a sign-in. A replace keeps the old profile's hosts
/// and adds the request host only if none of them covers it (D6).
pub(crate) fn login_plan(
    home: &Path,
    site: &Site,
    name: Option<&str>,
    reauth: bool,
) -> Result<LoginPlan> {
    let profile = name.map_or_else(|| default_name(site), str::to_owned);
    paths::validate_name(&profile)?;
    let host = norm(site.host());
    let exists = paths::profile_state(home, &profile).exists();
    if exists && !reauth {
        bail!(
            "a saved login named {profile:?} exists: add --reauth to replace it, \
             or --name to save under another name"
        );
    }
    let mut allow_domains = if exists {
        let meta = super::super::read_profile_meta(&paths::profile_meta(home, &profile))?;
        profile_domains(&meta).unwrap_or_default()
    } else {
        Vec::new()
    };
    if !allow_domains.iter().any(|d| within(&host, d)) {
        allow_domains.push(host);
    }
    Ok(LoginPlan {
        replace: exists,
        url: format!("{}/", site.origin()),
        allow_domains,
        profile,
    })
}

/// Encrypt and publish a captured state. The card's "logged in" is the
/// human confirmation `Handoff` requires; schema validation in the save is
/// the verification, as in `mur browser auth`.
pub(crate) fn save_login(
    home: &Path,
    plan: &LoginPlan,
    state: &[u8],
    store: &impl StateKeyStore,
    now: DateTime<Utc>,
    deadline: Instant,
) -> Result<ProfileMeta> {
    let mut h = Handoff::new();
    h.start()?;
    h.handoff()?;
    h.continue_after_login()?;
    h.verified()?;
    save_profile_by(
        &mut h,
        &paths::profile_state(home, &plan.profile),
        &paths::profile_meta(home, &plan.profile),
        state,
        &plan.url,
        &plan.allow_domains,
        now,
        store,
        plan.replace,
        deadline,
    )
}

/// Open the login page, wait for `logged_in`, save. `logged_in` resolving
/// to an error (the card closed) cancels and writes nothing.
pub(crate) async fn login(
    home: &Path,
    plan: &LoginPlan,
    browser: &str,
    logged_in: impl Future<Output = Result<()>>,
) -> Result<ProfileMeta> {
    let state = mur_browser::proxy::capture_storage_state_on(&plan.url, browser, logged_in).await?;
    save_login(
        home,
        plan,
        &state,
        &mur_browser::state::KeychainStateKeyStore,
        Utc::now(),
        Instant::now() + mur_browser::abandon::LOCK_WAIT,
    )
}

#[cfg(test)]
#[path = "login_tests.rs"]
mod tests;
