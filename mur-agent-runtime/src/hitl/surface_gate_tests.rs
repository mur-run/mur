//! Spec §7 merge gate (PR-1, before any reviewer code ships): a signed, fresh
//! allow from a non-human surface must not settle a later identical call.

use std::sync::Arc;

use mur_common::hitl::HitlResponse;
use mur_common::identity::AgentIdentity;

use super::store::{ChannelDecisionStore, DecisionStore, Settled};

fn store(home: &std::path::Path) -> ChannelDecisionStore {
    ChannelDecisionStore::new(
        home.to_path_buf(),
        "qa".into(),
        Arc::new(AgentIdentity::generate()),
        0,
    )
}

fn allow(hash: &str, id: &str, surface: &str) -> HitlResponse {
    HitlResponse {
        hitl_id: id.into(),
        action_hash: hash.into(),
        allow: true,
        reason: String::new(),
        surface: surface.into(),
        issued_at: Some(chrono::Utc::now()),
        label: None,
    }
}

#[tokio::test]
async fn a_machine_surface_allow_still_reaches_the_human() {
    for surface in ["reviewer", "shadow", "auto", "unknown", "made-up", ""] {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(allow("h", "a", surface)).await;
        assert_eq!(
            s.lookup("h").await,
            None,
            "surface {surface:?} must not settle"
        );
    }
}

#[tokio::test]
async fn a_human_surface_allow_still_settles() {
    for surface in ["cli", "hub", "ios", "mcp"] {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        s.record(allow("h", "a", surface)).await;
        assert_eq!(s.lookup("h").await, Some(Settled::Allow), "{surface}");
    }
}

/// A machine row arriving after a human deny must not unseat it.
#[tokio::test]
async fn a_machine_allow_does_not_override_a_human_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let s = store(tmp.path());
    let mut deny = allow("h", "a", "cli");
    deny.allow = false;
    s.record(deny).await;
    s.record(allow("h", "b", "reviewer")).await;
    assert_eq!(s.lookup("h").await, Some(Settled::Deny));
}
