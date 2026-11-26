//! `deny file-write-unlink` placement (#2582). SBPL is last-match-wins, so the
//! position is the guarantee: any `allow file-write*` after it would hand the
//! unlink right back.

use super::*;
use crate::sandbox::launch_chain::LaunchChain;
use std::path::PathBuf;

const OWN_HOME: &str = "/data/.mur/agents/alice";

fn policy(write: &[&str], nd: &[&str]) -> SandboxPolicy {
    let mur = PathBuf::from("/data/.mur");
    SandboxPolicy {
        fs_write: write.iter().map(PathBuf::from).collect(),
        fs_no_delete: nd.iter().map(PathBuf::from).collect(),
        launch_chain: LaunchChain::for_test(
            &PathBuf::from(OWN_HOME),
            &mur.join("bin"),
            &mur.join("home"),
        ),
        ..Default::default()
    }
}

#[test]
fn each_entry_is_denied_unlink_after_every_write_allow() {
    // One entry under a user grant, one under the own home (re-allowed late).
    let sbpl = build_sbpl_profile(&policy(
        &["/work"],
        &["/work/reports", "/data/.mur/agents/alice/keep"],
    ));
    let l: Vec<&str> = sbpl.lines().collect();
    let last_allow = l
        .iter()
        .rposition(|x| x.starts_with("(allow file-write*"))
        .expect("profile has write allows");
    for p in ["/work/reports", "/data/.mur/agents/alice/keep"] {
        let rule = format!("(deny file-write-unlink (subpath \"{p}\"))");
        let at = l
            .iter()
            .position(|x| *x == rule)
            .unwrap_or_else(|| panic!("missing {rule}"));
        assert!(
            at > last_allow,
            "{rule} at {at} precedes an allow at {last_allow}"
        );
    }
}

#[test]
fn no_entries_emit_no_unlink_clause() {
    let sbpl = build_sbpl_profile(&policy(&["/work"], &[]));
    assert!(!sbpl.contains("file-write-unlink"));
}
