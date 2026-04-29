use chrono::{Local, TimeZone};
use mur_agent_runtime::companion::situations::pick_for_hour;
use mur_common::companion::Situation;
use rand::SeedableRng;
use rand::rngs::StdRng;

fn local_at(hour: u32, minute: u32) -> chrono::DateTime<Local> {
    Local.with_ymd_and_hms(2026, 4, 29, hour, minute, 0).unwrap()
}

#[test]
fn morning_window_returns_morning_or_quote() {
    let mut rng = StdRng::seed_from_u64(42);
    for _ in 0..50 {
        let s = pick_for_hour(local_at(7, 0), None, &mut rng);
        assert!(
            matches!(s, Some(Situation::MorningGreeting | Situation::ShareQuote)),
            "got {s:?}"
        );
    }
}

#[test]
fn morning_already_sent_today_excludes_morning_greeting() {
    let mut rng = StdRng::seed_from_u64(42);
    let today = local_at(7, 0).date_naive();
    for _ in 0..50 {
        let s = pick_for_hour(local_at(7, 0), Some(today), &mut rng);
        assert_eq!(s, Some(Situation::ShareQuote));
    }
}

#[test]
fn midday_window_returns_check_quote_or_link() {
    let mut rng = StdRng::seed_from_u64(42);
    for _ in 0..50 {
        let s = pick_for_hour(local_at(11, 30), None, &mut rng);
        assert!(matches!(
            s,
            Some(Situation::GentleCheckIn | Situation::ShareQuote | Situation::ShareLink)
        ));
    }
}

#[test]
fn afternoon_window_returns_check_or_link() {
    let mut rng = StdRng::seed_from_u64(42);
    for _ in 0..50 {
        let s = pick_for_hour(local_at(15, 0), None, &mut rng);
        assert!(matches!(
            s,
            Some(Situation::GentleCheckIn | Situation::ShareLink)
        ));
    }
}

#[test]
fn evening_window_returns_quote_or_link() {
    let mut rng = StdRng::seed_from_u64(42);
    for _ in 0..50 {
        let s = pick_for_hour(local_at(20, 0), None, &mut rng);
        assert!(matches!(
            s,
            Some(Situation::ShareQuote | Situation::ShareLink)
        ));
    }
}

#[test]
fn night_returns_none() {
    let mut rng = StdRng::seed_from_u64(42);
    assert_eq!(pick_for_hour(local_at(23, 0), None, &mut rng), None);
    assert_eq!(pick_for_hour(local_at(2, 0), None, &mut rng), None);
    assert_eq!(pick_for_hour(local_at(5, 30), None, &mut rng), None);
}

#[test]
fn morning_window_distribution_roughly_sixty_forty() {
    let mut rng = StdRng::seed_from_u64(42);
    let mut morning = 0;
    let mut quote = 0;
    for _ in 0..1000 {
        match pick_for_hour(local_at(7, 0), None, &mut rng) {
            Some(Situation::MorningGreeting) => morning += 1,
            Some(Situation::ShareQuote) => quote += 1,
            _ => unreachable!(),
        }
    }
    let m_pct = morning as f32 / 1000.0;
    let q_pct = quote as f32 / 1000.0;
    assert!(m_pct > 0.5 && m_pct < 0.7, "morning {m_pct}, expected ~0.6");
    assert!(q_pct > 0.3 && q_pct < 0.5, "quote {q_pct}, expected ~0.4");
}
