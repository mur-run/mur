//! P3b-§6.5: the footer label is built only from `turn_sent` and the
//! cumulative usage already on the channel. Expected strings are literals.

use chrono::{DateTime, Duration, TimeZone, Utc};
use mur_common::channel::{ChannelActor, ChannelEvent, EventKind};

use super::render::{LabelFacts, review_label};
use crate::cmd::fleet::review::schema::{
    Cumulative, ReviewPayload, Role, VerdictKind, to_note_payload,
};

const NAME: &str = "review-ab190001";

fn t0() -> DateTime<Utc> {
    Utc.with_ymd_and_hms(2026, 10, 7, 9, 0, 0).unwrap()
}

fn ev(seq: u64, ts: DateTime<Utc>, p: &ReviewPayload) -> ChannelEvent {
    ChannelEvent {
        seq,
        ts,
        actor: ChannelActor::System,
        kind: EventKind::Note,
        payload: to_note_payload(p),
        idempotency_key: None,
        sig: None,
        key_version: None,
    }
}

fn sent(to: Role) -> ReviewPayload {
    ReviewPayload::TurnSent {
        round: 1,
        to,
        restart_note: None,
        human_wait_ms: 0,
    }
}

fn verdict(cost_usd_micros: u64) -> ReviewPayload {
    ReviewPayload::Verdict {
        round: 1,
        kind: VerdictKind::Revise,
        cumulative: Cumulative {
            exec_time_ms: 1_000,
            cost_usd_micros,
        },
    }
}

/// Two sends plus usage: elapsed runs from the first `turn_sent`, the cost is
/// the highest cumulative, written as a lower bound.
#[test]
fn label_from_two_turns_shows_elapsed_and_cost_lower_bound() {
    let events = [
        ev(1, t0(), &sent(Role::Main)),
        ev(2, t0() + Duration::seconds(30), &sent(Role::Reviewer)),
        ev(3, t0() + Duration::seconds(31), &verdict(1_500_000)),
    ];
    let facts = LabelFacts::from_events(&events);

    let label = review_label(NAME, &facts, t0() + Duration::seconds(125));

    assert_eq!(label, "review review-ab190001 · 2m5s · ≥ $1.50");
}

/// Nothing sent yet: no elapsed part, and the cost reads `≥ $0.00`.
#[test]
fn label_without_turn_sent_is_zero_cost_and_no_elapsed() {
    let facts = LabelFacts::from_events(&[]);

    assert_eq!(
        review_label(NAME, &facts, t0()),
        "review review-ab190001 · ≥ $0.00"
    );
}

/// Usage from a later round raises the bound; a stale lower figure never
/// lowers it (events are folded by max, not last).
#[test]
fn label_cost_is_the_highest_cumulative() {
    let events = [
        ev(1, t0(), &sent(Role::Main)),
        ev(2, t0(), &verdict(2_250_000)),
        ev(3, t0(), &verdict(400_000)),
    ];

    let facts = LabelFacts::from_events(&events);

    assert_eq!(facts.cost_micros, 2_250_000);
    assert_eq!(facts.first_sent, Some(t0()));
}
