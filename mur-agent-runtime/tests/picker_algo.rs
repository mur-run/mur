//! M4.2: picker selection + record algorithm.

use chrono::{Duration, Utc};
use mur_agent_runtime::companion::picker::{BanditState, Picker, TemplateState};
use mur_common::companion::{Signal, Situation};

fn t(id: &str, situation: Situation, weight: f32, cooldown_days: u32) -> TemplateState {
    TemplateState {
        id: id.into(),
        situation,
        weight,
        last_used_at: None,
        pos_count: 0,
        neg_count: 0,
        dismiss_count: 0,
        cooldown_days,
    }
}

fn state(templates: Vec<TemplateState>) -> BanditState {
    let mut s = BanditState::default();
    for t in templates {
        s.templates.insert(t.id.clone(), t);
    }
    s
}

#[test]
fn empty_pool_returns_none() {
    let mut p = Picker::with_seed(BanditState::default(), 42);
    assert!(p.pick(Situation::MorningGreeting, Utc::now()).is_none());
}

#[test]
fn single_eligible_template_picked() {
    let s = state(vec![t("only", Situation::MorningGreeting, 1.0, 7)]);
    let mut p = Picker::with_seed(s, 42);
    assert_eq!(
        p.pick(Situation::MorningGreeting, Utc::now()),
        Some("only".into())
    );
}

#[test]
fn cooldown_excludes_recently_used() {
    let mut tt = t("recent", Situation::MorningGreeting, 1.0, 7);
    tt.last_used_at = Some(Utc::now() - Duration::days(3));
    let s = state(vec![tt]);
    let mut p = Picker::with_seed(s, 42);
    assert!(p.pick(Situation::MorningGreeting, Utc::now()).is_none());
}

#[test]
fn weight_cap_at_five() {
    let s = state(vec![t("x", Situation::MorningGreeting, 4.5, 7)]);
    let mut p = Picker::with_seed(s, 42);
    p.record(&"x".into(), Signal::Positive, Utc::now());
    p.record(&"x".into(), Signal::Positive, Utc::now());
    p.record(&"x".into(), Signal::Positive, Utc::now());
    assert!(p.state.templates["x"].weight <= 5.0 + f32::EPSILON);
    assert!(p.state.templates["x"].weight >= 4.5);
}

#[test]
fn weight_floor_at_zero_one() {
    let s = state(vec![t("x", Situation::MorningGreeting, 0.5, 7)]);
    let mut p = Picker::with_seed(s, 42);
    for _ in 0..20 {
        p.record(&"x".into(), Signal::Negative, Utc::now());
    }
    assert!(p.state.templates["x"].weight >= 0.1);
    assert!(p.state.templates["x"].weight <= 0.11);
}

#[test]
fn equal_weights_roughly_uniform_over_200_picks() {
    // 4 templates, equal weight, no cooldown — distribution should be ~25% each ±10%.
    let s = state(vec![
        t("a", Situation::ShareQuote, 1.0, 0),
        t("b", Situation::ShareQuote, 1.0, 0),
        t("c", Situation::ShareQuote, 1.0, 0),
        t("d", Situation::ShareQuote, 1.0, 0),
    ]);
    let mut p = Picker::with_seed(s, 42);
    let mut counts = std::collections::HashMap::new();
    let now = Utc::now();
    for _ in 0..200 {
        let id = p.pick(Situation::ShareQuote, now).unwrap();
        *counts.entry(id).or_insert(0u32) += 1;
    }
    for k in ["a", "b", "c", "d"] {
        let c = *counts.get(k).unwrap_or(&0);
        let pct = c as f32 / 200.0;
        assert!(
            (0.15..=0.35).contains(&pct),
            "template {k} count {c} not in [30,70] of 200"
        );
    }
}

#[test]
fn double_weight_picked_roughly_twice_as_often() {
    let s = state(vec![
        t("heavy", Situation::ShareQuote, 2.0, 0),
        t("light", Situation::ShareQuote, 1.0, 0),
    ]);
    let mut p = Picker::with_seed(s, 42);
    let mut h = 0;
    let mut l = 0;
    let now = Utc::now();
    for _ in 0..600 {
        match p.pick(Situation::ShareQuote, now).unwrap().as_str() {
            "heavy" => h += 1,
            "light" => l += 1,
            _ => unreachable!(),
        }
    }
    let ratio = h as f32 / l as f32;
    assert!(ratio > 1.7 && ratio < 2.3, "ratio {ratio} not ≈ 2.0");
}

#[test]
fn record_sent_sets_last_used_at() {
    let s = state(vec![t("x", Situation::MorningGreeting, 1.0, 7)]);
    let mut p = Picker::with_seed(s, 42);
    let now = Utc::now();
    p.record(&"x".into(), Signal::Sent, now);
    assert_eq!(p.state.templates["x"].last_used_at, Some(now));
}

#[test]
fn record_dismiss_increments_counter_only() {
    let s = state(vec![t("x", Situation::MorningGreeting, 1.0, 7)]);
    let mut p = Picker::with_seed(s, 42);
    let w0 = p.state.templates["x"].weight;
    p.record(&"x".into(), Signal::Dismiss, Utc::now());
    assert_eq!(p.state.templates["x"].weight, w0); // unchanged
    assert_eq!(p.state.templates["x"].dismiss_count, 1);
}
