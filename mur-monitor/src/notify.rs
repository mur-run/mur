//! Which recorded events are worth a human's attention, and what each one
//! says (spec §通知策略). Pure: no I/O, no channels, no clock — `render`
//! takes the row and the event and returns text. The delivery side is
//! `store::notify` (the queue) and `mur-core`'s channels.

use crate::action::verb_and_index_from_key;
use crate::state::MonitorState;
use crate::store::{ActionRow, EventRow, MonitorRow};

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
    // spec §通知策略 lists 「需要 approval」 and 「自動補救失敗或達嘗試上限」
    // as triggers. The notifications plan deferred both with "no approval
    // exists until the actions plan; NOTIFIABLE gains the kind then" — this
    // is that plan. Without `approval_required` a gated action parks and
    // nobody is ever told, which is a monitor that has silently stopped.
    "approval_required",
    "remediation_failed",
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

/// Short display length for monitor IDs. Must match `mur-core::cmd::monitor::ID_SHORT`
/// (cannot be imported due to architectural boundary: `mur-monitor` sits below `mur-core`).
const ID_SHORT_LEN: usize = 13;

/// The single action the user should take. §通知策略 requires one — not a
/// list of options, which is how a notification becomes something people
/// dismiss without reading.
fn next_step(row: &MonitorRow, kind: &str) -> String {
    let short = &row.id[..row.id.len().min(ID_SHORT_LEN)];
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
            "monitor cannot read source ({} unknown checks) — this is a monitor issue, not work failure. `mur monitor show {short}` to diagnose",
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
        "approval_required" => format!(
            "waiting for you: `mur monitor show {short}` names the action and the exact `mur channel approve` command that releases it"
        ),
        "remediation_failed" => {
            format!("an automatic remedy failed. `mur monitor show {short}` for what was tried")
        }
        other => format!("`mur monitor show {short}` ({other})"),
    }
}

/// Rendered when a monitor has no action rows for this episode at all.
/// §通知策略 requires the 「執行過的動作」 field in every notification, so it
/// stays present and says "nothing" rather than vanishing.
const NO_ACTIONS: &str = "—";

/// How many actions are named before the field collapses to a count. A
/// monitor may carry a list of ten; a desktop banner may not.
const ACTIONS_IN_BODY: usize = 3;

/// The 「執行過的動作」 field: what this monitor's actions actually did,
/// oldest first, as `<verb> <state>`. The caller passes the rows for the
/// row's own cycle — `render` stays pure, with no store of its own.
fn actions_summary(actions: &[ActionRow]) -> String {
    if actions.is_empty() {
        return NO_ACTIONS.to_string();
    }
    let named: Vec<String> = actions
        .iter()
        .take(ACTIONS_IN_BODY)
        .map(|a| {
            let verb =
                verb_and_index_from_key(&a.action_key).map_or(a.action_key.as_str(), |(v, _)| v);
            format!("{verb} {}", a.state.as_str())
        })
        .collect();
    // `named.len()` is `min(ACTIONS_IN_BODY, actions.len())`, so this never
    // underflows.
    let rest = actions.len() - named.len();
    if rest == 0 {
        named.join(", ")
    } else {
        format!("{}, +{rest} more", named.join(", "))
    }
}

/// `actions` are the monitor's action rows for the episode being reported,
/// resolved by the caller (`drain_with` filters `actions_for` to the row's
/// `cycle_id`). Passed in rather than read here so this stays a pure
/// function of its inputs — the property that makes the whole table of
/// tests below addressable without a store.
pub fn render(row: &MonitorRow, event: &EventRow, actions: &[ActionRow]) -> Notification {
    let actions = actions_summary(actions);
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
    use crate::action::ActionState;
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
            next_check_at: t + chrono::Duration::minutes(10),
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
            &[],
        );
        for needle in [
            "wait-for-ci",
            "mur-run/mur/123",
            "pending",
            "actions: —",
            "next check: 2026-09-15T12:10",
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

    fn action_row(key_verb: &str, index: usize, state: ActionState) -> ActionRow {
        ActionRow {
            action_key: crate::action::action_key("m1", "c1", 1, key_verb, index),
            monitor_id: "m1".into(),
            cycle_id: "c1".into(),
            risk: mur_common::hitl::RiskTier::Read,
            approval_id: None,
            state,
            attempt: 0,
            result: None,
            created_at: Utc.with_ymd_and_hms(2026, 9, 15, 12, 0, 0).unwrap(),
            proposed_action: None,
        }
    }

    /// M5 (whole-branch review). The 「執行過的動作」 field §通知策略 requires
    /// was a hardcoded `—` with a comment saying no executor existed yet —
    /// false since the actions slice shipped. An `approval_required` or
    /// `remediation_failed` banner said `actions: —` and named nothing.
    ///
    /// Both the verb AND the state are asserted, and against a row whose
    /// verb is only recoverable from the `action_key`: a summary that
    /// printed just the states, or just a count, passes neither.
    #[test]
    fn the_actions_field_names_what_the_monitor_actually_did() {
        let n = render(
            &row(MonitorState::AwaitingApproval, Outcome::Failed),
            &event("approval_required"),
            &[
                action_row("collect_logs", 0, ActionState::Done),
                action_row("rerun", 1, ActionState::Blocked),
            ],
        );
        assert!(
            n.body.contains("actions: collect_logs done, rerun blocked"),
            "the field must name each action and its state: {:?}",
            n.body
        );
        assert!(
            !n.body.contains(&format!("actions: {NO_ACTIONS}")),
            "the em-dash is for a monitor with no actions, not a placeholder: {:?}",
            n.body
        );
    }

    /// The field is bounded: a long list must not turn a desktop banner
    /// into a wall of text. `ACTIONS_IN_BODY + 2` rows, so the overflow
    /// count is 2 — a hardcoded "+1 more" goes red.
    #[test]
    fn a_long_action_list_is_summarised_rather_than_dumped() {
        let rows: Vec<_> = (0..ACTIONS_IN_BODY + 2)
            .map(|i| action_row("notify", i, ActionState::Done))
            .collect();
        let n = render(
            &row(MonitorState::Completed, Outcome::Failed),
            &event("terminal"),
            &rows,
        );
        assert!(n.body.contains("+2 more"), "{:?}", n.body);
        assert_eq!(
            n.body.matches("notify done").count(),
            ACTIONS_IN_BODY,
            "exactly the cap is named, the rest counted: {:?}",
            n.body
        );
    }

    #[test]
    fn the_next_step_differs_by_kind_and_is_never_empty() {
        let mut steps = std::collections::HashSet::new();
        for k in NOTIFIABLE {
            let n = render(
                &row(MonitorState::Sleeping, Outcome::Pending),
                &event(k),
                &[],
            );
            assert!(!n.next_step.trim().is_empty(), "{k} has no next step");
            steps.insert(n.next_step.clone());
        }
        assert!(
            steps.len() == NOTIFIABLE.len(),
            "every kind got the same next step — the field is decoration"
        );
    }

    #[test]
    fn an_exhausted_monitor_is_told_retry_will_not_help_when_the_deadline_passed() {
        let mut r = row(MonitorState::Exhausted, Outcome::Unknown);
        r.hard_reached = true;
        let n = render(&r, &event("exhausted"), &[]);
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
        let n = render(&r, &event("monitor_unhealthy"), &[]);
        let all = format!("{} {} {}", n.title, n.body, n.next_step);
        assert!(
            !all.contains("keychain:"),
            "a credential reference must not be rendered: {all}"
        );
    }
}
