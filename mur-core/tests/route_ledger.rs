mod route_fixtures;
use mur_common::route::RouteDecision;
use mur_core::route::ledger::EscalationLedger;
use route_fixtures::make_event;
use tempfile::TempDir;

#[test]
fn ledger_appends_and_replays() {
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
    ledger.append(&make_event(true)).unwrap();
    ledger.append(&make_event(false)).unwrap();
    ledger.flush().unwrap();
    drop(ledger);

    let events = EscalationLedger::replay_today(tmp.path());
    assert_eq!(events.len(), 2);
    let escalations: Vec<_> = events
        .iter()
        .filter(|e| matches!(e.decision, RouteDecision::Escalate { .. }))
        .collect();
    assert_eq!(escalations.len(), 1);
}

#[test]
fn summary_reports_rate_and_savings() {
    let tmp = TempDir::new().unwrap();
    let mut ledger = EscalationLedger::open(tmp.path()).unwrap();
    // 3 local (each avoids $0.015), 2 escalate (each spends $0.015).
    for escalate in [false, true, false, false, true] {
        ledger.append(&make_event(escalate)).unwrap();
    }
    ledger.flush().unwrap();
    drop(ledger);

    let s = EscalationLedger::summary(tmp.path(), 1);
    assert_eq!(s.escalations, 2);
    assert_eq!(s.total, 5);
    assert!((s.rate - 0.4).abs() < 0.001);
    assert!((s.spend_usd - 0.030).abs() < 1e-9, "spend={}", s.spend_usd);
    assert!(
        (s.savings_usd - 0.045).abs() < 1e-9,
        "savings={}",
        s.savings_usd
    );
}

#[test]
fn empty_ledger_has_zero_summary() {
    let tmp = TempDir::new().unwrap();
    let s = EscalationLedger::summary(tmp.path(), 7);
    assert_eq!(s.total, 0);
    assert_eq!(s.rate, 0.0);
    assert_eq!(s.savings_usd, 0.0);
}
