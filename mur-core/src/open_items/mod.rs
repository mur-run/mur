//! Open items: what is still outstanding, and who says so.
//!
//! Two kinds of claim, never mixed:
//!
//! - [`ItemSource::Observed`] — derived from state MUR itself holds (a queued
//!   fleet job, a proposal in the inbox). These cannot be wrong about
//!   *existence*; the file is there or it is not.
//! - [`ItemSource::Reported`] — written by an agent because it decided
//!   something was left undone. Useful, because it catches promises made in
//!   conversation that no file records — and unverifiable, because nothing but
//!   the agent's word says the item is real.
//!
//! The display keeps them apart. A panel that presents both as equally certain
//! teaches the user to distrust the whole panel, which is worse than not
//! having one: the failure mode of a status surface is not being wrong once,
//! it is being ignored forever after.
//!
//! Deliberately NOT a source: unmerged local git branches. In a squash-merge
//! repo `git branch --no-merged` reports branches whose content is already in
//! main, so it manufactures false positives at a steady rate.

use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub mod observed;
pub mod reported;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSource {
    /// Derived from MUR's own state.
    Observed,
    /// Asserted by an agent.
    Reported,
}

impl ItemSource {
    /// Short marker for dense displays (a TUI footer, a status line).
    pub fn marker(&self) -> &'static str {
        match self {
            ItemSource::Observed => "●",
            ItemSource::Reported => "○",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ItemSource::Observed => "observed",
            ItemSource::Reported => "reported",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenItem {
    /// One line, imperative where possible.
    pub title: String,
    /// The command or place that resolves it, when there is an obvious one.
    pub next: Option<String>,
    pub source: ItemSource,
    /// Where it came from: `"inbox"`, `"fleet:acme"`, `"agent:mur"`.
    pub origin: String,
    pub at: DateTime<Utc>,
}

/// Everything outstanding, observed first.
///
/// Ordering is the policy: facts before assertions, so a reader who stops
/// after the first few lines has read the reliable ones.
pub fn collect(mur_home: &Path) -> Vec<OpenItem> {
    let mut items = observed::collect(mur_home);
    items.extend(reported::open(mur_home));
    items.sort_by(|a, b| a.source.cmp(&b.source).then(b.at.cmp(&a.at)));
    items
}

impl PartialOrd for ItemSource {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ItemSource {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        fn rank(s: &ItemSource) -> u8 {
            match s {
                ItemSource::Observed => 0,
                ItemSource::Reported => 1,
            }
        }
        rank(self).cmp(&rank(other))
    }
}

/// Render for a terminal. Groups by source with the marker legend, because an
/// unlabelled mixed list is the thing this module exists to avoid.
pub fn render(items: &[OpenItem]) -> String {
    if items.is_empty() {
        return "No open items.\n".into();
    }
    let mut out = String::new();
    let mut last: Option<ItemSource> = None;
    for it in items {
        if last != Some(it.source) {
            let caveat = match it.source {
                ItemSource::Observed => "from MUR's own state",
                ItemSource::Reported => "an agent said so, unverified",
            };
            out.push_str(&format!(
                "\n{} {} — {caveat}\n",
                it.source.marker(),
                it.source.label()
            ));
            last = Some(it.source);
        }
        out.push_str(&format!("  {} [{}]\n", it.title, it.origin));
        if let Some(next) = &it.next {
            out.push_str(&format!("      → {next}\n"));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(source: ItemSource, title: &str, at: DateTime<Utc>) -> OpenItem {
        OpenItem {
            title: title.into(),
            next: None,
            source,
            origin: "t".into(),
            at,
        }
    }

    /// Facts before assertions: a reader who stops after two lines must have
    /// read the two that cannot be wrong about their own existence.
    #[test]
    fn observed_items_sort_before_reported_regardless_of_age() {
        let old = Utc::now() - chrono::TimeDelta::days(30);
        let new = Utc::now();
        let mut v = vec![
            item(ItemSource::Reported, "agent said so", new),
            item(ItemSource::Observed, "queued job", old),
        ];
        v.sort_by(|a, b| a.source.cmp(&b.source).then(b.at.cmp(&a.at)));
        assert_eq!(v[0].title, "queued job");
    }

    /// The whole point of the feature is that the reader can tell which is
    /// which without being told twice.
    #[test]
    fn render_labels_both_sources() {
        let out = render(&[
            item(ItemSource::Observed, "a", Utc::now()),
            item(ItemSource::Reported, "b", Utc::now()),
        ]);
        assert!(out.contains("observed"), "{out}");
        assert!(out.contains("reported"), "{out}");
        assert!(out.contains("unverified"), "{out}");
    }

    #[test]
    fn empty_says_so_rather_than_printing_a_bare_header() {
        assert_eq!(render(&[]), "No open items.\n");
    }
}
