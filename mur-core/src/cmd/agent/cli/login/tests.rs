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
fn an_upstream_status_line_cannot_reach_the_transcript_unbounded() {
    // `codex login status` has no `--json`, so the identity is a whole line
    // of upstream text murmur does not control — and it lands in the
    // transcript and then in the terminal's own scrollback.
    let long = format!("Logged in using ChatGPT {}", "x".repeat(500));
    let id = parse_codex_status(&long).identity.expect("identity");
    assert!(
        id.chars().count() <= MAX_IDENTITY_CHARS + 1,
        "not capped: {} chars",
        id.chars().count()
    );
    assert!(id.ends_with('…'), "a truncated line must say so: {id:?}");
}

#[test]
fn control_characters_are_stripped_from_an_identity_line() {
    // An escape sequence survives into a ratatui cell and is re-emitted by
    // the terminal verbatim — that is injection, not a formatting quirk.
    let s = parse_codex_status("Logged in using \u{1b}[2JChatGPT\u{7}");
    let id = s.identity.expect("identity");
    assert!(
        !id.contains('\u{1b}') && !id.contains('\u{7}'),
        "control characters survived: {id:?}"
    );
    assert!(
        id.contains("ChatGPT"),
        "the readable part must survive: {id}"
    );

    // Same bound on the Anthropic path: those fields are selected, but they
    // are still upstream strings.
    let json = format!(r#"{{"loggedIn":true,"email":"{}@b.c"}}"#, "a".repeat(400));
    let id = parse_claude_status(&json).identity.expect("identity");
    assert!(id.chars().count() <= MAX_IDENTITY_CHARS + 1, "{id}");
}

#[test]
fn a_short_identity_is_passed_through_untouched() {
    // Negative control for the two tests above: the cap must not be a
    // blanket rewrite. `codex_identity_is_the_status_line_only_when_logged_in`
    // would still pass if `sanitize_identity` returned a fixed label.
    assert_eq!(
        sanitize_identity("Logged in using ChatGPT"),
        "Logged in using ChatGPT"
    );
    assert_eq!(sanitize_identity("  a@b.c (max)  "), "a@b.c (max)");
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
    // must fail if the seam's two match arms are swapped (Anthropic
    // wired to the codex file, Chatgpt to the claude file) — that swap
    // previously passed every test in this file. A keychain thunk that
    // answers `None` drives the Anthropic arm through its file fallback
    // only; see `store_stamp_in`'s doc comment for why the real keychain
    // can't be used here.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();

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

    let at = |p| store_stamp_in(p, || Some(home.clone()), || None);
    assert_eq!(
        at(Provider::Anthropic),
        Some(expected_claude),
        "Anthropic must read the claude store, not the codex one"
    );
    assert_eq!(
        at(Provider::Chatgpt),
        Some(expected_codex),
        "Chatgpt must read the codex store, not the claude one"
    );
}

#[test]
fn store_stamp_prefers_the_keychain_probe_over_the_file_fallback() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();
    // A claude file exists too, so a precedence *reversal* (file-then-
    // keychain instead of keychain-then-file) would surface as a
    // mismatch here, not just as a param-ignored bug.
    std::fs::create_dir_all(home.join(".claude")).unwrap();
    std::fs::write(home.join(".claude/.credentials.json"), "{}").unwrap();

    let from_keychain = StoreStamp("keychain-mdat-line".into());
    let want = from_keychain.clone();
    assert_eq!(
        store_stamp_in(
            Provider::Anthropic,
            || Some(home.clone()),
            move || Some(from_keychain)
        ),
        Some(want),
        "a keychain hit must win over the file fallback, even when a file exists too"
    );
}

#[test]
fn the_seam_resolves_only_what_it_needs() {
    // Laziness is routing, and routing lives in the seam. `store_stamp` used
    // to compose these two rules itself, which is how it ended up passing
    // `keychain: None` unconditionally and leaving the seam's keychain arm
    // dead in production. Both properties below are behavioural, not stylistic:
    // forcing either thunk eagerly reddens this test.
    use std::cell::Cell;
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().to_path_buf();

    // 1. The codex arm must never run the keychain probe — that shells out to
    //    `security` on macOS for a provider whose credential is never there.
    let probed = Cell::new(0usize);
    let _ = store_stamp_in(
        Provider::Chatgpt,
        || Some(home.clone()),
        || {
            probed.set(probed.get() + 1);
            Some(StoreStamp("k".into()))
        },
    );
    assert_eq!(
        probed.get(),
        0,
        "the codex arm must not shell out to the keychain"
    );

    // 2. A keychain hit must not need the home directory resolved. This is the
    //    exact regression the `or_else` exists for: a box where `home_dir()`
    //    fails still gets its stamp, instead of an otherwise-correct "✓ ..."
    //    carrying a spurious "(no credential store found)".
    let homed = Cell::new(0usize);
    let got = store_stamp_in(
        Provider::Anthropic,
        || {
            homed.set(homed.get() + 1);
            None
        },
        || Some(StoreStamp("k".into())),
    );
    assert_eq!(got, Some(StoreStamp("k".into())));
    assert_eq!(
        homed.get(),
        0,
        "a keychain hit must not need the home directory"
    );

    // 3. ...and with neither, the answer is `None`, not a panic.
    assert_eq!(store_stamp_in(Provider::Anthropic, || None, || None), None);
    assert_eq!(store_stamp_in(Provider::Chatgpt, || None, || None), None);
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
    // Names the binary it looked for, so the row is actionable for someone
    // who does use the provider…
    assert!(line.contains("claude"), "{line}");
    // …and offers no repair, because murmur cannot honour one without the CLI.
    assert!(
        !line.contains("/login anthropic"),
        "cannot repair without the CLI: {line}"
    );
    // Bare `/login` always lists both providers, so this row is what an
    // Anthropic-only user sees for ChatGPT every single time. It must not
    // read as a failure report.
    let line = render_status_line(Provider::Chatgpt, &s, false);
    assert!(line.contains("codex"), "{line}");
    for alarm in ["cannot", "✗", "error", "failed"] {
        assert!(
            !line.to_lowercase().contains(alarm),
            "a provider the user does not use must not read like an error: {line}"
        );
    }
}

#[test]
fn headless_instructions_quote_the_paths_the_stamp_actually_reads() {
    // The instructions and `store_stamp` must name the same file: telling a
    // user to copy a credential to a path murmur never stamps is a silent
    // dead end. Same constants, asserted from the outside.
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path();
    let claude = claude_credentials_path(home);
    let codex = codex_auth_path(home);
    let rel = |p: &std::path::Path| {
        p.strip_prefix(home)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/")
    };
    assert!(
        print_only_instructions(Provider::Anthropic).contains(&rel(&claude)),
        "{}",
        print_only_instructions(Provider::Anthropic)
    );
    assert!(
        print_only_instructions(Provider::Chatgpt).contains(&rel(&codex)),
        "{}",
        print_only_instructions(Provider::Chatgpt)
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

#[test]
fn browser_detection_matrix() {
    let m = |macos, display, ssh| {
        has_browser(&BrowserEnv {
            macos,
            display,
            ssh,
        })
    };
    // Local macOS desktop: always has a browser.
    assert!(m(true, false, false));
    // macOS reached over SSH: no usable browser on this end.
    assert!(!m(true, false, true));
    // Linux desktop.
    assert!(m(false, true, false));
    // Linux over SSH with X forwarding still works.
    assert!(m(false, true, true));
    // Headless Linux box.
    assert!(!m(false, false, false));
}

#[test]
fn detect_reads_the_variable_names_it_claims_to() {
    // Pins the NAMES and the mapping, which `browser_detection_matrix`
    // structurally cannot: it is handed a BrowserEnv already built.
    // Acceptance criterion is a mutation — swap DISPLAY and
    // SSH_CONNECTION inside `detect_with` and confirm this goes red.
    let only = |want: &'static str| move |name: &str| name == want;

    let e = BrowserEnv::detect_with(false, only("SSH_CONNECTION"));
    assert!(e.ssh, "SSH_CONNECTION must set ssh");
    assert!(!e.display, "SSH_CONNECTION must not set display");

    let e = BrowserEnv::detect_with(false, only("DISPLAY"));
    assert!(e.display, "DISPLAY must set display");
    assert!(!e.ssh, "DISPLAY must not set ssh");

    // Wayland is the second display variable, not a third field.
    let e = BrowserEnv::detect_with(false, only("WAYLAND_DISPLAY"));
    assert!(e.display, "WAYLAND_DISPLAY must set display");
    assert!(!e.ssh);

    // An unrelated variable must move nothing.
    let e = BrowserEnv::detect_with(false, only("EDITOR"));
    assert!(!e.display);
    assert!(!e.ssh);

    // `macos` comes from the caller, never from the environment.
    assert!(BrowserEnv::detect_with(true, |_| false).macos);
    assert!(!BrowserEnv::detect_with(false, |_| true).macos);
}

#[test]
fn headless_instructions_name_a_command_that_works_without_a_browser() {
    let a = print_only_instructions(Provider::Anthropic);
    assert!(a.contains("claude setup-token"), "{a}");
    let c = print_only_instructions(Provider::Chatgpt);
    // Both lines ship; `||` would stay green with either one deleted.
    assert!(c.contains("--with-api-key"), "{c}");
    assert!(c.contains("--with-access-token"), "{c}");
}

#[test]
fn headless_instructions_mention_transplanting_a_credential() {
    // The other supported path: log in where a browser exists, copy it over.
    let a = print_only_instructions(Provider::Anthropic);
    assert!(a.contains(".credentials.json"), "{a}");
    let c = print_only_instructions(Provider::Chatgpt);
    assert!(c.contains(".codex/auth.json"), "{c}");
}

#[test]
fn a_second_login_lock_is_refused_while_the_first_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let first = acquire_login_lock(dir.path()).expect("first lock");
    assert!(
        matches!(acquire_login_lock(dir.path()), Err(LockDenied::Busy)),
        "a second flow must be refused while the first holds the lock"
    );
    drop(first);
    assert!(
        acquire_login_lock(dir.path()).is_ok(),
        "the lock must be released on drop"
    );
}

#[test]
fn a_lock_that_cannot_be_opened_is_not_reported_as_contention() {
    // A `~/.mur` that is read-only, or on a filesystem with no advisory
    // locking, is not contention — and reporting it as such tells the user
    // to go finish a login nobody started. Provoked here with a "home" that
    // is a regular file, so `open` cannot create `login.lock` under it.
    let dir = tempfile::tempdir().unwrap();
    let not_a_dir = dir.path().join("home");
    std::fs::write(&not_a_dir, "").unwrap();
    match acquire_login_lock(&not_a_dir) {
        Err(LockDenied::Unavailable(_)) => {}
        other => panic!("expected Unavailable, got {other:?}"),
    }
}
