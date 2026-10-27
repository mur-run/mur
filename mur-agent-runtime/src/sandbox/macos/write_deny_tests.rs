//! The post-allow write-denies (spec 2026-10-25 §Callers, R5-1, R6-1, R7-1).
//!
//! SBPL is last-match-wins, so position is the rule. These tests assert the
//! order of the generated lines, not just their presence.

use super::*;
use crate::sandbox::launch_chain::{CAPTURE_STORES, LaunchChain};
use std::path::PathBuf;

const MUR: &str = "/data/.mur";
const OWN_HOME: &str = "/data/.mur/agents/alice";

fn chain() -> LaunchChain {
    let mur = PathBuf::from(MUR);
    LaunchChain::for_test(
        &PathBuf::from(OWN_HOME),
        &mur.join("bin"),
        &mur.join("home"),
    )
}

fn policy(write: &[PathBuf], deny: &[PathBuf], fleet: bool) -> SandboxPolicy {
    SandboxPolicy {
        fs_write: write.to_vec(),
        fs_deny: deny.to_vec(),
        launch_chain: chain(),
        fleet_conversations_grant: fleet,
        ..Default::default()
    }
}

fn rule(verb: &str, p: &Path) -> String {
    format!("({verb} (subpath \"{}\"))", p.display())
}

fn lines(sbpl: &str) -> Vec<&str> {
    sbpl.lines().collect()
}

fn pos(l: &[&str], needle: &str) -> Option<usize> {
    l.iter().position(|x| *x == needle)
}

fn last_pos(l: &[&str], needle: &str) -> Option<usize> {
    l.iter().rposition(|x| *x == needle)
}

/// Index of the first credential-loop read-deny: everything from here on is
/// "after every allow".
fn credential_loop_start(l: &[&str]) -> usize {
    let first = chain().credential_paths()[0].clone();
    last_pos(l, &rule("deny file-read*", &first)).expect("credential loop missing")
}

fn write_denies_from(l: &[&str], from: usize) -> Vec<PathBuf> {
    l[from..]
        .iter()
        .filter_map(|x| x.strip_prefix("(deny file-write* (subpath \""))
        .filter_map(|x| x.strip_suffix("\"))"))
        .map(PathBuf::from)
        .collect()
}

#[test]
fn the_post_allow_write_denies_are_exactly_the_extra_rows_plus_the_capture_stores() {
    let c = chain();
    let mur = PathBuf::from(MUR);
    for fleet in [false, true] {
        let sbpl = build_sbpl_profile(&policy(&[], &[], fleet));
        let l = lines(&sbpl);
        let got = write_denies_from(&l, credential_loop_start(&l));

        let deny = c.deny_paths();
        let mut expected: Vec<PathBuf> = c
            .write_protected_paths()
            .into_iter()
            .filter(|p| !deny.contains(p))
            .collect();
        expected.extend(
            CAPTURE_STORES
                .iter()
                .filter(|s| !(fleet && **s == "conversations"))
                .map(|s| mur.join(s)),
        );
        assert_eq!(got, expected, "fleet={fleet}\n{sbpl}");
        // R5-1, release-build guarantee: none can reach the own home.
        for p in &got {
            assert!(
                !c.agent_self_home().starts_with(p),
                "{} covers the own home",
                p.display()
            );
        }
    }
}

/// R5-1 both ways: the launch-chain denies (which contain `agents/`) stay
/// before the own-home allow, and that allow is the only write allow after
/// the first `fs_deny` rule. Keyed on `deny_paths()`, not "anything under
/// `agents/`": the self-protected re-denies and the #2159 re-asserts are
/// deliberately after the allow.
#[test]
fn deny_paths_precede_the_own_home_allow_which_is_the_last_write_allow() {
    let c = chain();
    let user_deny = PathBuf::from(OWN_HOME).join("private");
    let sbpl = build_sbpl_profile(&policy(
        &[PathBuf::from(MUR)],
        std::slice::from_ref(&user_deny),
        false,
    ));
    let l = lines(&sbpl);
    let own = pos(&l, &rule("allow file-write*", c.agent_self_home())).unwrap();
    for d in c.deny_paths() {
        let at = pos(&l, &rule("deny file-write*", &d)).unwrap();
        assert!(at < own, "{} comes after the own-home allow", d.display());
    }
    let first_fs_deny = pos(&l, &rule("deny file-read*", &user_deny)).unwrap();
    let allows_after: Vec<&&str> = l[first_fs_deny..]
        .iter()
        .filter(|x| x.starts_with("(allow file-write*"))
        .collect();
    assert_eq!(allows_after, vec![&l[own]], "{sbpl}");
}

/// R6-1/R7-1: a parent write grant cannot reopen a capture store, and nothing
/// after the credential loop re-allows one.
#[test]
fn a_parent_grant_cannot_reopen_the_capture_stores() {
    let mur = PathBuf::from(MUR);
    for fleet in [false, true] {
        let sbpl = build_sbpl_profile(&policy(std::slice::from_ref(&mur), &[], fleet));
        let l = lines(&sbpl);
        let grant = pos(&l, &rule("allow file-write*", &mur)).unwrap();
        for s in CAPTURE_STORES {
            let p = mur.join(s);
            let write_deny = last_pos(&l, &rule("deny file-write*", &p));
            if fleet && s == "conversations" {
                assert!(write_deny.is_none(), "fleet conversations still denied");
            } else {
                assert!(write_deny.unwrap() > grant, "{s} fleet={fleet}");
            }
            assert!(pos(&l, &rule("deny file-read*", &p)).is_some(), "{s} read");
        }
        let start = credential_loop_start(&l);
        assert!(
            !l[start..]
                .iter()
                .any(|x| x.starts_with("(allow file-write*")),
            "an allow follows the credential loop:\n{sbpl}"
        );
    }
}

/// R7-1 regression a: with the fleet grant, a user `fs_deny` under
/// `conversations/` is the last write rule that matches it.
#[test]
fn a_user_deny_under_fleet_conversations_wins() {
    let conv = PathBuf::from(MUR).join("conversations");
    let private = conv.join("private");
    let sbpl = build_sbpl_profile(&policy(
        std::slice::from_ref(&conv),
        std::slice::from_ref(&private),
        true,
    ));
    let l = lines(&sbpl);
    let deny_w = last_pos(&l, &rule("deny file-write*", &private)).unwrap();
    assert!(pos(&l, &rule("deny file-read*", &private)).is_some());
    let covering_allow = l
        .iter()
        .rposition(|x| {
            x.strip_prefix("(allow file-write* (subpath \"")
                .and_then(|x| x.strip_suffix("\"))"))
                .is_some_and(|p| private.starts_with(p))
        })
        .unwrap();
    assert!(deny_w > covering_allow, "{sbpl}");
    assert!(last_pos(&l, &rule("deny file-write*", &conv)).is_none());
}

/// R7-1 regression b: without the flag the store keeps its write-deny, the
/// user deny is still there, and no allow for `conversations/` follows.
#[test]
fn without_the_flag_conversations_keeps_its_write_deny() {
    let conv = PathBuf::from(MUR).join("conversations");
    let private = conv.join("private");
    let sbpl = build_sbpl_profile(&policy(
        std::slice::from_ref(&conv),
        std::slice::from_ref(&private),
        false,
    ));
    let l = lines(&sbpl);
    let start = credential_loop_start(&l);
    assert!(last_pos(&l, &rule("deny file-write*", &conv)).unwrap() > start);
    assert!(pos(&l, &rule("deny file-write*", &private)).is_some());
    let allow = rule("allow file-write*", &conv);
    let grant = pos(&l, &allow).unwrap();
    assert_eq!(last_pos(&l, &allow), Some(grant), "a second allow follows");
}

/// R5-3, N2, R5-5 on the kernel side.
#[test]
fn key_files_get_the_right_denies() {
    let sbpl = build_sbpl_profile(&policy(&[], &[], false));
    let c = PathBuf::from(MUR).join("commander");
    for p in [c.join("identity.pub"), c.join("identity.prev.pub")] {
        assert!(
            sbpl.contains(&rule("deny file-write*", &p)),
            "{}",
            p.display()
        );
        assert!(
            !sbpl.contains(&rule("deny file-read*", &p)),
            "{}",
            p.display()
        );
    }
    for p in [
        c.join("identity.key"),
        c.join("signing.key"),
        PathBuf::from(MUR).join("publisher").join("identity.key"),
    ] {
        assert!(
            sbpl.contains(&rule("deny file-write*", &p)),
            "{}",
            p.display()
        );
        assert!(
            sbpl.contains(&rule("deny file-read*", &p)),
            "{}",
            p.display()
        );
    }
}
