//! #2101: the parts of `/browser` that run before (or instead of) a turn.
//!
//! - `/browser list [keyword]... [--tag <t>]...` — the `mur browser list`
//!   table, in-session, with free-text keywords (item 1).
//! - `--profile <site>` on `testing` / `automation` (and any other skill
//!   turn), or the one saved profile whose domains cover the URL in the
//!   request — so a saved login is usable without a detour through
//!   `/browser live` (item 3).
//! - A host pre-check: a URL whose host the `browser` entry may not reach is
//!   asked about before the turn starts, with the same 這次放行 / 永久放行 /
//!   不用 chips a mid-run `egress/denied` gets (item 4).
//!
//! Everything here is pure over plain inputs; `browser_cmd::handle` does the
//! I/O and the App updates.

use std::collections::HashSet;
use std::path::Path;

use chrono::{DateTime, Utc};
use mur_common::agent::{McpNetMode, McpServerEntry};

use super::browser_live_cmd::{LIVE_ENTRY, strip_url};
use super::browser_profile_hint::{SavedProfile, matching};
use super::egress_denied::DISMISS_PREFIX;
use super::suggest::Suggestion;
use crate::cmd::browser::list::{self, Filter, Format, Sort};

/// Flag that names a saved `mur browser auth` profile.
pub(super) const PROFILE_FLAG: &str = "--profile";
const TAG_FLAG: &str = "--tag";
pub(super) const LIST_USAGE: &str = "usage: /browser list [keyword]... [--tag <tag>]...";
/// Newest first: the usual reason to list is "find the run I just made".
const LIST_SORT: Sort = Sort::Recent;

// ── item 1: /browser list ───────────────────────────────────────────────────

#[derive(Debug, Default, PartialEq, Eq)]
pub(super) struct ListArgs {
    /// Every keyword must appear (case-insensitive) in a run's name,
    /// description, tags, domain, or step intents.
    pub keywords: Vec<String>,
    /// Every tag must be on the run, exactly as `mur browser list --tag`.
    pub tags: Vec<String>,
}

pub(super) fn parse_list(args: &[String]) -> Result<ListArgs, String> {
    let mut out = ListArgs::default();
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == TAG_FLAG {
            let tag = it
                .next()
                .ok_or_else(|| format!("`{TAG_FLAG}` needs a tag — {LIST_USAGE}"))?;
            out.tags.push(tag.clone());
        } else if a.starts_with("--") {
            return Err(format!("unknown flag `{a}` — {LIST_USAGE}"));
        } else {
            out.keywords.push(a.to_lowercase());
        }
    }
    Ok(out)
}

/// The recordings table for `runs` (the `<home>/browser/runs` directory).
pub(super) fn render_list(runs: &Path, args: &ListArgs, now: DateTime<Utc>) -> String {
    let all = list::load_rows(runs);
    if all.is_empty() {
        return "no browser recordings yet — `/browser testing` or `/browser automation` \
                records one"
            .to_owned();
    }
    let total = all.len();
    let filter = Filter {
        tags: args.tags.clone(),
        ..Filter::default()
    };
    let rows: Vec<_> = list::apply(all, &filter, LIST_SORT, now)
        .into_iter()
        .filter(|r| {
            let hay = r.search_text.to_lowercase();
            args.keywords.iter().all(|k| hay.contains(k.as_str()))
        })
        .collect();
    if rows.is_empty() {
        return format!("no recordings match — {total} recorded; `/browser list` shows them all");
    }
    format!(
        "{}\nreplay one: `/browser automation replay <name>`",
        list::render(&rows, Format::Table, true)
    )
}

// ── item 3: --profile on testing / automation ───────────────────────────────

/// A skill turn's arguments with `--profile` taken out, and the profile it
/// will run as.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct TurnPlan {
    pub args: Vec<String>,
    pub profile: Option<String>,
    /// Shown in the transcript before the turn: why this profile (or why
    /// none was picked when several match).
    pub note: Option<String>,
}

/// Hosts of every `http(s)://` URL in `args`. Bare words are never read as
/// hosts here: a turn's text is a request, not a host list.
pub(super) fn url_hosts(args: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in args.iter().filter(|a| a.contains("://")) {
        if strip_url(a).is_err() {
            continue;
        }
        let host = url::Url::parse(a)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.trim_end_matches('.').to_owned()));
        if let Some(h) = host
            && !out.contains(&h)
        {
            out.push(h);
        }
    }
    out
}

fn names(saved: &[SavedProfile]) -> String {
    if saved.is_empty() {
        return "none saved — `/browser auth` records one".to_owned();
    }
    saved
        .iter()
        .map(|p| format!("`{}`", p.site))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Split out `--profile <site>` (it must be a saved profile), or pick the one
/// saved profile that covers a URL host in the request. Several matches pick
/// none and say which: guessing a login is worse than asking.
pub(super) fn plan_turn(args: &[String], saved: &[SavedProfile]) -> Result<TurnPlan, String> {
    let mut rest = Vec::new();
    let mut explicit: Option<String> = None;
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a != PROFILE_FLAG {
            rest.push(a.clone());
            continue;
        }
        let site = it.next().ok_or_else(|| {
            format!(
                "`{PROFILE_FLAG}` needs a saved profile name ({})",
                names(saved)
            )
        })?;
        if explicit.replace(site.clone()).is_some() {
            return Err(format!(
                "`{PROFILE_FLAG}` was given twice; name one profile"
            ));
        }
    }
    if let Some(site) = explicit {
        if !saved.iter().any(|p| p.site == site) {
            return Err(format!(
                "no saved browser profile `{site}` — saved: {}",
                names(saved)
            ));
        }
        return Ok(TurnPlan {
            args: rest,
            profile: Some(site),
            note: None,
        });
    }
    let hosts = url_hosts(&rest);
    let (profile, note) = match matching(saved, &hosts).as_slice() {
        [] => (None, None),
        [site] => (
            Some((*site).to_owned()),
            Some(format!(
                "signing the browser in as the saved profile `{site}` (it covers {}) — \
                 `{PROFILE_FLAG} <site>` picks another",
                hosts.join(", ")
            )),
        ),
        many => (
            None,
            Some(format!(
                "several saved profiles cover {}: {} — add `{PROFILE_FLAG} <site>` to pick one; \
                 running without a login",
                hosts.join(", "),
                many.iter()
                    .map(|s| format!("`{s}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            )),
        ),
    };
    Ok(TurnPlan {
        args: rest,
        profile,
        note,
    })
}

/// The turn text the browser skill receives.
pub(super) fn instruction(plan: &TurnPlan) -> String {
    let mut out = if plan.args.is_empty() {
        "Use the browser skill.".to_owned()
    } else {
        format!("Use the browser skill. {}", plan.args.join(" "))
    };
    if let Some(site) = &plan.profile {
        out.push_str(&format!(
            "\n\nUse the saved login profile `{site}`: pass `{PROFILE_FLAG} {site}` to \
             `mur browser record` and `mur browser replay`. Do not ask the user to switch to \
             `/browser live` for it."
        ));
    }
    out
}

// ── item 4: ask before a blocked host is dispatched ─────────────────────────

/// URL hosts the `browser` entry may not reach. Only a host-scoped
/// (`Restricted`) entry is checked: with no entry there is nothing to scope,
/// and research mode reaches everything but its deny list, which is never
/// grantable. `skip` holds hosts already allowed this session or dismissed.
pub(super) fn blocked_hosts(
    hosts: &[String],
    entry: Option<&McpServerEntry>,
    skip: &HashSet<String>,
) -> Vec<String> {
    let Some(net) = entry.and_then(|e| e.network.as_ref()) else {
        return Vec::new();
    };
    if net.mode != McpNetMode::Restricted {
        return Vec::new();
    }
    hosts
        .iter()
        .filter(|h| !skip.contains(*h))
        .filter(|h| !mur_common::net::host_allowed(h, &net.allow_hosts))
        .cloned()
        .collect()
}

/// The question and its chips for the first blocked host. The rest are
/// named so the user is not surprised by a second prompt.
pub(super) fn allow_prompt(blocked: &[String]) -> Option<(String, Vec<Suggestion>)> {
    let (host, more) = blocked.split_first()?;
    let mut text = format!(
        "`{host}` is not allowed for `{LIVE_ENTRY}` — allow it before the run starts? \
         Pick below, then send the command again (↑ recalls it)."
    );
    if !more.is_empty() {
        text.push_str(&format!(" Also not allowed: {}.", more.join(", ")));
    }
    let item = |text: String, desc: &str| Suggestion {
        text,
        desc: Some(desc.to_owned()),
    };
    Some((
        text,
        vec![
            item(
                format!("/browser allow --session {host}"),
                "這次放行 — this session only, nothing saved",
            ),
            item(
                format!("/browser allow {host}"),
                "永久放行 — preview, then save to the profile",
            ),
            item(
                format!("{DISMISS_PREFIX}{host}"),
                "不用 — keep it blocked; sending again runs anyway",
            ),
        ],
    ))
}

#[cfg(test)]
#[path = "browser_mode_cmd_tests.rs"]
mod tests;
