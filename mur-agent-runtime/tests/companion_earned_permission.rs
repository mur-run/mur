//! M4.5 — earned_permission gate tests.

use chrono::{DateTime, Duration, Local, TimeZone, Utc};
use mur_agent_runtime::companion::earned_permission::*;
use mur_common::agent::{ProactiveConfig, QuietHours};

fn local_at(h: u32, m: u32) -> DateTime<Local> {
    Local.with_ymd_and_hms(2026, 4, 29, h, m, 0).unwrap()
}
fn utc_now() -> DateTime<Utc> {
    Utc::now()
}

fn pc() -> ProactiveConfig {
    ProactiveConfig {
        enabled: true,
        learning_until: None,
        quiet_hours: None,
        active_hours: None,
        daily_cap: 3,
        channels: vec!["stdout".into()],
        paused_until: None,
    }
}

#[test]
fn allowed_when_enabled_no_gates() {
    let out = check(&pc(), utc_now(), local_at(11, 0));
    assert_eq!(out, GateOutcome::Allowed);
}

#[test]
fn blocked_when_disabled() {
    let p = ProactiveConfig {
        enabled: false,
        ..pc()
    };
    let out = check(&p, utc_now(), local_at(11, 0));
    assert_eq!(
        out,
        GateOutcome::Blocked {
            reason: BlockReason::ProactiveDisabled
        }
    );
}

#[test]
fn blocked_when_paused_in_future() {
    let p = ProactiveConfig {
        paused_until: Some(Utc::now() + Duration::hours(1)),
        ..pc()
    };
    let out = check(&p, Utc::now(), local_at(11, 0));
    assert_eq!(
        out,
        GateOutcome::Blocked {
            reason: BlockReason::Paused
        }
    );
}

#[test]
fn allowed_when_paused_in_past() {
    let p = ProactiveConfig {
        paused_until: Some(Utc::now() - Duration::hours(1)),
        ..pc()
    };
    let out = check(&p, Utc::now(), local_at(11, 0));
    assert_eq!(out, GateOutcome::Allowed);
}

#[test]
fn blocked_when_learning_in_future() {
    let p = ProactiveConfig {
        learning_until: Some(Utc::now() + Duration::days(3)),
        ..pc()
    };
    let out = check(&p, Utc::now(), local_at(11, 0));
    assert_eq!(
        out,
        GateOutcome::Blocked {
            reason: BlockReason::Learning
        }
    );
}

#[test]
fn quiet_hours_block_at_night_with_wrap() {
    let p = ProactiveConfig {
        quiet_hours: Some(QuietHours {
            start: "22:00".into(),
            end: "08:00".into(),
        }),
        ..pc()
    };
    let out1 = check(&p, Utc::now(), local_at(23, 0));
    assert_eq!(
        out1,
        GateOutcome::Blocked {
            reason: BlockReason::QuietHours
        }
    );
    let out2 = check(&p, Utc::now(), local_at(3, 0));
    assert_eq!(
        out2,
        GateOutcome::Blocked {
            reason: BlockReason::QuietHours
        }
    );
    let out3 = check(&p, Utc::now(), local_at(12, 0));
    assert_eq!(out3, GateOutcome::Allowed);
}

#[test]
fn quiet_hours_non_wrap_block_only_in_window() {
    let p = ProactiveConfig {
        quiet_hours: Some(QuietHours {
            start: "12:00".into(),
            end: "14:00".into(),
        }),
        ..pc()
    };
    let out_in = check(&p, Utc::now(), local_at(13, 0));
    assert_eq!(
        out_in,
        GateOutcome::Blocked {
            reason: BlockReason::QuietHours
        }
    );
    let out_out = check(&p, Utc::now(), local_at(15, 0));
    assert_eq!(out_out, GateOutcome::Allowed);
}

#[test]
fn paused_takes_precedence_over_learning_takes_precedence_over_quiet() {
    let p = ProactiveConfig {
        paused_until: Some(Utc::now() + Duration::hours(1)),
        learning_until: Some(Utc::now() + Duration::days(3)),
        quiet_hours: Some(QuietHours {
            start: "22:00".into(),
            end: "08:00".into(),
        }),
        ..pc()
    };
    let out = check(&p, Utc::now(), local_at(23, 0));
    assert_eq!(
        out,
        GateOutcome::Blocked {
            reason: BlockReason::Paused
        }
    );
}
