use super::*;

fn all() -> OverrideFlags {
    OverrideFlags {
        no_hitl: true,
        net_unrestricted: true,
        full_perms: true,
    }
}

fn run(is_tty: bool, typed: &str, flag_confirm: bool) -> (Confirmation, String) {
    let mut input = typed.as_bytes();
    let mut out = Vec::new();
    let c = confirm(
        "alpha",
        all(),
        flag_confirm,
        Terminal {
            is_tty,
            input: &mut input,
            output: &mut out,
        },
    );
    (c, String::from_utf8(out).unwrap())
}

#[test]
fn banner_says_what_the_spec_requires() {
    let b = banner("alpha", all());
    for phrase in [
        "NEXT boot only",
        "deferred",
        "classified Spend or Privileged",
        "no\n    declared tier",
        "`ask` rules",
        "remembered denials still apply",
        "egress proxy",
        "secrets/",
        "fails closed",
    ] {
        assert!(b.contains(phrase), "banner lacks {phrase:?}:\n{b}");
    }
    // Never the over-promise the spec forbids.
    assert!(!b.contains("privileged actions always ask"));
}

#[test]
fn banner_lists_only_the_flags_asked_for() {
    let b = banner(
        "alpha",
        OverrideFlags {
            no_hitl: true,
            ..OverrideFlags::default()
        },
    );
    assert!(b.contains("--no-hitl"));
    assert!(!b.contains("--net=unrestricted"));
    assert!(!b.contains("--full-perms"));
}

#[test]
fn tty_typed_name_confirms() {
    let (c, out) = run(true, "alpha\n", false);
    assert_eq!(c, Confirmation::Confirmed(ConfirmedBy::Tty));
    assert!(out.contains("Type the agent name (alpha)"));
}

#[test]
fn tty_wrong_name_or_eof_declines() {
    assert_eq!(run(true, "alpah\n", false).0, Confirmation::Declined);
    assert_eq!(run(true, "", false).0, Confirmation::Declined);
}

#[test]
fn tty_still_asks_with_confirm_flag() {
    let (c, _) = run(true, "nope\n", true);
    assert_eq!(c, Confirmation::Declined);
}

#[test]
fn non_tty_needs_the_confirm_flag() {
    let (c, out) = run(false, "alpha\n", false);
    assert_eq!(c, Confirmation::NeedsConfirmFlag);
    assert!(out.is_empty(), "no prompt without a TTY");
    assert_eq!(
        run(false, "", true).0,
        Confirmation::Confirmed(ConfirmedBy::FlagConfirm)
    );
}
