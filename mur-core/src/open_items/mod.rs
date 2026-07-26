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

pub use mur_common::open_items::{ItemSource, OpenItem};

pub mod observed;

/// The agent-authored half lives in `mur-common` so the agent runtime can
/// write to it without depending on this crate.
pub use mur_common::open_items as reported;

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

/// One line for a place that cannot afford a panel — a TUI turn boundary.
///
/// `None` when there is nothing open, because the alternative is telling the
/// user "0 open items" after every single turn, which is how a surface earns
/// its way into the part of the screen people stop reading.
pub fn summary_line(items: &[OpenItem]) -> Option<String> {
    if items.is_empty() {
        return None;
    }
    let observed = items
        .iter()
        .filter(|i| i.source == ItemSource::Observed)
        .count();
    let reported = items.len() - observed;
    let mut parts = Vec::new();
    if observed > 0 {
        parts.push(format!("{observed} observed"));
    }
    if reported > 0 {
        parts.push(format!("{reported} reported"));
    }
    Some(format!(
        "{} open item{} ({}) — /open",
        items.len(),
        if items.len() == 1 { "" } else { "s" },
        parts.join(", ")
    ))
}

/// Identity of a set of items, so a caller can tell "something changed" from
/// "the same three things are still open".
///
/// Deliberately excludes timestamps: an observed item is re-derived with a
/// fresh `at` on every collection, so hashing time would report a change every
/// turn and the end-of-turn notice would fire forever.
pub fn fingerprint(items: &[OpenItem]) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for it in items {
        it.title.hash(&mut h);
        it.origin.hash(&mut h);
        it.source.label().hash(&mut h);
    }
    h.finish()
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
    use chrono::{DateTime, Utc};

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

    /// Nothing open must produce no line at all. "0 open items" after every
    /// turn is how a surface trains people to stop reading it.
    #[test]
    fn summary_is_silent_when_nothing_is_open() {
        assert_eq!(summary_line(&[]), None);
    }

    #[test]
    fn summary_counts_each_source_separately() {
        let s = summary_line(&[
            item(ItemSource::Observed, "a", Utc::now()),
            item(ItemSource::Observed, "b", Utc::now()),
            item(ItemSource::Reported, "c", Utc::now()),
        ])
        .unwrap();
        assert!(s.contains("3 open items"), "{s}");
        assert!(s.contains("2 observed"), "{s}");
        assert!(s.contains("1 reported"), "{s}");
    }

    /// Observed items are re-derived with a fresh timestamp on every
    /// collection. If the fingerprint noticed that, the end-of-turn notice
    /// would fire on every turn forever and become noise.
    #[test]
    fn fingerprint_ignores_timestamps_so_unchanged_items_stay_quiet() {
        let a = vec![item(ItemSource::Observed, "same thing", Utc::now())];
        let b = vec![item(
            ItemSource::Observed,
            "same thing",
            Utc::now() - chrono::TimeDelta::days(3),
        )];
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }

    /// But a genuinely new item has to break the silence.
    #[test]
    fn fingerprint_changes_when_an_item_appears_or_changes_source() {
        let base = vec![item(ItemSource::Observed, "one", Utc::now())];
        let added = vec![
            item(ItemSource::Observed, "one", Utc::now()),
            item(ItemSource::Observed, "two", Utc::now()),
        ];
        assert_ne!(fingerprint(&base), fingerprint(&added));

        // Same title, different source = a different claim about the world.
        let reported = vec![item(ItemSource::Reported, "one", Utc::now())];
        assert_ne!(fingerprint(&base), fingerprint(&reported));
    }
}
