//! `prompt` tests. The one that matters is the ordering test: everything
//! else here would still pass if `clean` truncated before redacting.

use super::*;

/// A real GitHub PAT shape — `ghp_` plus 36 word characters — because
/// `redact_secrets` matches on patterns, and a made-up "SECRET123" would
/// sail through unredacted and make every test below prove nothing.
const PAT: &str = "ghp_abcdefghijklmnopqrstuvwxyz0123456789";

#[test]
fn the_pat_shape_is_one_the_redactor_actually_matches() {
    // Guard for the guard: if `redact_secrets` ever stops matching this
    // shape, the tests below go quietly useless, so assert it directly.
    assert_eq!(PAT.len(), 40, "ghp_ + 36");
    assert!(
        mur_common::redact::redact_secrets(PAT).contains("REDACTED"),
        "the fixture must be a secret the chokepoint recognises"
    );
}

#[test]
fn secrets_are_redacted_in_every_field_that_carries_source_text() {
    let ctx = Context {
        source_type: "github_actions".into(),
        outcome: "failed".into(),
        error: Some(format!("auth failed with {PAT}")),
        attempted: vec![format!("rerun with {PAT}: failed")],
        log_tail: Some(format!("curl -H 'Authorization: {PAT}'")),
    };
    let out = user_prompt(&ctx);
    assert!(
        !out.contains(PAT),
        "no field may reach the prompt unredacted; got:\n{out}"
    );
    assert_eq!(
        out.matches("REDACTED").count(),
        3,
        "error, attempted and log_tail each carried one"
    );
}

#[test]
fn home_paths_are_collapsed() {
    let ctx = Context {
        error: Some("no such file: /Users/dave/.mur/monitor/monitors.db".into()),
        ..Default::default()
    };
    let out = user_prompt(&ctx);
    assert!(!out.contains("/Users/dave"), "got:\n{out}");
    assert!(
        out.contains("~/"),
        "the path itself is still useful; got:\n{out}"
    );
}

/// The ordering test, and the reason `clean` redacts first.
///
/// Sized so the truncation boundary lands INSIDE the secret. `PAT` plus a
/// space plus 1970 filler is 2011 characters against a 2000 cap, so
/// truncating first would drop the leading 11 characters of the PAT and keep
/// the other 29 — a fragment that no longer matches the redactor and is
/// still most of a live credential. Redacting first turns it into a
/// 21-character label, bringing the whole string to 1992 and under the cap,
/// so nothing is cut at all.
///
/// The separating space is load-bearing, and its absence is what made the
/// first version of this test a false alarm: `redact_secrets` matches
/// `\bghp_[A-Za-z0-9]{36}\b`, so filler appended directly to the PAT
/// removes the trailing word boundary and the pattern cannot match. A
/// fixture the redactor does not recognise would have tested nothing.
#[test]
fn a_secret_straddling_the_truncation_boundary_leaves_no_fragment() {
    let log = format!("{PAT} {}", "X".repeat(1970));
    assert_eq!(log.len(), 2011, "must exceed the cap by less than the PAT");
    assert!(
        mur_common::redact::redact_secrets(&log).contains("REDACTED"),
        "the padded fixture must still be matchable — see the doc above"
    );

    let out = user_prompt(&Context {
        log_tail: Some(log),
        ..Default::default()
    });
    assert!(!out.contains(PAT), "whole secret leaked");
    assert!(
        !out.contains(&PAT[11..]),
        "a truncated fragment of the secret leaked — `clean` truncated before redacting"
    );
    assert!(
        out.contains("REDACTED"),
        "it should be a label; got:\n{out}"
    );
}

#[test]
fn the_log_tail_is_capped() {
    let ctx = Context {
        log_tail: Some("Y".repeat(LOG_TAIL_MAX_CHARS * 3)),
        ..Default::default()
    };
    let out = user_prompt(&ctx);
    assert!(
        out.matches('Y').count() <= LOG_TAIL_MAX_CHARS,
        "an unbounded tail is both a cost and a disclosure surface"
    );
    assert!(
        out.contains('…'),
        "truncation should be visible to the reader"
    );
}

/// Without this line the model re-proposes the action that just failed, and
/// the remediation cap pays for it.
#[test]
fn an_empty_attempted_list_says_so_rather_than_being_omitted() {
    let out = user_prompt(&Context::default());
    assert!(out.contains("already tried: nothing"), "got:\n{out}");

    let tried = user_prompt(&Context {
        attempted: vec!["rerun: failed".into()],
        ..Default::default()
    });
    assert!(tried.contains("rerun: failed"), "got:\n{tried}");
}

/// The prompt and the parser must agree on the verb set. If the prompt ever
/// offered a fourth verb, the model would propose it and `parse_proposal`
/// could only refuse — a loop that spends the resolver's one call per cycle
/// on a reply it was invited to give.
#[test]
fn the_system_prompt_offers_exactly_the_proposable_verbs() {
    let sys = system_prompt();
    for verb in super::super::PROPOSABLE {
        assert!(sys.contains(verb), "{verb} must be offered; got:\n{sys}");
    }
    assert!(
        sys.contains("voids your whole reply"),
        "the model must be told refusal is total"
    );
    assert!(
        sys.contains("not consulted"),
        "the model must be told it does not set the tier"
    );
}
