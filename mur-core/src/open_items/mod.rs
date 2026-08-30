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

pub use mur_open_items::{ItemSource, OpenItem};

pub mod observed;

/// The agent-authored half lives in its own crate so the agent runtime can
/// write to it without depending on this one.
pub use mur_open_items as reported;

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

/// Split `items` by the mute list.
///
/// Returns the items to show, and the muted origins that actually matched
/// something — the footer names what the reader would otherwise have seen,
/// not what the config happens to contain, so a stale mute stays quiet.
pub fn partition(items: Vec<OpenItem>, muted: &[String]) -> (Vec<OpenItem>, Vec<String>) {
    let mut hidden: Vec<String> = Vec::new();
    let visible: Vec<OpenItem> = items
        .into_iter()
        .filter(|it| {
            // Exact match, never prefix: `fleet` must not swallow `fleet:acme`.
            if muted.iter().any(|m| m == &it.origin) {
                if !hidden.contains(&it.origin) {
                    hidden.push(it.origin.clone());
                }
                false
            } else {
                true
            }
        })
        .collect();
    hidden.sort();
    (visible, hidden)
}

/// One line for a place that cannot afford a panel — a TUI turn boundary.
///
/// `None` when there is nothing open, because the alternative is telling the
/// user "0 open items" after every single turn, which is how a surface earns
/// its way into the part of the screen people stop reading.
/// Split off the items that have aged out of the default view.
///
/// Returns `(fresh, stale)` preserving `collect`'s ordering within each half.
pub fn split_stale(
    items: Vec<OpenItem>,
    now: chrono::DateTime<chrono::Utc>,
) -> (Vec<OpenItem>, Vec<OpenItem>) {
    items.into_iter().partition(|i| !i.is_stale_at(now))
}

pub fn summary_line(items: &[OpenItem], stale: usize) -> Option<String> {
    if items.is_empty() {
        // Stale items are demoted, not gone. Dropping the line entirely here
        // would make them invisible, which is worse than the accumulation
        // this ageing exists to fix.
        return (stale > 0).then(|| format!("{stale} stale — mur open --all"));
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
        "{} open item{} ({}){} — /open",
        items.len(),
        if items.len() == 1 { "" } else { "s" },
        parts.join(", "),
        if stale > 0 {
            format!(" · {stale} stale")
        } else {
            String::new()
        }
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
pub fn render(items: &[OpenItem], muted: &[String], stale: usize) -> String {
    let mut out = if items.is_empty() {
        "No open items.\n".to_string()
    } else {
        let mut s = String::new();
        let mut last: Option<ItemSource> = None;
        for it in items {
            if last != Some(it.source) {
                let caveat = match it.source {
                    ItemSource::Observed => "from MUR's own state",
                    ItemSource::Reported => "an agent said so, unverified",
                };
                s.push_str(&format!(
                    "\n{} {} — {caveat}\n",
                    it.source.marker(),
                    it.source.label()
                ));
                last = Some(it.source);
            }
            s.push_str(&format!("  {} [{}]\n", it.title, it.origin));
            if let Some(next) = &it.next {
                s.push_str(&format!("      → {next}\n"));
            }
        }
        s
    };

    // Collapsed, never hidden. This one line is what makes a permanent mute
    // safe: the reader never has to wonder whether something is missing, so
    // the real trade is one line versus N — not show versus hide.
    if !muted.is_empty() {
        out.push_str(&format!(
            "\n{} source{} muted ({}) — mur open --all\n",
            muted.len(),
            if muted.len() == 1 { "" } else { "s" },
            muted.join(", ")
        ));
    }
    // Same trade as the mute footer above: one line versus N, never
    // show-versus-hide. A reader must not have to wonder what aged out.
    if stale > 0 {
        out.push_str(&format!(
            "\n{stale} stale item{} not shown — mur open --all\n",
            if stale == 1 { "" } else { "s" }
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    fn old(source: ItemSource, days: i64) -> OpenItem {
        item(source, "aged", Utc::now() - chrono::Duration::days(days))
    }

    #[test]
    fn split_keeps_observed_fresh_and_moves_only_aged_reports() {
        let n = mur_open_items::REPORTED_STALE_AFTER_DAYS + 1;
        let (fresh, stale) = split_stale(
            vec![
                old(ItemSource::Observed, n),
                old(ItemSource::Reported, n),
                item(ItemSource::Reported, "new", Utc::now()),
            ],
            Utc::now(),
        );
        assert_eq!(fresh.len(), 2, "observed and the recent report stay");
        assert_eq!(stale.len(), 1);
    }

    #[test]
    fn summary_appends_the_stale_count_and_omits_it_at_zero() {
        let one = vec![item(ItemSource::Reported, "a", Utc::now())];
        assert!(summary_line(&one, 3).unwrap().contains("3 stale"));
        assert!(!summary_line(&one, 0).unwrap().contains("stale"));
    }

    /// Demoted, not deleted. If everything aged out, the line must still say
    /// so — silence here would be the accumulation bug traded for a worse one.
    #[test]
    fn summary_still_speaks_when_every_item_is_stale() {
        let s = summary_line(&[], 4).expect("4 hidden items cannot be silent");
        assert!(s.contains("4 stale"), "{s}");
        assert!(s.contains("--all"), "must name the way to see them: {s}");
    }

    #[test]
    fn render_footer_names_what_aged_out_and_how_to_see_it() {
        let out = render(&[item(ItemSource::Observed, "a", Utc::now())], &[], 2);
        assert!(out.contains("2 stale items not shown"), "{out}");
        assert!(out.contains("mur open --all"), "{out}");
    }

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
        let mut v = [
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
        let out = render(
            &[
                item(ItemSource::Observed, "a", Utc::now()),
                item(ItemSource::Reported, "b", Utc::now()),
            ],
            &[],
            0,
        );
        assert!(out.contains("observed"), "{out}");
        assert!(out.contains("reported"), "{out}");
        assert!(out.contains("unverified"), "{out}");
    }

    #[test]
    fn empty_says_so_rather_than_printing_a_bare_header() {
        assert_eq!(render(&[], &[], 0), "No open items.\n");
    }

    /// A permanent mute is only safe if the list always says something is
    /// muted. The reader must never have to wonder whether anything is hidden.
    #[test]
    fn footer_names_muted_sources() {
        let out = render(
            &[item(ItemSource::Observed, "visible", Utc::now())],
            &["inbox".to_string(), "fleet:old".to_string()],
            0,
        );
        assert!(out.contains("2 sources muted"), "{out}");
        assert!(out.contains("inbox"), "{out}");
        assert!(out.contains("fleet:old"), "{out}");
        assert!(out.contains("mur open --all"), "{out}");
    }

    #[test]
    fn no_footer_when_nothing_is_muted() {
        let out = render(&[item(ItemSource::Observed, "a", Utc::now())], &[], 0);
        assert!(!out.contains("muted"), "{out}");
    }

    /// Everything muted is not the same as nothing outstanding, and the
    /// difference has to be visible.
    #[test]
    fn everything_muted_still_shows_the_footer() {
        let out = render(&[], &["inbox".to_string()], 0);
        assert!(out.contains("1 source muted"), "{out}");
        assert!(out.contains("No open items"), "{out}");
    }

    /// Nothing open must produce no line at all. "0 open items" after every
    /// turn is how a surface trains people to stop reading it.
    #[test]
    fn summary_is_silent_when_nothing_is_open() {
        assert_eq!(summary_line(&[], 0), None);
    }

    #[test]
    fn summary_counts_each_source_separately() {
        let s = summary_line(
            &[
                item(ItemSource::Observed, "a", Utc::now()),
                item(ItemSource::Observed, "b", Utc::now()),
                item(ItemSource::Reported, "c", Utc::now()),
            ],
            0,
        )
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

    #[test]
    fn partition_hides_muted_origins_and_names_them() {
        let items = vec![
            OpenItem {
                origin: "inbox".into(),
                ..item(ItemSource::Observed, "a", Utc::now())
            },
            OpenItem {
                origin: "fleet:x".into(),
                ..item(ItemSource::Observed, "b", Utc::now())
            },
        ];
        let (visible, muted) = partition(items, &["inbox".to_string()]);
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].origin, "fleet:x");
        assert_eq!(muted, vec!["inbox".to_string()]);
    }

    /// `fleet` must not swallow `fleet:acme`. Prefix matching is the one
    /// outcome a mute must never produce by accident.
    #[test]
    fn mute_matching_is_exact_not_prefix() {
        let items = vec![OpenItem {
            origin: "fleet:acme".into(),
            ..item(ItemSource::Observed, "a", Utc::now())
        }];
        let (visible, muted) = partition(items, &["fleet".to_string()]);
        assert_eq!(visible.len(), 1, "prefix must not match");
        assert!(muted.is_empty());
    }

    /// A configured mute that matched nothing is not named — the footer
    /// reports what the reader would otherwise have seen, not the config.
    #[test]
    fn a_mute_that_matched_nothing_is_not_reported() {
        let items = vec![OpenItem {
            origin: "inbox".into(),
            ..item(ItemSource::Observed, "a", Utc::now())
        }];
        let (_, muted) = partition(items, &["fleet:gone".to_string()]);
        assert!(muted.is_empty());
    }

    /// Muting a noisy source has to silence the turn notice too, or the mute
    /// does nothing where it matters most.
    #[test]
    fn fingerprint_over_visible_ignores_muted_churn() {
        let mk = |n: usize| OpenItem {
            title: format!("{n} proposals"),
            origin: "inbox".into(),
            ..item(ItemSource::Observed, "x", Utc::now())
        };
        let (v1, _) = partition(vec![mk(246)], &["inbox".to_string()]);
        let (v2, _) = partition(vec![mk(300)], &["inbox".to_string()]);
        assert_eq!(fingerprint(&v1), fingerprint(&v2));
    }
}
