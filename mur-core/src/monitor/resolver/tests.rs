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

/// `ask` tests. A mock client, so these exercise the real prompt builder and
/// the real parser end to end without a network.
mod ask_tests {
    use super::super::*;
    use mur_common::error::LlmError;
    use mur_common::llm::LlmClient;
    use std::sync::Mutex;

    /// Records what it was asked, so a test can assert the call carried a
    /// system prompt and a redacted user prompt rather than just checking
    /// the return value.
    struct MockLlm {
        reply: Result<String, String>,
        seen: Mutex<Option<(String, Option<String>)>>,
    }

    impl MockLlm {
        fn ok(reply: &str) -> Self {
            Self {
                reply: Ok(reply.to_string()),
                seen: Mutex::new(None),
            }
        }
        fn err(msg: &str) -> Self {
            Self {
                reply: Err(msg.to_string()),
                seen: Mutex::new(None),
            }
        }
    }

    impl LlmClient for MockLlm {
        fn complete(
            &self,
            prompt: &str,
            system: Option<&str>,
        ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
            *self.seen.lock().unwrap() = Some((prompt.to_string(), system.map(str::to_string)));
            let r = self.reply.clone();
            async move { r.map_err(LlmError::Request) }
        }

        async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
            Ok(vec![])
        }
    }

    fn ctx() -> prompt::Context {
        prompt::Context {
            source_type: "github_actions".into(),
            outcome: "failed".into(),
            error: Some("job `build` exited 1".into()),
            attempted: vec!["rerun: failed".into()],
            log_tail: None,
        }
    }

    #[tokio::test]
    async fn a_usable_reply_becomes_a_proposal() {
        let m = MockLlm::ok(r#"{"action_type":"collect_logs","reason":"need the failing step"}"#);
        let p = ask(&m, &ctx()).await.expect("should propose");
        assert_eq!(p.action.r#type, "collect_logs");
        assert_eq!(p.reason, "need the failing step");
    }

    /// The call must carry both halves. A system prompt dropped to `None`
    /// would leave the model with no verb list and no format, and it would
    /// still "work" often enough to pass a test that only checked the happy
    /// path's return value.
    #[tokio::test]
    async fn the_call_carries_the_system_prompt_and_the_context() {
        let m = MockLlm::ok(r#"{"action_type":"notify","reason":"nothing to retry"}"#);
        ask(&m, &ctx()).await.expect("should propose");
        let (user, system) = m.seen.lock().unwrap().clone().expect("client was called");
        let system = system.expect("a system prompt must be sent");
        for verb in PROPOSABLE {
            assert!(system.contains(verb), "system prompt lost {verb}");
        }
        assert!(user.contains("github_actions"), "context lost the source");
        assert!(
            user.contains("rerun: failed"),
            "context lost what was already tried, so the model will re-propose it"
        );
    }

    #[tokio::test]
    async fn an_unusable_reply_is_refused_not_guessed_at() {
        let m = MockLlm::ok(r#"{"action_type":"rm_minus_rf","reason":"clean slate"}"#);
        match ask(&m, &ctx()).await {
            Err(ResolverError::Refused(ProposalError::UnknownActionType(t))) => {
                assert_eq!(t, "rm_minus_rf");
            }
            other => panic!("an unlisted verb must be refused, got {other:?}"),
        }
    }

    /// A consultation that could not happen is not a work failure — the
    /// monitor is left where it was, which is why this is its own arm.
    #[tokio::test]
    async fn an_unreachable_model_is_its_own_error() {
        let m = MockLlm::err("connection refused");
        match ask(&m, &ctx()).await {
            Err(ResolverError::Unreachable(e)) => assert!(e.contains("connection refused")),
            other => panic!("expected Unreachable, got {other:?}"),
        }
    }
}
