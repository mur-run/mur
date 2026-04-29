use mur_agent_runtime::companion::picker::{BanditState, TemplateState};
use mur_common::companion::Situation;

#[test]
fn bandit_state_serde_roundtrip() {
    let mut s = BanditState {
        version: 1,
        morning_sent_today: None,
        templates: Default::default(),
    };
    s.templates.insert(
        "t1".into(),
        TemplateState {
            id: "t1".into(),
            situation: Situation::MorningGreeting,
            weight: 1.5,
            last_used_at: Some(chrono::Utc::now()),
            pos_count: 3,
            neg_count: 1,
            dismiss_count: 2,
            cooldown_days: 7,
        },
    );
    let json = serde_json::to_string(&s).unwrap();
    let s2: BanditState = serde_json::from_str(&json).unwrap();
    assert_eq!(s, s2);
}

#[test]
fn legacy_minimal_bandit_state_loads_with_defaults() {
    let s: BanditState = serde_json::from_str("{}").unwrap();
    assert_eq!(s.version, 1);
    assert!(s.templates.is_empty());
    assert!(s.morning_sent_today.is_none());
}
