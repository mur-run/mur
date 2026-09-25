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

#[test]
fn bundle_id_lookup_covers_common_terminals() {
    use super::super::bundle_id_for_term_program;
    assert_eq!(
        bundle_id_for_term_program("ghostty"),
        Some("com.mitchellh.ghostty")
    );
    // casing varies between terminals and shells
    assert_eq!(
        bundle_id_for_term_program("GHOSTTY"),
        Some("com.mitchellh.ghostty")
    );
    assert_eq!(
        bundle_id_for_term_program("iTerm.app"),
        Some("com.googlecode.iterm2")
    );
    assert_eq!(
        bundle_id_for_term_program("Apple_Terminal"),
        Some("com.apple.Terminal")
    );
    // unknown terminal → no guess, so the click never lands on the wrong app
    assert_eq!(bundle_id_for_term_program("some-unknown-term"), None);
    assert_eq!(bundle_id_for_term_program(""), None);
}

#[test]
fn notifier_args_starting_with_dash_are_not_read_as_flags() {
    use super::super::sanitize_notifier_arg;
    // a leading `-` would be parsed as the next option key, losing the text
    assert_eq!(sanitize_notifier_arg("-title stolen"), " -title stolen");
    assert_eq!(sanitize_notifier_arg("--message"), " --message");
    // ordinary text is passed through untouched
    assert_eq!(sanitize_notifier_arg("rustsmith"), "rustsmith");
    assert_eq!(sanitize_notifier_arg("done — 3 files"), "done — 3 files");
    assert_eq!(sanitize_notifier_arg(""), "");
}
