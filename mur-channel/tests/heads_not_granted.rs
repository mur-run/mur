//! The head directory must stay outside every write grant MUR builds for an
//! agent (channel-log-integrity spec 7.4, #1770). The head is only worth
//! anything if an agent that can write `channels/` still cannot write it.
//!
//! Static guard over the shared grant lists. It does not see the sandbox
//! policy built at runtime, nor an operator-granted `filesystem.write` of
//! `<mur_home>` (the known hole in 7.4, decision E2).

use mur_channel::seal::HEADS_DIR_NAME;
use mur_common::agent::AUTHORING_DIRS;
use mur_common::paths::RUN_STATE_DIRS;

#[test]
fn heads_are_not_run_state() {
    assert!(
        !RUN_STATE_DIRS.contains(&HEADS_DIR_NAME),
        "a fleet run could rewrite the heads that vouch for its own channel"
    );
}

#[test]
fn heads_are_not_an_authoring_grant() {
    assert!(
        !AUTHORING_DIRS.contains(&HEADS_DIR_NAME),
        "the concierge's authoring grant must not reach the heads"
    );
}

#[test]
fn heads_dir_is_not_nested_under_a_granted_dir() {
    // A head directory placed under `channels/` would inherit that grant.
    assert!(!HEADS_DIR_NAME.contains('/'));
    assert_ne!(HEADS_DIR_NAME, "channels");
}
