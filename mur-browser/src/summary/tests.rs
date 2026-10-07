use super::*;
use crate::recorder::{Mode, Step};
use chrono::{Duration, TimeZone};

fn step(action: Action, intent: &str, value: Option<&str>) -> Step {
    Step {
        step: 1,
        intent: intent.into(),
        intent_auto: false,
        action,
        value: value.map(Into::into),
        locators: vec![],
        healed: false,
        last_hit: 0,
        ref_at_record: None,
    }
}

fn now() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 6, 12, 0, 0).unwrap()
}

fn run(steps: Vec<Step>) -> Run {
    Run {
        name: "shop-login".into(),
        mode: Mode::Automation,
        profile: None,
        recorded_at: now() - Duration::days(30),
        description: None,
        tags: vec![],
        replayed_at: None,
        replay_count: 0,
        steps,
    }
}

fn replayed(count: u32, ago: Duration) -> Run {
    let mut r = run(vec![]);
    r.replay_count = count;
    r.replayed_at = Some(now() - ago);
    r
}

#[test]
fn domain_is_host_of_first_goto() {
    let r = run(vec![
        step(Action::Click, "點擊登入按鈕", None),
        step(
            Action::Goto,
            "開啟商店首頁",
            Some("https://shop.example.com/a?b=1"),
        ),
        step(
            Action::Goto,
            "開啟其他頁面",
            Some("https://other.example.org/"),
        ),
    ]);
    assert_eq!(r.domain().as_deref(), Some("shop.example.com"));
}

#[test]
fn domain_none_without_goto() {
    let r = run(vec![step(Action::Click, "點擊登入按鈕", None)]);
    assert_eq!(r.domain(), None);
}

#[test]
fn domain_none_on_unparseable_url() {
    let r = run(vec![step(Action::Goto, "開啟商店首頁", Some("not a url"))]);
    assert_eq!(r.domain(), None);
}

#[test]
fn summary_prefers_description() {
    let mut r = run(vec![step(Action::Click, "點擊登入按鈕", None)]);
    r.description = Some("每日登入".into());
    assert_eq!(r.summary(), "每日登入");
}

#[test]
fn summary_joins_non_goto_intents_with_arrow() {
    let r = run(vec![
        step(
            Action::Goto,
            "開啟商店首頁",
            Some("https://shop.example.com/"),
        ),
        step(Action::Fill, "輸入帳號", Some("me")),
        step(Action::Click, "按下登入", None),
    ]);
    assert_eq!(r.summary(), "輸入帳號 → 按下登入");
}

#[test]
fn summary_truncates_at_width_with_ellipsis() {
    let long = "a".repeat(SUMMARY_WIDTH + 10);
    let r = run(vec![step(Action::Click, &long, None)]);
    let s = r.summary();
    assert_eq!(s.chars().count(), SUMMARY_WIDTH);
    assert!(s.ends_with('…'));
}

#[test]
fn summary_truncation_counts_cjk_as_two_columns() {
    let long = "登".repeat(SUMMARY_WIDTH);
    let r = run(vec![step(Action::Click, &long, None)]);
    let s = r.summary();
    let cols: usize = s.chars().map(|c| c.width().unwrap_or(0)).sum();
    assert!(cols <= SUMMARY_WIDTH, "{cols} columns");
    // 19 wide glyphs (38 cols) + the one-column ellipsis.
    assert_eq!(s.chars().count(), 20);
    assert!(s.ends_with('…'));
}

#[test]
fn search_text_keeps_intent_summary_truncates() {
    let long = format!("{}登入", "a".repeat(SUMMARY_WIDTH));
    let r = run(vec![step(Action::Click, &long, None)]);
    assert!(!r.summary().contains("登入"));
    assert!(r.search_text().contains("登入"));
}

#[test]
fn search_text_includes_intent_when_description_set() {
    let mut r = run(vec![
        step(
            Action::Goto,
            "開啟商店首頁",
            Some("https://shop.example.com/"),
        ),
        step(Action::Click, "按下登入", None),
    ]);
    r.description = Some("每日簽到".into());
    r.tags = vec!["daily".into()];
    let text = r.search_text();
    for needle in [
        "shop-login",
        "每日簽到",
        "daily",
        "shop.example.com",
        "按下登入",
    ] {
        assert!(text.contains(needle), "missing {needle}");
    }
}

#[test]
fn frecency_zero_replays_falls_back_to_recorded_at() {
    let mut older = run(vec![]);
    older.recorded_at = now() - Duration::days(10);
    let mut newer = run(vec![]);
    newer.recorded_at = now() - Duration::days(1);
    assert!(newer.frecency(now()) > older.frecency(now()));
}

#[test]
fn frecency_decays_with_half_life() {
    let half = Duration::seconds((FRECENCY_HALF_LIFE_DAYS * SECONDS_PER_DAY) as i64);
    let old = replayed(8, half).frecency(now());
    let fresh = replayed(4, Duration::zero()).frecency(now());
    assert!((old - fresh).abs() < 1e-9, "{old} vs {fresh}");
}

#[test]
fn frecency_replayed_beats_unreplayed_regardless_of_recorded_at() {
    let mut never = run(vec![]);
    never.recorded_at = now();
    let ancient = replayed(1, Duration::days(3650));
    assert!(ancient.frecency(now()) > never.frecency(now()));
}
