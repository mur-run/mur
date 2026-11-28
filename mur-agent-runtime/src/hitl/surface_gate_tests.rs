//! Spec §7 merge gate (PR-1, before any reviewer code ships): a signed, fresh
//! allow from a non-human surface must not settle a later identical call.

use std::sync::Arc;

use mur_common::hitl::HitlResponse;
use mur_common::identity::AgentIdentity;

use super::remembered::signed_allows;
use super::store::{ChannelDecisionStore, DecisionStore, Settled};

/// Signed allows in the log for `hash`, surface ignored: proves a row was
/// written, so a `lookup == None` comes from the gate, not a failed record.
fn written_allows(home: &std::path::Path, id: &AgentIdentity, hash: &str) -> usize {
    let svc = mur_channel::ChannelService::open(home).unwrap();
    let marker = home
        .join("agents")
        .join("qa")
        .join(mur_common::agent::HITL_CHANNEL_FILE);
    let channel_id = std::fs::read_to_string(marker).unwrap().trim().to_string();
    let events = svc.load_events(&channel_id).unwrap();
    signed_allows(&channel_id, &events, &id.verifying_key_bytes())
        .iter()
        .filter(|(h, _)| h == hash)
        .count()
}

fn store(home: &std::path::Path) -> ChannelDecisionStore {
    store_with(home, Arc::new(AgentIdentity::generate()))
}

fn store_with(home: &std::path::Path, id: Arc<AgentIdentity>) -> ChannelDecisionStore {
    ChannelDecisionStore::new(home.to_path_buf(), "qa".into(), id, 0)
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
        let id = Arc::new(AgentIdentity::generate());
        let s = store_with(tmp.path(), id.clone());
        s.record(allow("h", "a", surface)).await;
        assert_eq!(
            written_allows(tmp.path(), &id, "h"),
            1,
            "surface {surface:?}: the row must be in the log"
        );
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
    let deny_at = deny.issued_at.unwrap();
    s.record(deny).await;
    // Strictly newer, so "a deny wins a tie" cannot make this pass without
    // the gate.
    let mut later = allow("h", "b", "reviewer");
    later.issued_at = Some(deny_at + chrono::Duration::seconds(1));
    s.record(later).await;
    assert_eq!(s.lookup("h").await, Some(Settled::Deny));
}

/// A deny from a non-human surface is filtered too: the call asks again
/// rather than being refused on a machine's say-so. Pinned as a decision.
#[tokio::test]
async fn a_machine_surface_deny_asks_again() {
    for surface in ["reviewer", "auto", "unknown", "policy"] {
        let tmp = tempfile::tempdir().unwrap();
        let s = store(tmp.path());
        let mut deny = allow("h", "a", surface);
        deny.allow = false;
        s.record(deny).await;
        assert_eq!(s.lookup("h").await, None, "surface {surface:?}");
    }
}
