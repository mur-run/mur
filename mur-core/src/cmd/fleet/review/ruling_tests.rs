use std::collections::BTreeSet;

use super::constants::{RULE_USAGE_HINT, RULING_PROMPT_HINT};
use super::ruling::{PromptLine, RulingInput, classify_ruling_line, parse_rule_command};
use super::schema::RulingDecision;

fn open(ids: &[&str]) -> BTreeSet<String> {
    ids.iter().map(|s| s.to_string()).collect()
}

fn classify(raw: &str) -> PromptLine {
    classify_ruling_line(raw, &open(&["F1", "F2"]))
}

#[test]
fn eof_and_q_leave_paused() {
    assert_eq!(classify(""), PromptLine::Leave, "EOF");
    assert_eq!(classify("q\n"), PromptLine::Leave);
    assert_eq!(classify("Q"), PromptLine::Leave);
}

/// Enter is not leave: only `q` and EOF are (P2-§5.2, R8).
#[test]
fn enter_is_not_leave() {
    assert_eq!(
        classify("\n"),
        PromptLine::Other(RULING_PROMPT_HINT.to_string())
    );
}

#[test]
fn abandon_is_explicit() {
    assert_eq!(classify("/abandon\n"), PromptLine::Abandon);
}

#[test]
fn valid_rule_parses() {
    assert_eq!(
        classify("/rule drop F1 dup of F2\n"),
        PromptLine::Rule(RulingInput {
            finding: "F1".into(),
            decision: RulingDecision::Drop,
            text: "dup of F2".into(),
        })
    );
    assert_eq!(
        classify("/rule fix F2 must handle EOF"),
        PromptLine::Rule(RulingInput {
            finding: "F2".into(),
            decision: RulingDecision::Fix,
            text: "must handle EOF".into(),
        })
    );
}

#[test]
fn malformed_rule_gives_usage() {
    let usage = PromptLine::Other(RULE_USAGE_HINT.to_string());
    assert_eq!(classify("/rule fix F1"), usage, "no text");
    assert_eq!(classify("/rule fix F1   \n"), usage, "blank text");
    assert_eq!(classify("/rule keep F1 x"), usage, "bad decision");
    assert_eq!(classify("/rule"), usage, "bare");
}

/// P2-§5.4: closed or unknown findings are refused at input, naming the id.
#[test]
fn rule_on_finding_outside_open_set_is_refused() {
    let PromptLine::Other(hint) = classify("/rule drop F7 x") else {
        panic!("expected a hint");
    };
    assert!(hint.contains("F7"), "{hint}");
    assert_ne!(hint, RULE_USAGE_HINT);
}

#[test]
fn other_input_gets_the_prompt_hint() {
    let hint = PromptLine::Other(RULING_PROMPT_HINT.to_string());
    assert_eq!(classify("hello"), hint);
    assert_eq!(classify("/rules drop F1 x"), hint, "not the /rule command");
}

/// The send prompt reuses the same parser (P2-§5.3).
#[test]
fn parse_rule_command_is_shared() {
    let open = open(&["F3"]);
    assert!(parse_rule_command("/rule fix F3 do it", &open).is_ok());
    assert_eq!(
        parse_rule_command("/rule fix", &open),
        Err(RULE_USAGE_HINT.to_string())
    );
    assert!(
        parse_rule_command("/rule drop F1 x", &open)
            .unwrap_err()
            .contains("F1")
    );
}
