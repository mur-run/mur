//! `/browser live --no-profile`: the one-command unbind.

use super::*;

// Unbind: `--no-profile` drops the saved session from an existing entry.
#[test]
fn no_profile_drops_the_session_and_keeps_the_hosts() {
    let mut f = Fake::default();
    run(
        "bob",
        &s(&["--yes", "a.example.com", "--profile", "site"]),
        true,
        &mut f,
    )
    .unwrap();
    let (_, chip) = run("bob", &s(&["a.example.com", "--no-profile"]), true, &mut f).unwrap();
    assert_eq!(
        chip.expect("chip").kind,
        mur_common::proposal::ProposalKind::Slash(
            "browser live --yes a.example.com --no-profile".into()
        )
    );
    let (text, _) = run(
        "bob",
        &s(&["--yes", "a.example.com", "--no-profile"]),
        true,
        &mut f,
    )
    .unwrap();
    let e = f.servers.iter().find(|e| e.name == LIVE_ENTRY).unwrap();
    assert_eq!(e.args, live_argv("bob", &[]));
    assert_eq!(hosts_of(&f.servers), vec!["a.example.com"]);
    assert!(text.contains("profile unbound"), "{text}");
}

// #2263: `--profile` repeats; the entry carries every named profile, and
// naming a subset is how one is unbound while the other stays.
#[test]
fn repeated_profile_flags_bind_several_and_a_subset_unbinds_one() {
    let mut f = Fake::default();
    let (text, _) = run(
        "bob",
        &s(&[
            "--yes",
            "a.example.com",
            "--profile",
            "one",
            "--profile",
            "two",
        ]),
        true,
        &mut f,
    )
    .unwrap();
    let e = f.servers.iter().find(|e| e.name == LIVE_ENTRY).unwrap();
    assert_eq!(e.args, live_argv("bob", &s(&["one", "two"])));
    assert!(text.contains("`one`, `two` (merged"), "{text}");

    let (_, chip) = run(
        "bob",
        &s(&["a.example.com", "--profile", "two"]),
        true,
        &mut f,
    )
    .unwrap();
    assert_eq!(
        chip.expect("chip").kind,
        mur_common::proposal::ProposalKind::Slash(
            "browser live --yes a.example.com --profile two".into()
        )
    );
    run(
        "bob",
        &s(&["--yes", "a.example.com", "--profile", "two"]),
        true,
        &mut f,
    )
    .unwrap();
    let e = f.servers.iter().find(|e| e.name == LIVE_ENTRY).unwrap();
    assert_eq!(e.args, live_argv("bob", &s(&["two"])));
}

#[test]
fn a_repeated_profile_name_is_bound_once() {
    let (_, p) = split_profile(&s(&["a.example.com", "--profile", "x", "--profile", "x"])).unwrap();
    assert_eq!(p, ProfileArg::Set(s(&["x"])));
}

// A rerun with no profile flag keeps the bound session (unchanged, no chip).
#[test]
fn rerun_without_a_profile_flag_keeps_the_binding() {
    let mut f = Fake::default();
    run(
        "bob",
        &s(&["--yes", "a.example.com", "--profile", "site"]),
        true,
        &mut f,
    )
    .unwrap();
    let (_, chip) = run("bob", &s(&["a.example.com"]), true, &mut f).unwrap();
    assert!(chip.is_none());
    let e = f.servers.iter().find(|e| e.name == LIVE_ENTRY).unwrap();
    assert_eq!(e.args, live_argv("bob", &s(&["site"])));
}

// #2545: a flagless rerun that changes the hosts keeps the binding, so its
// note must name the profile it kept, not claim the browser has no session.
#[test]
fn a_flagless_rerun_names_the_profile_it_kept() {
    let mut f = Fake::default();
    run(
        "bob",
        &s(&["--yes", "a.example.com", "--profile", "site"]),
        true,
        &mut f,
    )
    .unwrap();
    let (text, _) = run("bob", &s(&["--yes", "b.example.com"]), true, &mut f).unwrap();
    let e = f.servers.iter().find(|e| e.name == LIVE_ENTRY).unwrap();
    assert_eq!(e.args, live_argv("bob", &s(&["site"])));
    assert_eq!(hosts_of(&f.servers), vec!["a.example.com", "b.example.com"]);
    assert!(
        text.contains("still signed in as the saved profile `site`"),
        "{text}"
    );
    assert!(!text.contains("no profile"), "{text}");
    assert!(!text.contains("carries no session"), "{text}");
}

// #2545: the sessionless hint stays for the cases it is true in — a new
// entry, and an existing one that has no profile bound.
#[test]
fn a_flagless_run_with_nothing_bound_still_says_no_profile() {
    let mut f = Fake::default();
    let (text, _) = run("bob", &s(&["--yes", "a.example.com"]), true, &mut f).unwrap();
    assert!(
        text.contains("no profile: the browser carries no session"),
        "{text}"
    );
    let (text, _) = run("bob", &s(&["--yes", "b.example.com"]), true, &mut f).unwrap();
    assert!(
        text.contains("no profile: the browser carries no session"),
        "{text}"
    );
    assert!(!text.contains("still signed in"), "{text}");
}

// #2545: the note reads the binding back out of the entry's argv, so the
// reader must invert `live_argv` and agree with the runtime's own test
// (`wants_state_key`) on the `--profile=<site>` spelling.
#[test]
fn bound_profiles_reads_back_what_live_argv_wrote() {
    assert!(bound_profiles(&live_argv("bob", &[])).is_empty());
    assert_eq!(
        bound_profiles(&live_argv("bob", &s(&["one", "two"]))),
        s(&["one", "two"])
    );
    let mut hand_edited = live_argv("bob", &[]);
    hand_edited.push("--profile=site".to_owned());
    assert_eq!(bound_profiles(&hand_edited), s(&["site"]));
    assert!(mur_common::browser_state_key::wants_state_key(&hand_edited));
}

// #2545: several kept profiles are named the way `--profile a --profile b`
// names them when it binds them.
#[test]
fn a_flagless_rerun_names_every_profile_it_kept() {
    let mut f = Fake::default();
    run(
        "bob",
        &s(&[
            "--yes",
            "a.example.com",
            "--profile",
            "one",
            "--profile",
            "two",
        ]),
        true,
        &mut f,
    )
    .unwrap();
    let (text, _) = run("bob", &s(&["--yes", "b.example.com"]), true, &mut f).unwrap();
    assert!(
        text.contains(
            "still signed in as the saved profiles `one`, `two` (merged into one session); \
             rerun with `--no-profile` to unbind"
        ),
        "{text}"
    );
}

#[test]
fn profile_and_no_profile_together_are_refused() {
    let err = split_profile(&s(&["a.example.com", "--profile", "x", "--no-profile"])).unwrap_err();
    assert!(err.contains("contradict"), "{err}");
}

/// The runtime decides which MCP child gets the profile key from the argv
/// alone (`mur_common::browser_state_key`); pin it to the argv actually
/// written here, not a hand-copied one (#2369).
#[test]
fn the_runtime_recognises_the_written_live_argv() {
    use mur_common::browser_state_key::{is_live_browser, wants_state_key};
    let bare = live_argv("bob", &[]);
    assert!(
        is_live_browser(&bare),
        "pre-seal read must cover a bare entry"
    );
    assert!(!wants_state_key(&bare));
    let bound = live_argv("bob", &s(&["one", "two"]));
    assert!(is_live_browser(&bound));
    assert!(wants_state_key(&bound));
}
