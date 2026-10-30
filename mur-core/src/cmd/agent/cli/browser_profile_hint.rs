//! Hint for #2100: the `browser` entry launches with no `--profile`, while
//! `mur browser auth` has a saved session for a host the entry may reach.
//!
//! `/browser allow` only edits the host allowlist and `mur browser setup`
//! creates the entry sessionless, so a user can end up with a reachable but
//! logged-out site and nothing saying why. Only `/browser live … --profile`
//! writes the profile into the launch argv (`browser_live_cmd::live_argv`);
//! this module points there, it never attaches a profile by itself.

use std::path::Path;

use mur_common::agent::McpServerEntry;
use mur_common::proposal::Proposal;

pub(super) use crate::cmd::browser::live_entry::attached_profile;
use crate::cmd::browser::live_entry::{norm, within};

/// One saved `mur browser auth` profile and the hosts it was made for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SavedProfile {
    pub site: String,
    /// The profile's `allow_domains`, or its login URL's host when it has
    /// none (the same default `mur browser auth` applies).
    pub domains: Vec<String>,
}

/// Every complete profile under `<home>/browser/profiles`. A profile with no
/// encrypted state, or whose metadata does not parse, is skipped: it cannot
/// be attached, so it must not be suggested. Best-effort by design — a hint
/// must never turn a successful command into an error.
pub(super) fn load_saved_profiles(home: &Path) -> Vec<SavedProfile> {
    let root = mur_browser::paths::browser_root(home).join("profiles");
    let Ok(dir) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    dir.filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|site| mur_browser::paths::validate_name(site).is_ok())
        .filter(|site| mur_browser::paths::profile_state(home, site).is_file())
        .filter_map(|site| {
            let yaml =
                std::fs::read_to_string(mur_browser::paths::profile_meta(home, &site)).ok()?;
            let meta: mur_browser::auth::ProfileMeta = serde_yaml::from_str(&yaml).ok()?;
            let domains = if meta.allow_domains.is_empty() {
                vec![url::Url::parse(&meta.url).ok()?.host_str()?.to_owned()]
            } else {
                meta.allow_domains
            };
            Some(SavedProfile {
                site,
                domains: domains.iter().map(|d| norm(d)).collect(),
            })
        })
        .collect()
}

/// Sites whose domains overlap any allowed host, in `profiles` order. Either
/// direction counts: an allowed `deepseek.com` and a profile for
/// `chat.deepseek.com` belong to the same login.
pub(super) fn matching<'a>(profiles: &'a [SavedProfile], hosts: &[String]) -> Vec<&'a str> {
    profiles
        .iter()
        .filter(|p| {
            hosts.iter().any(|h| {
                let h = norm(h.strip_prefix("*.").unwrap_or(h));
                p.domains.iter().any(|d| within(&h, d) || within(d, &h))
            })
        })
        .map(|p| p.site.as_str())
        .collect()
}

/// Text plus, when exactly one profile matches, a chip for its command.
#[derive(Debug)]
pub(super) struct Hint {
    pub text: String,
    pub chip: Option<Proposal>,
}

/// The command names every host already allowed, so following it keeps every
/// grant even when the user adds `--replace` (#2101). The chip
/// runs the preview form (no `--yes`): the user still sees the change.
fn live_command(hosts: &[String], site: &str) -> String {
    format!("browser live {} --profile {site}", hosts.join(" "))
}

/// The #2100 hint, or `None` when a profile is attached already or no saved
/// profile belongs to any allowed host.
pub(super) fn hint(
    entry: &McpServerEntry,
    hosts: &[String],
    profiles: &[SavedProfile],
) -> Option<Hint> {
    if attached_profile(entry).is_some() || hosts.is_empty() {
        return None;
    }
    let sites = matching(profiles, hosts);
    let lead = "note: the browser launches with no saved profile, so it carries no cookies \
                and sites behind a login show their logged-out page.";
    match sites.as_slice() {
        [] => None,
        [site] => {
            let cmd = live_command(hosts, site);
            let args = serde_json::json!({
                "label": format!("sign the browser in as `{site}`"),
                "kind": "slash",
                "command": cmd,
            });
            Some(Hint {
                text: format!(
                    "{lead} `mur browser auth` saved `{site}` for this site — attach it with `/{cmd}`"
                ),
                chip: mur_common::proposal::vet(&args).ok(),
            })
        }
        many => Some(Hint {
            text: format!(
                "{lead} Saved profiles for these hosts: {}. Attach one with:\n  {}",
                many.iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", "),
                many.iter()
                    .map(|s| format!("/{}", live_command(hosts, s)))
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ),
            chip: None,
        }),
    }
}

/// `/browser status` line: which saved session the browser launches with.
pub(super) fn status_line(entry: &McpServerEntry) -> String {
    match attached_profile(entry) {
        Some(site) => format!("browser profile: `{site}`"),
        None => "browser profile: none (launches with no cookies)".to_owned(),
    }
}

#[cfg(test)]
#[path = "browser_profile_hint_tests.rs"]
mod tests;
