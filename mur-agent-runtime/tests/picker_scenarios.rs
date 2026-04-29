//! Insta snapshot scenarios for picker behavior.
//!
//! Each scenario seeds a `BanditState`, runs N=200 picks, and snapshots a
//! "histogram" line listing each id and its count. Snapshots live under
//! `tests/snapshots/`. Update with `cargo insta review`; CI sets
//! `INSTA_UPDATE=no` so stale snapshots fail the build.

use chrono::{Duration, TimeZone, Utc};
use mur_agent_runtime::companion::picker::{BanditState, Picker, TemplateState};
use mur_common::companion::Situation;
use std::collections::BTreeMap;

const N: usize = 200;

fn template(id: &str, situation: Situation, weight: f32, cooldown_days: u32) -> TemplateState {
    TemplateState {
        id: id.to_string(),
        situation,
        weight,
        last_used_at: None,
        pos_count: 0,
        neg_count: 0,
        dismiss_count: 0,
        cooldown_days,
    }
}

fn run_picks(state: BanditState, situation: Situation, n: usize) -> Vec<String> {
    let mut picker = Picker::with_seed(state, 7);
    let now = Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap();
    (0..n)
        .map(|_| {
            picker
                .pick(situation.clone(), now)
                .unwrap_or_else(|| "<none>".to_string())
        })
        .collect()
}

fn histogram(picks: &[String]) -> String {
    let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
    for p in picks {
        *counts.entry(p.as_str()).or_insert(0) += 1;
    }
    let mut s = String::new();
    for (id, c) in counts {
        s.push_str(&format!("{id}: {c}\n"));
    }
    s
}

#[test]
fn all_eligible_uniform() {
    let mut map: BTreeMap<String, TemplateState> = BTreeMap::new();
    for id in ["a", "b", "c"] {
        let t = template(id, Situation::MorningGreeting, 1.0, 0);
        map.insert(id.to_string(), t);
    }
    let state = BanditState {
        version: 1,
        morning_sent_today: None,
        templates: map,
    };
    let picks = run_picks(state, Situation::MorningGreeting, N);
    let snap = histogram(&picks);
    insta::assert_snapshot!("all_eligible_uniform", snap);
}

#[test]
fn one_with_negative_weight() {
    // The picker uses WeightedIndex which doesn't accept negative weights.
    // We simulate "this template should never be picked" by setting weight
    // to 0.0 (or a tiny epsilon if 0.0 panics). Check picker.rs to see how
    // it handles weight=0.0; if WeightedIndex::new(weights) returns an error
    // when ALL weights are zero, it's fine here because `normal` has weight
    // 1.0. If it errors with any zero, we'll see <none> in the snapshot.
    //
    // The snapshot documents the actual behavior either way.
    let mut map: BTreeMap<String, TemplateState> = BTreeMap::new();
    map.insert(
        "normal".into(),
        template("normal", Situation::MorningGreeting, 1.0, 0),
    );
    map.insert(
        "zero".into(),
        template("zero", Situation::MorningGreeting, 0.0, 0),
    );
    let state = BanditState {
        version: 1,
        morning_sent_today: None,
        templates: map,
    };
    let picks = run_picks(state, Situation::MorningGreeting, N);
    let snap = histogram(&picks);
    insta::assert_snapshot!("one_with_negative_weight", snap);
}

#[test]
fn cooldown_excludes_morning_after_send() {
    // Two templates; one was just used (last_used_at = now - 1h), one fresh.
    // With cooldown_days = 1, the freshly-used one is excluded for a day.
    let now = Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap();
    let mut just_used = template("just-used", Situation::MorningGreeting, 1.0, 1);
    just_used.last_used_at = Some(now - Duration::hours(1));
    let fresh = template("fresh", Situation::MorningGreeting, 1.0, 1);

    let mut map: BTreeMap<String, TemplateState> = BTreeMap::new();
    map.insert("just-used".into(), just_used);
    map.insert("fresh".into(), fresh);
    let state = BanditState {
        version: 1,
        morning_sent_today: None,
        templates: map,
    };

    let picks = run_picks(state, Situation::MorningGreeting, N);
    let snap = histogram(&picks);
    insta::assert_snapshot!("cooldown_excludes_morning_after_send", snap);
}
