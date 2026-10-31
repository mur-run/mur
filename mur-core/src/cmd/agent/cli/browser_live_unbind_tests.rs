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
    assert_eq!(e.args, live_argv("bob", None));
    assert_eq!(hosts_of(&f.servers), vec!["a.example.com"]);
    assert!(text.contains("profile unbound"), "{text}");
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
    assert_eq!(e.args, live_argv("bob", Some("site")));
}

#[test]
fn profile_and_no_profile_together_are_refused() {
    let err = split_profile(&s(&["a.example.com", "--profile", "x", "--no-profile"])).unwrap_err();
    assert!(err.contains("contradict"), "{err}");
}
