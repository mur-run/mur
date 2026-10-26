//! Issue #2156: a user `fs_deny` under the agent's own home must still deny
//! writes. The own-home re-allow is emitted after the `fs_deny` block and SBPL
//! is last-match-wins, so the deny has to be re-asserted after the re-allow.

use super::*;
use std::path::PathBuf;

const OWN_HOME: &str = "/data/.mur/agents/mur";

fn policy_with_own_home_deny(deny: &[&str]) -> SandboxPolicy {
    let home = PathBuf::from(OWN_HOME);
    let mur_home = PathBuf::from("/data/.mur");
    SandboxPolicy {
        fs_deny: deny.iter().map(PathBuf::from).collect(),
        launch_chain: crate::sandbox::launch_chain::LaunchChain::for_test(
            &home,
            &mur_home.join("bin"),
            &mur_home.join("home"),
        ),
        ..Default::default()
    }
}

fn own_reallow_index(sbpl: &str) -> usize {
    sbpl.rfind(&format!(r#"(allow file-write* (subpath "{OWN_HOME}"))"#))
        .expect("own-home re-allow missing")
}

#[test]
fn fs_deny_under_own_home_is_write_denied_after_the_reallow() {
    let denied = format!("{OWN_HOME}/notes");
    let sbpl = build_sbpl_profile(&policy_with_own_home_deny(&[&denied]));

    let last_write_deny = sbpl
        .rfind(&format!(r#"(deny file-write* (subpath "{denied}"))"#))
        .expect("write deny missing");
    // Ordering IS the mechanism: a deny that only exists before the re-allow
    // is overridden and the agent can write the path.
    assert!(
        last_write_deny > own_reallow_index(&sbpl),
        "fs_deny under own home must be re-asserted after the own-home re-allow:\n{sbpl}"
    );
}

#[test]
fn fs_deny_under_own_home_keeps_its_read_deny() {
    let denied = format!("{OWN_HOME}/notes");
    let sbpl = build_sbpl_profile(&policy_with_own_home_deny(&[&denied]));
    assert!(
        sbpl.contains(&format!(r#"(deny file-read* (subpath "{denied}"))"#)),
        "read deny must still be emitted:\n{sbpl}"
    );
}

#[test]
fn fs_deny_outside_own_home_is_not_re_emitted() {
    let outside = "/data/elsewhere";
    let sbpl = build_sbpl_profile(&policy_with_own_home_deny(&[outside]));
    let line = format!(r#"(deny file-write* (subpath "{outside}"))"#);
    assert_eq!(
        sbpl.matches(&line).count(),
        1,
        "a deny outside own home is not overridden, so it needs no re-assertion:\n{sbpl}"
    );
}

#[test]
fn fs_deny_on_an_ancestor_of_own_home_does_not_seal_the_home() {
    // A deny on the agents root is overridden by the re-allow by design: the
    // runtime cannot function without writing its own home. Re-asserting an
    // ancestor would seal the whole home, so it must not follow the re-allow.
    let ancestor = "/data/.mur/agents";
    let sbpl = build_sbpl_profile(&policy_with_own_home_deny(&[ancestor]));
    let last = sbpl
        .rfind(&format!(r#"(deny file-write* (subpath "{ancestor}"))"#))
        .expect("ancestor deny missing");
    assert!(
        last < own_reallow_index(&sbpl),
        "an ancestor deny must stay before the own-home re-allow:\n{sbpl}"
    );
}
