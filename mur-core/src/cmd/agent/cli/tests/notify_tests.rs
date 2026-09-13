//! `notify_tests`, moved out of `cli/mod.rs` for CLAUDE.md §4's 800-line rule.
//! Pure movement: dedented one level, nothing else.

use super::super::notify_script;

#[test]
fn notify_script_escapes_quotes() {
    let s = notify_script("rustsmith", r#"finished "the" task"#);
    assert!(s.contains("display notification"));
    // clean title → PLAIN quote delimiters
    assert!(s.contains(r#"with title "rustsmith""#));
    // embedded quotes in the message ARE escaped to \"
    assert!(s.contains(r#"finished \"the\" task"#));
    // the full message is wrapped in plain delimiters
    assert!(s.contains(r#"display notification "finished \"the\" task""#));
}
