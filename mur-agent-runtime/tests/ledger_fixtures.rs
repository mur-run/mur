//! Ledger fixture tests (M7.3).
//!
//! Two fixture files live under `tests/fixtures/ledger/`:
//!
//! * `v_current_full_coverage.jsonl` — one `OutboxEvent` per variant in the
//!   *current* schema.  Re-generate by running the `regenerate_v_current_fixture`
//!   ignored test, then committing the result.
//! * `v1_frozen.jsonl` — the schema baseline written on 2026-04-29 (day of
//!   spec).  **DO NOT regenerate or edit this file after the initial commit.**
//!   If a schema change breaks deserialization of this file that is intentional:
//!   the failing test tells you to add `#[serde(default)]` or write a migration,
//!   not to update the fixture.

use chrono::TimeZone;
use mur_agent_runtime::companion::telemetry::OutboxEvent;
use mur_common::companion::{Relationship, Signal, Situation};

const V_CURRENT_PATH: &str = "tests/fixtures/ledger/v_current_full_coverage.jsonl";
const V1_FROZEN_PATH: &str = "tests/fixtures/ledger/v1_frozen.jsonl";

/// Every variant tag that exists in `OutboxEvent`.
///
/// When you add a variant you MUST:
/// 1. Add its tag here.
/// 2. Add a corresponding instance to `build_full_coverage`.
/// 3. Re-run `regenerate_v_current_fixture` and commit the updated
///    `v_current_full_coverage.jsonl`.
const VARIANT_TAGS: &[&str] = &[
    "CompanionInitialized",
    "RelationshipChanged",
    "QuietRequested",
    "MessageScheduled",
    "MessageGenerated",
    "MessagePaused",
    "MessageSent",
    "MessageDropped",
    "UserSignal",
    "PassiveDismissInferred",
    "LocaleMismatchUnresolved",
    "VoiceMdComposed",
    "RhythmWiped",
];

fn fixed_ts() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc.with_ymd_and_hms(2026, 4, 29, 12, 0, 0).unwrap()
}

/// Returns exactly one instance of every `OutboxEvent` variant with
/// deterministic, human-readable sentinel values.
fn build_full_coverage() -> Vec<OutboxEvent> {
    let ts = fixed_ts();
    vec![
        OutboxEvent::CompanionInitialized { at: ts, version: 1 },
        OutboxEvent::RelationshipChanged {
            old: Relationship::Friend,
            new: Relationship::Coach,
            at: ts,
        },
        OutboxEvent::QuietRequested {
            until: ts,
            reason: "user_requested".into(),
            at: ts,
        },
        OutboxEvent::MessageScheduled {
            id: "msg-001".into(),
            situation: Situation::MorningGreeting,
            template_id: "tmpl-a".into(),
            scheduled_for: ts,
        },
        OutboxEvent::MessageGenerated {
            id: "msg-001".into(),
            locale_used: "en-US".into(),
            body_sha256: "abc123".into(),
            linter_violations: 0,
            regen_count: 0,
        },
        OutboxEvent::MessagePaused {
            id: "msg-001".into(),
            resume_at: ts,
            reason: "rate_limit_429".into(),
        },
        OutboxEvent::MessageSent {
            id: "msg-001".into(),
            channel: "stdout".into(),
            sent_at: ts,
        },
        OutboxEvent::MessageDropped {
            id: "msg-001".into(),
            reason: "permission_denied".into(),
        },
        OutboxEvent::UserSignal {
            id: "msg-001".into(),
            signal: Signal::Positive,
            at: ts,
        },
        OutboxEvent::PassiveDismissInferred {
            id: "msg-001".into(),
            at: ts,
        },
        OutboxEvent::LocaleMismatchUnresolved {
            id: "msg-001".into(),
            attempts: 4,
            at: ts,
        },
        OutboxEvent::VoiceMdComposed {
            relationship: Relationship::Friend,
            locale_used: "en-US".into(),
            fallback_from: None,
            at: ts,
        },
        OutboxEvent::RhythmWiped { at: ts },
    ]
}

fn fixture_jsonl(events: &[OutboxEvent]) -> String {
    let lines: Vec<String> = events
        .iter()
        .map(|e| serde_json::to_string(e).expect("serialize OutboxEvent"))
        .collect();
    lines.join("\n") + "\n"
}

// ─── Non-ignored tests ───────────────────────────────────────────────────────

/// Assert that the on-disk `v_current_full_coverage.jsonl` matches the output
/// of `build_full_coverage()`.  Fails when a field is renamed/added/removed
/// without regenerating the fixture.
#[test]
fn v_current_matches_serialized_today() {
    let events = build_full_coverage();
    let expected = fixture_jsonl(&events);
    let actual =
        std::fs::read_to_string(V_CURRENT_PATH).expect("read v_current_full_coverage.jsonl");
    assert_eq!(
        actual, expected,
        "v_current_full_coverage.jsonl is out of date — regenerate with:\n\
         cargo test -p mur-agent-runtime --test ledger_fixtures \
         regenerate_v_current_fixture -- --ignored\n\
         then commit the updated file.\n\
         --- expected ---\n{expected}"
    );
}

/// Assert that every variant tag listed in `VARIANT_TAGS` appears at least
/// once in `v_current_full_coverage.jsonl`.
#[test]
fn v_current_covers_all_variants() {
    let body = std::fs::read_to_string(V_CURRENT_PATH).expect("read v_current_full_coverage.jsonl");
    for tag in VARIANT_TAGS {
        let needle = format!(r#""event":"{tag}""#);
        assert!(
            body.contains(&needle),
            "v_current_full_coverage.jsonl is missing variant `{tag}`.\n\
             Either it was added without updating the fixture, or removed \
             without updating VARIANT_TAGS.\n\
             Regenerate with:\n  cargo test -p mur-agent-runtime --test \
             ledger_fixtures regenerate_v_current_fixture -- --ignored"
        );
    }
}

/// Deserialize every line of the FROZEN v1 fixture into `OutboxEvent`.
/// A failure here means a schema change broke backward compatibility with the
/// v1 ledger format.  Fix the schema (add `#[serde(default)]`, etc.) rather
/// than updating this fixture.
#[test]
fn v1_frozen_deserializes_into_current_schema() {
    let body = std::fs::read_to_string(V1_FROZEN_PATH).expect("read v1_frozen.jsonl");
    for (i, line) in body.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
        let _: OutboxEvent = serde_json::from_str(line).unwrap_or_else(|e| {
            panic!(
                "v1_frozen.jsonl line {} failed to deserialize: {e}\n  line: {line}",
                i + 1
            )
        });
    }
}

// ─── Fixture generators (ignored — run once to bootstrap, then commit) ───────

/// Write `v_current_full_coverage.jsonl`.
///
/// Run with:
/// ```
/// cargo test -p mur-agent-runtime --test ledger_fixtures \
///   regenerate_v_current_fixture -- --ignored
/// ```
/// Commit the result.  Never run this for `v1_frozen.jsonl` after the
/// initial bootstrap.
#[test]
#[ignore]
fn regenerate_v_current_fixture() {
    let events = build_full_coverage();
    let body = fixture_jsonl(&events);
    std::fs::write(V_CURRENT_PATH, &body).expect("write v_current_full_coverage.jsonl");
    println!("Wrote {V_CURRENT_PATH}");
}

/// Bootstrap `v1_frozen.jsonl` — identical to v_current on day-of-spec.
///
/// **Run ONCE at schema birth, then never again.**
/// ```
/// cargo test -p mur-agent-runtime --test ledger_fixtures \
///   bootstrap_v1_frozen_fixture -- --ignored
/// ```
/// Commit the result.  DO NOT regenerate after the initial commit.
#[test]
#[ignore]
fn bootstrap_v1_frozen_fixture() {
    let events = build_full_coverage();
    let body = fixture_jsonl(&events);
    std::fs::write(V1_FROZEN_PATH, &body).expect("write v1_frozen.jsonl");
    println!("Wrote {V1_FROZEN_PATH}");
}
