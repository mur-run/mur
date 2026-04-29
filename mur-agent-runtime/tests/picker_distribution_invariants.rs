//! Statistical sanity checks on the picker's WeightedIndex math.
//!
//! Uses a fixed seed (42) so the tolerance bands stay stable across runs.
//! Seed 42 is conventional and gives stable distributions over N=10_000 picks.
//! If this test ever flakes, the tolerance band has too much slack OR a real
//! bug was introduced — do NOT widen the band past ±5% without investigation.

use chrono::{TimeZone, Utc};
use mur_agent_runtime::companion::picker::{BanditState, Picker, TemplateState};
use mur_common::companion::Situation;
use std::collections::BTreeMap;

fn template(id: &str, situation: Situation, weight: f32) -> TemplateState {
    TemplateState {
        id: id.to_string(),
        situation,
        weight,
        last_used_at: None,
        pos_count: 0,
        neg_count: 0,
        dismiss_count: 0,
        cooldown_days: 0,
    }
}

fn build_state(templates: Vec<TemplateState>) -> BanditState {
    let mut map = BTreeMap::new();
    for t in templates {
        map.insert(t.id.clone(), t);
    }
    BanditState {
        version: 1,
        morning_sent_today: None,
        templates: map,
    }
}

#[test]
fn equal_weights_distribute_uniformly() {
    let templates = vec![
        template("a", Situation::MorningGreeting, 1.0),
        template("b", Situation::MorningGreeting, 1.0),
    ];
    let state = build_state(templates);
    let mut picker = Picker::with_seed(state, 42);
    let now = Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap();
    let n: usize = 10_000;
    let mut count_a = 0;
    let mut count_b = 0;
    for _ in 0..n {
        match picker.pick(Situation::MorningGreeting, now).as_deref() {
            Some("a") => count_a += 1,
            Some("b") => count_b += 1,
            _ => panic!("unexpected pick"),
        }
    }
    let expected = n as f64 / 2.0;
    let tolerance = expected * 0.05; // ±5%
    assert!(
        ((count_a as f64) - expected).abs() <= tolerance,
        "count_a = {count_a}, expected ~{expected}"
    );
    assert!(
        ((count_b as f64) - expected).abs() <= tolerance,
        "count_b = {count_b}, expected ~{expected}"
    );
}

#[test]
fn weight_2x_distributes_2_to_1() {
    let templates = vec![
        template("heavy", Situation::MorningGreeting, 2.0),
        template("light", Situation::MorningGreeting, 1.0),
    ];
    let state = build_state(templates);
    let mut picker = Picker::with_seed(state, 42);
    let now = Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap();
    let n: usize = 10_000;
    let mut count_h = 0;
    let mut count_l = 0;
    for _ in 0..n {
        match picker.pick(Situation::MorningGreeting, now).as_deref() {
            Some("heavy") => count_h += 1,
            Some("light") => count_l += 1,
            _ => panic!("unexpected pick"),
        }
    }
    // Expected 2:1 means heavy = 2/3 * n, light = 1/3 * n.
    let expected_h = (n as f64) * 2.0 / 3.0;
    let expected_l = (n as f64) * 1.0 / 3.0;
    let tol_h = expected_h * 0.05;
    let tol_l = expected_l * 0.05;
    assert!(
        ((count_h as f64) - expected_h).abs() <= tol_h,
        "count_heavy = {count_h}, expected ~{expected_h}"
    );
    assert!(
        ((count_l as f64) - expected_l).abs() <= tol_l,
        "count_light = {count_l}, expected ~{expected_l}"
    );
}
