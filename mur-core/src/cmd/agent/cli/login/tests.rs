use super::*;

#[test]
fn provider_aliases() {
    for s in ["anthropic", "Anthropic", "claude", "CLAUDE"] {
        assert_eq!(Provider::parse(s), Some(Provider::Anthropic), "{s}");
    }
    for s in ["chatgpt", "codex", "openai", "OpenAI"] {
        assert_eq!(Provider::parse(s), Some(Provider::Chatgpt), "{s}");
    }
    assert_eq!(Provider::parse("bogus"), None);
}

#[test]
fn labels_are_user_facing_brand_spellings() {
    assert_eq!(Provider::Anthropic.label(), "Anthropic");
    assert_eq!(Provider::Chatgpt.label(), "ChatGPT");
}

#[test]
fn keychain_read_never_asks_for_the_secret() {
    // `-w` is what makes `security` print the password itself. murmur reads
    // metadata only; this test is the guard on that promise.
    let args = keychain_stamp_args();
    assert!(
        !args.contains(&"-w"),
        "must not request the secret: {args:?}"
    );
    assert!(args.contains(&"Claude Code-credentials"));
}

#[test]
fn stamp_tracks_the_store_not_the_clock() {
    // Deliberately NOT `StoreStamp("a") == StoreStamp("a")` — that would
    // exercise `#[derive(PartialEq)]` and would still pass if `store_stamp`
    // returned a constant. Drive the real function instead: the same
    // untouched store must stamp identically, and a changed one must not.
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("auth.json");
    std::fs::write(&f, "{}").unwrap();
    let first = file_stamp(&f).expect("stamp");
    assert_eq!(file_stamp(&f).expect("stamp"), first, "unchanged store");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&f, "{\"a\":1}").unwrap();
    assert_ne!(file_stamp(&f).expect("stamp"), first, "changed store");
}

#[test]
fn missing_file_has_no_stamp() {
    assert_eq!(
        file_stamp(std::path::Path::new("/nonexistent/auth.json")),
        None
    );
}

#[test]
fn claude_status_logged_in() {
    let json =
        r#"{"loggedIn":true,"authMethod":"claude.ai","email":"a@b.c","subscriptionType":"max"}"#;
    let s = parse_claude_status(json);
    assert!(s.logged_in);
    assert_eq!(s.identity.as_deref(), Some("a@b.c (max)"));
    assert!(s.cli_present);
}

#[test]
fn claude_status_logged_out() {
    let s = parse_claude_status(r#"{"loggedIn":false}"#);
    assert!(!s.logged_in);
    assert_eq!(s.identity, None);
}

#[test]
fn claude_status_without_subscription_still_shows_the_email() {
    let s = parse_claude_status(r#"{"loggedIn":true,"email":"a@b.c"}"#);
    assert_eq!(s.identity.as_deref(), Some("a@b.c"));
}

#[test]
fn claude_status_logged_out_ignores_identity_fields_when_present() {
    // `{"loggedIn":false}` alone can't tell whether the empty identity
    // came from the early return or from email/sub simply being absent.
    // Carry them anyway: a stale-but-present email must not leak into
    // the identity line once loggedIn says false.
    let s = parse_claude_status(r#"{"loggedIn":false,"email":"a@b.c","subscriptionType":"max"}"#);
    assert!(!s.logged_in);
    assert_eq!(s.identity, None);
}

#[test]
fn malformed_claude_status_is_not_a_panic() {
    // A CLI upgrade could change the shape. Degrade to "unknown", never crash.
    let s = parse_claude_status("not json at all");
    assert!(!s.logged_in);
    assert_eq!(s.identity, None);
}

#[test]
fn codex_status_variants() {
    assert!(parse_codex_status("Logged in using ChatGPT").logged_in);
    assert!(!parse_codex_status("Not logged in").logged_in);
    assert!(!parse_codex_status("").logged_in);
}

#[test]
fn codex_identity_is_the_status_line_only_when_logged_in() {
    let in_ = parse_codex_status("Logged in using ChatGPT");
    assert_eq!(in_.identity.as_deref(), Some("Logged in using ChatGPT"));
    let out = parse_codex_status("Not logged in");
    assert_eq!(out.identity, None);
}

#[test]
fn codex_status_is_case_insensitive() {
    // Pins the union design: a capitalisation change upstream (the
    // brief's own concern) must not silently start reporting "not
    // logged in". Fails under the old case-sensitive `starts_with`.
    let s = parse_codex_status("logged in using ChatGPT");
    assert!(s.logged_in);
    assert_eq!(s.identity.as_deref(), Some("logged in using ChatGPT"));
}

#[test]
fn run_capture_kills_a_wedged_process_and_degrades_like_empty_output() {
    // A real subprocess, not a mock: proves the kill-on-timeout path
    // itself, not just that a `Duration` value got threaded through.
    let (bin, args): (&str, &[&str]) = if cfg!(windows) {
        (
            "powershell",
            &["-NoProfile", "-Command", "Start-Sleep -Seconds 5"],
        )
    } else {
        ("/bin/sleep", &["5"])
    };
    let start = std::time::Instant::now();
    let out = run_capture(bin, args, std::time::Duration::from_millis(200));
    // The value alone doesn't prove the timeout fired: a `sleep 5` that
    // ran to completion would *also* eventually return `Some("")`, since
    // sleep prints nothing. The elapsed bound is what actually
    // distinguishes "killed at ~200ms" from "waited out the full sleep".
    assert!(
        start.elapsed() < std::time::Duration::from_secs(3),
        "did not time out promptly: {:?}",
        start.elapsed()
    );
    assert_eq!(out, Some(String::new()));
}

#[test]
fn store_stamp_reads_each_providers_own_file_not_the_others() {
    // Regression pin for the acceptance criterion on `store_stamp`: this
    // must fail if the function's two match arms are swapped (Anthropic
    // wired to the codex file, Chatgpt to the claude file) — that swap
    // previously passed every test in this file. `keychain: None` drives
    // the Anthropic arm through its file fallback only; see
    // `store_stamp_in`'s doc comment for why the real keychain can't be
    // used here.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();

    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "claude").unwrap();
    let expected_claude = file_stamp(&home.join(".claude/.credentials.json")).expect("stamp");

    // A real, measurable mtime gap — otherwise two writes issued back to
    // back could land on the same filesystem-reported instant and the
    // two "expected" stamps below could coincide, letting the test pass
    // even after a swap.
    std::thread::sleep(std::time::Duration::from_millis(20));

    std::fs::create_dir_all(home.join(".codex")).unwrap();
    std::fs::write(home.join(".codex/auth.json"), "codex").unwrap();
    let expected_codex = file_stamp(&home.join(".codex/auth.json")).expect("stamp");

    assert_ne!(
        expected_claude, expected_codex,
        "test setup must produce distinguishable stamps"
    );

    assert_eq!(
        store_stamp_in(Provider::Anthropic, home, None),
        Some(expected_claude),
        "Anthropic must read the claude store, not the codex one"
    );
    assert_eq!(
        store_stamp_in(Provider::Chatgpt, home, None),
        Some(expected_codex),
        "Chatgpt must read the codex store, not the claude one"
    );
}

#[test]
fn store_stamp_prefers_the_keychain_probe_over_the_file_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    // A claude file exists too, so a precedence *reversal* (file-then-
    // keychain instead of keychain-then-file) would surface as a
    // mismatch here, not just as a param-ignored bug.
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();

    let from_keychain = StoreStamp("keychain-mdat-line".into());
    assert_eq!(
        store_stamp_in(Provider::Anthropic, home, Some(from_keychain.clone())),
        Some(from_keychain),
        "a keychain hit must win over the file fallback, even when a file exists too"
    );
}

#[test]
fn status_line_shows_identity_when_logged_in() {
    let s = OwnerStatus {
        logged_in: true,
        identity: Some("a@b.c (max)".into()),
        cli_present: true,
    };
    let line = render_status_line(Provider::Anthropic, &s, true);
    assert!(line.contains("Anthropic"), "{line}");
    assert!(line.contains("a@b.c (max)"), "{line}");
}

#[test]
fn status_line_names_the_repair_when_logged_out() {
    let s = OwnerStatus {
        logged_in: false,
        identity: None,
        cli_present: true,
    };
    let line = render_status_line(Provider::Chatgpt, &s, false);
    assert!(line.contains("/login chatgpt"), "must name the fix: {line}");
}

#[test]
fn status_line_flags_a_missing_owner_cli() {
    // A transplanted credential can work while being unrepairable here.
    let s = OwnerStatus {
        logged_in: false,
        identity: None,
        cli_present: false,
    };
    let line = render_status_line(Provider::Anthropic, &s, true);
    assert!(line.contains("not installed"), "{line}");
    assert!(
        !line.contains("/login anthropic"),
        "cannot repair without the CLI: {line}"
    );
}

fn st(logged_in: bool) -> OwnerStatus {
    OwnerStatus {
        logged_in,
        identity: None,
        cli_present: true,
    }
}

#[test]
fn a_moved_stamp_means_the_probe_repaired_it() {
    let before = Some(StoreStamp("a".into()));
    let after = Some(StoreStamp("b".into()));
    assert_eq!(
        classify_repair(before, after, &st(true)),
        Rung::RefreshedByProbe
    );
}

#[test]
fn an_unmoved_stamp_with_a_healthy_status_was_already_fine() {
    let s = Some(StoreStamp("a".into()));
    assert_eq!(
        classify_repair(s.clone(), s, &st(true)),
        Rung::AlreadyHealthy
    );
}

#[test]
fn an_unmoved_stamp_with_an_unhealthy_status_needs_a_real_login() {
    let s = Some(StoreStamp("a".into()));
    assert_eq!(classify_repair(s.clone(), s, &st(false)), Rung::NeedsLogin);
}

#[test]
fn a_vanished_store_is_not_reported_as_a_refresh() {
    // `before != after` alone is also true when the store disappeared
    // between the two stamps (Some -> None) — but vanishing is not
    // evidence of a repair. Only the `after.is_some()` conjunct in
    // `classify_repair`'s guard rules that out; deleting it would report
    // RefreshedByProbe here instead of falling through to the CLI status.
    let before = Some(StoreStamp("a".into()));
    assert_eq!(
        classify_repair(before.clone(), None, &st(true)),
        Rung::AlreadyHealthy
    );
    assert_eq!(classify_repair(before, None, &st(false)), Rung::NeedsLogin);
}

#[test]
fn no_stamp_degrades_to_the_cli_report() {
    // A Linux box whose credential lives in a keychain yields no stamp at
    // all. The repair must fall through to what the owner CLI says rather
    // than inventing a verdict — and in particular must NOT report
    // RefreshedByProbe on the strength of two `None`s comparing equal.
    assert_eq!(classify_repair(None, None, &st(true)), Rung::AlreadyHealthy);
    assert_eq!(classify_repair(None, None, &st(false)), Rung::NeedsLogin);
}

#[test]
fn cheap_repair_stamps_after_the_probe_not_before() {
    // The stamp only changes once the probe has run — exactly what a real
    // refresh looks like. An implementation that takes both stamps up
    // front sees "a" twice, reports the store unmoved, and falls through
    // to the CLI's report instead of RefreshedByProbe.
    use std::cell::Cell;
    let probed = Cell::new(false);
    let stamp = |_p: Provider| {
        Some(StoreStamp(
            if probed.get() { "after" } else { "before" }.to_string(),
        ))
    };
    let status = |_p: Provider| {
        probed.set(true);
        st(true)
    };
    assert_eq!(
        cheap_repair_in(Provider::Anthropic, stamp, status),
        Rung::RefreshedByProbe,
        "the second stamp must be taken after the probe, not before"
    );
}

#[test]
fn a_missing_owner_cli_cannot_be_repaired() {
    let absent = OwnerStatus {
        logged_in: false,
        identity: None,
        cli_present: false,
    };
    assert_eq!(classify_repair(None, None, &absent), Rung::NoOwnerCli);
}
