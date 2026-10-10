//! P3b-§3.1 grammar: AC-P3b-1, AC-P3b-2, AC-P3b-6 and the syntax half of
//! AC-P3b-5.

use super::args::{ReviewLine, parse_review_line};
use crate::cmd::fleet::review::constants::REVIEW_USAGE_MURMUR;
use crate::cmd::fleet::review::session::ReviewArgs;

fn start(line: &str) -> ReviewArgs {
    match parse_review_line(line) {
        Ok(ReviewLine::Start(a)) => a,
        other => panic!("expected Start for {line:?}, got {other:?}"),
    }
}

fn syntax_err(line: &str) -> String {
    match parse_review_line(line) {
        Err(e) => e,
        other => panic!("expected a syntax error for {line:?}, got {other:?}"),
    }
}

#[test]
fn ac_p3b_1_args_equal_what_the_cli_builds_for_the_same_flags() {
    // `dispatch/fleet.rs` builds exactly these fields from clap.
    let cli = ReviewArgs {
        main: "a".into(),
        reviewer: "b".into(),
        task: "fix the bug".into(),
        deadline: Some("30m".into()),
        budget_usd: Some(2.0),
    };
    let line = "--main a --reviewer b --deadline 30m --budget-usd 2 fix the bug";
    assert_eq!(start(line), cli);
}

#[test]
fn flags_may_come_in_any_order_and_optionals_default_to_none() {
    let a = start("--reviewer b --main a fix it");
    assert_eq!(a.main, "a");
    assert_eq!(a.reviewer, "b");
    assert_eq!(a.deadline, None);
    assert_eq!(a.budget_usd, None);
    assert_eq!(a.task, "fix it");
}

#[test]
fn task_keeps_its_own_spacing_and_runs_to_end_of_line() {
    let a = start("--main a --reviewer b   fix   the  bug  ");
    assert_eq!(a.task, "fix   the  bug");
}

#[test]
fn ac_p3b_2_task_after_double_dash_starting_with_dash_is_verbatim() {
    let a = start("--main a --reviewer b -- -x task");
    assert_eq!(a.task, "-x task");
}

#[test]
fn ac_p3b_2_flag_after_the_task_is_part_of_the_task() {
    let a = start("--main a --reviewer b task --main b");
    assert_eq!(a.main, "a");
    assert_eq!(a.task, "task --main b");
}

#[test]
fn auto_before_the_task_is_refused() {
    assert_eq!(
        parse_review_line("--auto --main a --reviewer b task"),
        Ok(ReviewLine::AutoRefused)
    );
}

#[test]
fn auto_between_flags_is_refused() {
    assert_eq!(
        parse_review_line("--main a --auto --reviewer b task"),
        Ok(ReviewLine::AutoRefused)
    );
}

#[test]
fn auto_after_the_task_is_refused_too() {
    // Silently starting a semi-auto session with "--auto" typed into the
    // task text would be a lie about what the user asked for.
    assert_eq!(
        parse_review_line("--main a --reviewer b fix it --auto"),
        Ok(ReviewLine::AutoRefused)
    );
}

#[test]
fn auto_after_double_dash_is_task_text() {
    let a = start("--main a --reviewer b -- explain --auto in the docs");
    assert_eq!(a.task, "explain --auto in the docs");
}

#[test]
fn resume_takes_one_session_name() {
    assert_eq!(
        parse_review_line("resume SESS-X"),
        Ok(ReviewLine::Resume("SESS-X".into()))
    );
}

#[test]
fn empty_and_blank_lines_are_bare() {
    assert_eq!(parse_review_line(""), Ok(ReviewLine::Bare));
    assert_eq!(parse_review_line("   "), Ok(ReviewLine::Bare));
}

#[test]
fn ac_p3b_5_missing_reviewer_is_a_syntax_error_ending_in_the_usage() {
    let e = syntax_err("--main a fix it");
    assert!(e.contains("--reviewer"), "{e}");
    assert!(e.ends_with(REVIEW_USAGE_MURMUR), "{e}");
}

#[test]
fn missing_main_is_a_syntax_error() {
    let e = syntax_err("--reviewer b fix it");
    assert!(e.contains("--main"), "{e}");
    assert!(e.ends_with(REVIEW_USAGE_MURMUR), "{e}");
}

#[test]
fn empty_task_is_a_syntax_error() {
    for line in ["--main a --reviewer b", "--main a --reviewer b --"] {
        let e = syntax_err(line);
        assert!(e.contains("task"), "{line}: {e}");
        assert!(e.ends_with(REVIEW_USAGE_MURMUR), "{line}: {e}");
    }
}

#[test]
fn unknown_flag_is_named() {
    let e = syntax_err("--main a --reviewer b --bogus x task");
    assert!(e.contains("--bogus"), "{e}");
    assert!(e.ends_with(REVIEW_USAGE_MURMUR), "{e}");
}

#[test]
fn flag_without_a_value_is_an_error_not_a_swallowed_flag() {
    // `--main --reviewer b` must not take "--reviewer" as the agent name.
    let e = syntax_err("--main --reviewer b task");
    assert!(e.contains("--main"), "{e}");
    let e = syntax_err("--main a --reviewer");
    assert!(e.contains("--reviewer"), "{e}");
}

#[test]
fn budget_must_be_a_finite_number() {
    for bad in ["abc", "NaN", "inf"] {
        let e = syntax_err(&format!("--main a --reviewer b --budget-usd {bad} task"));
        assert!(e.contains("--budget-usd"), "{bad}: {e}");
    }
}

#[test]
fn resume_without_a_name_or_with_extra_words_is_a_syntax_error() {
    for line in ["resume", "resume a b"] {
        let e = syntax_err(line);
        assert!(e.ends_with(REVIEW_USAGE_MURMUR), "{line}: {e}");
    }
}

#[test]
fn a_task_that_merely_starts_with_resume_is_not_a_resume() {
    // Only the first word, only when it is exactly `resume`.
    let a = start("--main a --reviewer b resume the migration");
    assert_eq!(a.task, "resume the migration");
}
