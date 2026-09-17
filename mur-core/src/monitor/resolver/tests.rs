//! `resolver` tests, in their own file per CLAUDE.md §4.
//!
//! The shape these aim at: every rejection must be TOTAL. A test that only
//! checks `is_err()` would pass even if a future edit downgraded a bad verb
//! to `notify`, so each one asserts the specific error too.

use super::*;

#[test]
fn a_well_formed_proposal_parses_with_its_params() {
    let p = parse_proposal(
        r#"{"action_type":"rerun","params":{"job":"build"},"reason":"flaky on arm64"}"#,
    )
    .expect("should parse");
    assert_eq!(p.action.r#type, "rerun");
    assert_eq!(p.reason, "flaky on arm64");
    assert_eq!(
        p.action.params.get("job").and_then(|v| v.as_str()),
        Some("build"),
        "params must survive into the action the claim path will use"
    );
}

#[test]
fn params_may_be_absent() {
    let p = parse_proposal(r#"{"action_type":"collect_logs","reason":"need the tail"}"#)
        .expect("params is optional");
    assert!(p.action.params.is_empty());
}

/// Property 1. Not `is_err()`: the point is that the verb is NOT swapped for
/// a safe one, so the error has to name what was asked for.
#[test]
fn an_unknown_verb_voids_the_whole_response() {
    let err = parse_proposal(
        r#"{"action_type":"bump_dependency","params":{"crate":"serde"},"reason":"outdated"}"#,
    )
    .expect_err("an unproposable verb must be refused");
    assert_eq!(
        err,
        ProposalError::UnknownActionType("bump_dependency".into()),
        "the response must be void, not downgraded to a safe verb"
    );
}

/// The regression that catches `PROPOSABLE` being wired to `risk::classify`'s
/// table. `reschedule_monitor` is classified there (Read tier) but has no
/// executor, so a proposal naming it would park an action that can never
/// run. If this test goes green-by-acceptance, the two sets have been merged.
#[test]
fn a_classified_verb_with_no_executor_is_still_not_proposable() {
    let err = parse_proposal(r#"{"action_type":"reschedule_monitor","reason":"retry later"}"#)
        .expect_err("classified elsewhere, but not proposable here");
    assert_eq!(
        err,
        ProposalError::UnknownActionType("reschedule_monitor".into())
    );
    assert!(
        !PROPOSABLE.contains(&"reschedule_monitor"),
        "the closed set must stay narrower than the risk table"
    );
}

/// Property 2, locked from the parser's side: nothing in the JSON reaches a
/// place where it could influence gating. A tier named at the top level is
/// dropped, and one smuggled into `params` cannot matter either, because
/// `classify` reads only the verb.
#[test]
fn a_tier_in_the_response_is_never_carried_into_the_action() {
    let p = parse_proposal(
        r#"{"action_type":"notify","tier":"read","risk":"none","reason":"tell the human"}"#,
    )
    .expect("extra keys are ignored, not fatal");
    assert!(
        p.action.params.is_empty(),
        "top-level keys other than params must not leak into the action"
    );
    assert_eq!(
        mur_monitor::action::risk::classify(&p.action.r#type),
        mur_common::hitl::RiskTier::Read,
        "the tier comes from the table keyed on the verb, never from the response"
    );
}

#[test]
fn a_proposal_with_no_reason_is_refused() {
    assert_eq!(
        parse_proposal(r#"{"action_type":"notify"}"#).expect_err("reason is required"),
        ProposalError::MissingReason
    );
    assert_eq!(
        parse_proposal(r#"{"action_type":"notify","reason":"   "}"#)
            .expect_err("blank is not a reason"),
        ProposalError::MissingReason
    );
}

#[test]
fn non_json_and_non_object_responses_are_refused() {
    assert_eq!(
        parse_proposal("I would suggest rerunning the failed job.").expect_err("prose"),
        ProposalError::NotJson
    );
    assert_eq!(
        parse_proposal(r#"["rerun"]"#).expect_err("array"),
        ProposalError::NotAnObject
    );
}

#[test]
fn a_non_object_params_is_refused_rather_than_dropped() {
    assert_eq!(
        parse_proposal(r#"{"action_type":"rerun","params":"job=build","reason":"flaky"}"#)
            .expect_err("params must be an object"),
        ProposalError::ParamsNotAnObject,
        "silently dropping malformed params would run the verb with none"
    );
}

#[test]
fn a_fenced_response_is_accepted() {
    let p = parse_proposal("```json\n{\"action_type\":\"notify\",\"reason\":\"heads up\"}\n```")
        .expect("a fenced block is transport, not content");
    assert_eq!(p.action.r#type, "notify");
}

/// The other side of `unfence`: it is not a scanner. JSON buried in prose is
/// refused, because accepting it would mean accepting responses nobody
/// deliberately formatted — and the next step from there is repairing them.
#[test]
fn json_buried_in_prose_is_not_dug_out() {
    assert_eq!(
        parse_proposal(
            "Here is my answer: {\"action_type\":\"notify\",\"reason\":\"x\"} — hope that helps"
        )
        .expect_err("must not scan for the first brace"),
        ProposalError::NotJson
    );
}
