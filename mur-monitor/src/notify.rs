//! Which recorded events are worth a human's attention, and what each one
//! says (spec §通知策略). Pure: no I/O, no channels, no clock — `render`
//! takes the row and the event and returns text. The delivery side is
//! `store::notify` (the queue) and `mur-core`'s channels.

use crate::state::MonitorState;
use crate::store::{EventRow, MonitorRow};

/// Exactly the kinds §通知策略 lists. Everything else a monitor records —
/// `created`, `retried`, `lease_recovered`, every observation — is
/// bookkeeping and must never reach a user. `hard_deadline` is deliberately
/// absent: crossing it is not itself news (the monitor either keeps polling
/// read-only or emits `exhausted`, and `exhausted` IS notifiable).
pub const NOTIFIABLE: &[&str] = &[
    "stalled",
    "stalled_recovered",
    "soft_deadline",
    "terminal",
    "monitor_unhealthy",
    "exhausted",
];

pub fn is_notifiable(kind: &str) -> bool {
    NOTIFIABLE.contains(&kind)
}

/// One rendered notification. `next_step` is separate from `body` so a
/// channel with a short field (a desktop banner) can lead with it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Notification {
    pub title: String,
    pub body: String,
    pub next_step: String,
}

/// The single action the user should take. §通知策略 requires one — not a
/// list of options, which is how a notification becomes something people
/// dismiss without reading.
fn next_step(row: &MonitorRow, kind: &str) -> String {
    let short = &row.id[..row.id.len().min(13)];
    match kind {
        "stalled" => format!(
            "no progress since {}. Check the source, or `mur monitor show {short} --history`",
            row.last_progress_at.to_rfc3339()
        ),
        "stalled_recovered" => "progress resumed — nothing to do".to_string(),
        "soft_deadline" => {
            format!("running longer than expected. `mur monitor show {short}` for evidence")
        }
        "terminal" => format!(
            "settled as {}. `mur monitor show {short}`",
            row.outcome.as_str()
        ),
        "monitor_unhealthy" => format!(
            "the monitor cannot read its source ({} consecutive unknown checks) — this is a monitor problem, not a failure of the work. Check the credential reference and the source's reachability",
            row.unknown_streak
        ),
        // A hard-deadline exhaustion cannot be retried: `reactivate` leaves
        // `hard_reached` set (a passed deadline is a fact), so `mur monitor
        // retry` refuses. Saying otherwise sends the user at a wall.
        "exhausted" if row.hard_reached => format!(
            "stopped: its hard deadline ({}) passed. Retry cannot help — register a new monitor, or one with a longer `hard_deadline`, if this is still worth watching",
            row.spec.policy.hard_deadline
        ),
        "exhausted" => {
            format!("stopped and needs a human. `mur monitor retry {short}` to re-enable")
        }
        other => format!("`mur monitor show {short}` ({other})"),
    }
}

pub fn render(row: &MonitorRow, event: &EventRow) -> Notification {
    // `actions taken` is required by §通知策略 but no executor exists until
    // the actions plan — the field renders as `—` rather than vanishing,
    // because its absence is information and the shape is the contract.
    let actions = "—";
    let next_check = if matches!(row.state, MonitorState::Completed | MonitorState::Exhausted) {
        "none (settled)".to_string()
    } else {
        row.next_check_at.to_rfc3339()
    };
    Notification {
        title: format!("MUR monitor: {} — {}", row.name, event.kind),
        body: format!(
            "{} · {} {}\noutcome: {} · actions: {} · next check: {}\nat {}",
            row.name,
            row.source_type.as_str(),
            row.reference,
            row.outcome.as_str(),
            actions,
            next_check,
            event.created_at.to_rfc3339(),
        ),
        next_step: next_step(row, &event.kind),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::MonitorSpec;
    use crate::state::{MonitorState, Outcome};
    use chrono::{TimeZone, Utc};

    fn spec() -> MonitorSpec {
        MonitorSpec::from_yaml(
            "schema_version: 1\nname: wait-for-ci\nsource: { type: github_actions, reference: mur-run/mur/123 }\nidempotency_key: k\ncreated_by: { actor: user:test }\n",
        )
        .unwrap()
    }

    fn row(state: MonitorState, outcome: Outcome) -> MonitorRow {
        let t = Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap();
        MonitorRow {
            id: "01a0-aaaa".into(),
            name: "wait-for-ci".into(),
            spec: spec(),
            state,
            outcome,
            source_type: crate::spec::SourceType::GithubActions,
            reference: "mur-run/mur/123".into(),
            idempotency_key: "k".into(),
            created_at: t,
            work_started_at: t,
            next_check_at: t + chrono::Duration::minutes(5),
            last_checked_at: Some(t),
            last_progress_at: t,
            progress_token: None,
            pending_attempts: 3,
            unknown_streak: 0,
            remediation_attempts: 0,
            cycle_id: "cyc".into(),
            stalled_since: None,
            soft_notified: false,
            hard_reached: false,
            fence: 1,
            version: 2,
        }
    }

    fn event(kind: &str) -> EventRow {
        EventRow {
            cycle_id: "cyc".into(),
            kind: kind.into(),
            payload: serde_json::json!({}),
            created_at: Utc.with_ymd_and_hms(2026, 9, 15, 12, 5, 0).unwrap(),
        }
    }

    #[test]
    fn only_the_spec_listed_kinds_notify() {
        for k in [
            "stalled",
            "stalled_recovered",
            "soft_deadline",
            "terminal",
            "monitor_unhealthy",
            "exhausted",
        ] {
            assert!(is_notifiable(k), "{k} must notify");
        }
        // Routine bookkeeping must never reach a user.
        for k in [
            "created",
            "retried",
            "lease_recovered",
            "observed",
            "cancelled",
            "hard_deadline",
        ] {
            assert!(!is_notifiable(k), "{k} must NOT notify");
        }
    }

    #[test]
    fn every_message_carries_the_six_required_fields() {
        // spec §通知策略: name, source reference, known outcome, actions
        // taken, next check time, and the single step for the user.
        let n = render(
            &row(MonitorState::Sleeping, Outcome::Pending),
            &event("stalled"),
        );
        for needle in [
            "wait-for-ci",
            "mur-run/mur/123",
            "pending",
            "—",
            "2026-09-15T12:05",
            "mur monitor show",
        ] {
            assert!(
                n.body.contains(needle) || n.next_step.contains(needle),
                "missing {needle:?} in body={:?} next_step={:?}",
                n.body,
                n.next_step
            );
        }
        assert!(n.title.contains("MUR"), "brand is uppercase: {:?}", n.title);
    }

    #[test]
    fn the_next_step_differs_by_kind_and_is_never_empty() {
        let mut steps = std::collections::HashSet::new();
        for k in NOTIFIABLE {
            let n = render(&row(MonitorState::Sleeping, Outcome::Pending), &event(k));
            assert!(!n.next_step.trim().is_empty(), "{k} has no next step");
            steps.insert(n.next_step.clone());
        }
        assert!(
            steps.len() > 1,
            "every kind got the same next step — the field is decoration"
        );
    }

    #[test]
    fn an_exhausted_monitor_is_told_retry_will_not_help_when_the_deadline_passed() {
        let mut r = row(MonitorState::Exhausted, Outcome::Unknown);
        r.hard_reached = true;
        let n = render(&r, &event("exhausted"));
        assert!(
            n.next_step.contains("hard deadline") && !n.next_step.contains("mur monitor retry"),
            "must not suggest a retry that refuses: {:?}",
            n.next_step
        );
    }

    #[test]
    fn no_credential_reference_value_reaches_the_message() {
        let mut r = row(MonitorState::Sleeping, Outcome::Unknown);
        r.spec.source.credential_ref = Some("keychain:mur/github-token".into());
        let n = render(&r, &event("monitor_unhealthy"));
        let all = format!("{} {} {}", n.title, n.body, n.next_step);
        assert!(
            !all.contains("keychain:"),
            "a credential reference must not be rendered: {all}"
        );
    }
}
