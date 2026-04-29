//! Spec §4.8 step 1 gates: enabled / paused / learning / quiet_hours.
//!
//! Combines all the proactive-send gates into one check; returns a typed outcome
//! so the outbox can log the reason a tick was skipped.

use chrono::{DateTime, Local, Timelike, Utc};
use mur_common::agent::{ProactiveConfig, QuietHours};

use crate::companion::schedule::parse_hhmm;

#[derive(Debug, Clone, PartialEq)]
pub enum GateOutcome {
    Allowed,
    Blocked { reason: BlockReason },
}

#[derive(Debug, Clone, PartialEq)]
pub enum BlockReason {
    ProactiveDisabled,
    Paused,
    Learning,
    QuietHours,
}

/// Evaluate gates in order. Spec §4.8 step 1.
pub fn check(
    proactive: &ProactiveConfig,
    now_utc: DateTime<Utc>,
    now_local: DateTime<Local>,
) -> GateOutcome {
    if !proactive.enabled {
        return GateOutcome::Blocked {
            reason: BlockReason::ProactiveDisabled,
        };
    }
    if let Some(p) = proactive.paused_until
        && p > now_utc
    {
        return GateOutcome::Blocked {
            reason: BlockReason::Paused,
        };
    }
    if let Some(l) = proactive.learning_until
        && l > now_utc
    {
        return GateOutcome::Blocked {
            reason: BlockReason::Learning,
        };
    }
    if let Some(qh) = &proactive.quiet_hours {
        let now_naive = chrono::NaiveTime::from_hms_opt(
            now_local.hour(),
            now_local.minute(),
            now_local.second(),
        )
        .unwrap_or(chrono::NaiveTime::MIN);
        if quiet_hours_contain(qh, now_naive) {
            return GateOutcome::Blocked {
                reason: BlockReason::QuietHours,
            };
        }
    }
    GateOutcome::Allowed
}

fn quiet_hours_contain(qh: &QuietHours, now: chrono::NaiveTime) -> bool {
    let Some(start) = parse_hhmm(&qh.start) else {
        return false;
    };
    let Some(end) = parse_hhmm(&qh.end) else {
        return false;
    };
    if start <= end {
        now >= start && now < end
    } else {
        now >= start || now < end
    }
}
