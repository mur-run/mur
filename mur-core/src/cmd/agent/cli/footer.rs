//! Pure footer math: tokens, cost, and context-window fill from `Task.usage`
//! plus the agent's `models.yaml` pricing. No ratatui, no I/O — unit-tested.

use mur_monitor::backoff::UNHEALTHY_AFTER_UNKNOWN;
use mur_monitor::state::MonitorState;
use mur_monitor::store::MonitorRow;
use serde_json::Value;

/// Context bar thresholds (percent) and width.
pub const CTX_YELLOW_PCT: u8 = 70;
pub const CTX_RED_PCT: u8 = 90;
pub const CTX_BAR_WIDTH: usize = 6;

/// How often `App::refresh_monitor_counts` is allowed to reopen the monitor
/// store — double `mur_core::monitor::service::TICK_INTERVAL` (15s, the
/// daemon's own poll cadence), so the footer never re-opens SQLite faster
/// than the daemon could possibly have changed anything.
pub const MONITOR_REFRESH_SECS: u64 = 30;

#[derive(Debug, Clone, Copy, Default)]
pub struct UsageCounts {
    pub input: u64,
    pub output: u64,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct Pricing {
    pub in_per_1k: Option<f64>,
    pub out_per_1k: Option<f64>,
    pub window: Option<u64>,
}

// P1 footer is monochrome; threshold color wires this in P2.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CtxColor {
    Green,
    Yellow,
    Red,
}

pub fn parse_usage(usage: &Value) -> UsageCounts {
    UsageCounts {
        input: usage
            .get("input_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        output: usage
            .get("output_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
    }
}

/// Clean per-context fill emitted by the runtime (Task 1). `None` on older
/// runtimes — the caller falls back to hiding the bar.
pub fn context_tokens(usage: &Value) -> Option<u64> {
    usage.get("context_tokens").and_then(Value::as_u64)
}

pub fn turn_cost(p: &Pricing, u: &UsageCounts) -> Option<f64> {
    match (p.in_per_1k, p.out_per_1k) {
        (Some(i), Some(o)) => Some(u.input as f64 / 1000.0 * i + u.output as f64 / 1000.0 * o),
        _ => None,
    }
}

pub fn context_pct(used: u64, window: u64) -> u8 {
    if window == 0 {
        return 0;
    }
    ((used as f64 / window as f64) * 100.0)
        .round()
        .clamp(0.0, 100.0) as u8
}

// P1 footer is monochrome; threshold color wires this in P2.
#[allow(dead_code)]
pub fn ctx_color(pct: u8) -> CtxColor {
    if pct < CTX_YELLOW_PCT {
        CtxColor::Green
    } else if pct < CTX_RED_PCT {
        CtxColor::Yellow
    } else {
        CtxColor::Red
    }
}

pub fn ctx_bar(pct: u8, width: usize) -> String {
    let filled = (pct as usize * width / 100).min(width);
    format!("{}{}", "▓".repeat(filled), "░".repeat(width - filled))
}

/// Does this monitor want a human or a second look? Quiet polling does not
/// count: a footer number that is always present stops being read.
pub fn has_condition(row: &MonitorRow) -> bool {
    matches!(
        row.state,
        MonitorState::Exhausted | MonitorState::ActionPending
    ) || row.stalled_since.is_some()
        || row.unknown_streak >= UNHEALTHY_AFTER_UNKNOWN
}

/// How many monitors currently have a condition. One per monitor, however
/// many conditions it has at once.
pub fn conditions(rows: &[MonitorRow]) -> usize {
    rows.iter().filter(|r| has_condition(r)).count()
}

/// Footer segment, or `None` when nothing wants attention.
pub fn monitor_label(n: usize) -> Option<String> {
    (n > 0).then(|| format!("MONITOR ({n})"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use mur_monitor::spec::{CreatedBy, MonitorSpec, Source, SourceType};
    use mur_monitor::state::Outcome;

    fn t0() -> DateTime<Utc> {
        DateTime::UNIX_EPOCH
    }

    /// Build a `MonitorRow` with only the fields a condition check cares
    /// about varied; everything else is a fixed, healthy default.
    fn row(
        state: MonitorState,
        stalled_since: Option<DateTime<Utc>>,
        unknown_streak: u32,
    ) -> MonitorRow {
        MonitorRow {
            id: "m1".to_string(),
            name: "test monitor".to_string(),
            spec: MonitorSpec {
                schema_version: 1,
                name: "test monitor".to_string(),
                source: Source {
                    r#type: SourceType::GithubActions,
                    reference: "owner/repo#1".to_string(),
                    credential_ref: None,
                    write_credential_ref: None,
                },
                outcomes: Default::default(),
                actions: Default::default(),
                policy: Default::default(),
                notifications: Default::default(),
                idempotency_key: "key1".to_string(),
                created_by: CreatedBy {
                    actor: "human".to_string(),
                    reason: String::new(),
                    originating_run_id: None,
                },
            },
            state,
            outcome: Outcome::Pending,
            source_type: SourceType::GithubActions,
            reference: "owner/repo#1".to_string(),
            idempotency_key: "key1".to_string(),
            created_at: t0(),
            work_started_at: t0(),
            next_check_at: t0(),
            last_checked_at: None,
            last_progress_at: t0(),
            progress_token: None,
            pending_attempts: 0,
            unknown_streak,
            remediation_attempts: 0,
            cycle_id: "cycle1".to_string(),
            stalled_since,
            soft_notified: false,
            hard_reached: false,
            fence: 0,
            version: 1,
        }
    }

    #[test]
    fn monitor_label_is_silent_without_a_condition() {
        assert_eq!(monitor_label(0), None);
    }

    #[test]
    fn monitor_label_counts_conditions() {
        assert_eq!(monitor_label(1).as_deref(), Some("MONITOR (1)"));
        assert_eq!(monitor_label(4).as_deref(), Some("MONITOR (4)"));
    }

    #[test]
    fn quiet_monitors_do_not_count() {
        // The three states a healthy monitor cycles through contribute nothing;
        // only a live condition does.
        let quiet = row(MonitorState::Sleeping, None, 0);
        let active = row(MonitorState::Active, None, 0);
        let checking = row(MonitorState::Checking, None, 0);
        assert_eq!(conditions(&[quiet, active, checking]), 0);
    }

    #[test]
    fn each_condition_counts_once() {
        let needs_human = row(MonitorState::Exhausted, None, 0);
        let parked = row(MonitorState::ActionPending, None, 0);
        let stalled = row(MonitorState::Sleeping, Some(t0()), 0);
        let sick = row(MonitorState::Sleeping, None, UNHEALTHY_AFTER_UNKNOWN);
        assert_eq!(conditions(&[needs_human, parked, stalled, sick]), 4);
        // A monitor that is both stalled AND sick is still one monitor.
        let both = row(MonitorState::Sleeping, Some(t0()), UNHEALTHY_AFTER_UNKNOWN);
        assert_eq!(conditions(&[both]), 1);
    }

    #[test]
    fn parses_usage_fields() {
        let u = parse_usage(&serde_json::json!({ "input_tokens": 1000, "output_tokens": 240 }));
        assert_eq!(u.input, 1000);
        assert_eq!(u.output, 240);
    }

    #[test]
    fn context_pct_is_input_over_window() {
        assert_eq!(context_pct(32_000, 100_000), 32);
        assert_eq!(context_pct(0, 100_000), 0);
        assert_eq!(context_pct(100, 0), 0); // no window → 0, never divide by zero
    }

    #[test]
    fn ctx_color_thresholds() {
        assert!(matches!(ctx_color(69), CtxColor::Green));
        assert!(matches!(ctx_color(70), CtxColor::Yellow));
        assert!(matches!(ctx_color(89), CtxColor::Yellow));
        assert!(matches!(ctx_color(90), CtxColor::Red));
    }

    #[test]
    fn cost_none_when_unpriced() {
        let u = UsageCounts {
            input: 1000,
            output: 1000,
        };
        let unpriced = Pricing {
            in_per_1k: None,
            out_per_1k: None,
            window: None,
        };
        assert!(turn_cost(&unpriced, &u).is_none());
        let priced = Pricing {
            in_per_1k: Some(0.003),
            out_per_1k: Some(0.015),
            window: Some(200_000),
        };
        let c = turn_cost(&priced, &u).unwrap();
        assert!((c - 0.018).abs() < 1e-9);
    }

    #[test]
    fn bar_fills_proportionally() {
        assert_eq!(ctx_bar(50, CTX_BAR_WIDTH), "▓▓▓░░░");
        assert_eq!(ctx_bar(0, CTX_BAR_WIDTH), "░░░░░░");
        assert_eq!(ctx_bar(100, CTX_BAR_WIDTH), "▓▓▓▓▓▓");
    }
}
