//! C5: `restart_chain` carries the human owner onto the fresh chain.

use std::path::Path;

use chrono::Utc;
use mur_common::channel::{ChannelActor, EventKind, OwnerKeyEnrolledPayload};

use super::super::test_home::*;
use super::*;
use crate::head_io::read_head;
use crate::repair_record::REPAIR_EVENT_TYPE;
use crate::service::{ChannelOwner, EnrollGate};

fn open_gate() -> EnrollGate<'static> {
    EnrollGate {
        identity_protected: true,
        log_intact: &|| Ok(true),
        legacy_sealer: None,
    }
}

/// An owned conversation (B3) with one human message after the record.
fn owned_channel(h: &Home) -> String {
    let rh = h.router_home();
    let ch = h
        .svc()
        .create_for_agent(
            "qa",
            ChannelOwner::Human {
                signer: &h.human,
                router: h.seal(&rh),
            },
        )
        .unwrap();
    h.svc()
        .append_human(
            &ch.id,
            &h.human,
            h.seal(&rh),
            &open_gate(),
            EventKind::Message,
            serde_json::json!({ "text": "hello" }),
            None,
        )
        .unwrap();
    ch.id
}

/// Rewrite the log's first line so the channel is damaged (chain break).
fn damage(h: &Home, id: &str) {
    let p = h.svc().store().events_path(id).unwrap();
    let log = std::fs::read_to_string(&p).unwrap();
    let mut lines: Vec<String> = log.lines().map(str::to_owned).collect();
    let last = lines.len() - 1;
    lines[last] = lines[last].replace("hello", "HELLO");
    std::fs::write(&p, lines.join("\n") + "\n").unwrap();
}

fn signer<'a>(h: &'a Home, rh: &'a Path, owner: bool) -> RestartSigner<'a> {
    RestartSigner {
        identity: &h.router,
        kv: 0,
        anchor_home: Some(rh),
        owner: owner.then(|| RestartOwner {
            signer: &h.human,
            router: h.seal(rh),
            identity_protected: true,
        }),
    }
}

fn record() -> Result<serde_json::Value> {
    Ok(serde_json::json!({ "type": REPAIR_EVENT_TYPE, "text": "repaired" }))
}

/// What `mur channel repair`'s `prepare` does: drop the old head, then hand
/// back the record.
fn prepare(h: &Home, id: &str) -> Result<serde_json::Value> {
    crate::head_io::remove_head(&crate::anchor::heads_dir(h.path()), id)?;
    crate::anchor::clear_dirty(h.path(), id)?;
    record()
}

fn repair(h: &Home, id: &str, owner: bool) -> Result<Option<Restarted>> {
    let rh = h.router_home();
    h.svc().restart_chain(
        id,
        Utc::now(),
        ChannelActor::System,
        signer(h, &rh, owner),
        || Ok(true),
        |_| prepare(h, id),
    )
}

fn kinds(h: &Home, id: &str) -> Vec<EventKind> {
    h.svc()
        .load_events(id)
        .unwrap()
        .iter()
        .map(|e| e.kind)
        .collect()
}

#[test]
fn repair_preserves_ownership() {
    let h = Home::new();
    let id = owned_channel(&h);
    damage(&h, &id);
    let out = repair(&h, &id, true).unwrap().expect("repaired");

    assert_eq!(out.event.seq, 0);
    let rec = out.owner_record.expect("owner carried over");
    assert_eq!(rec.seq, 1);
    assert_eq!(
        kinds(&h, &id),
        [EventKind::Note, EventKind::OwnerKeyEnrolled]
    );
    let p: OwnerKeyEnrolledPayload = serde_json::from_value(rec.payload).unwrap();
    assert!(
        p.human_cut.is_none(),
        "nothing before the restart is vouched for"
    );
    // One router-sealed head over both lines; the record is live and clean.
    let head = read_head(&crate::anchor::heads_dir(h.path()), &id)
        .unwrap()
        .unwrap();
    assert_eq!(head.seq, 1);
    assert!(head.verify(&h.router.verifying_key_bytes()));
    let t = h.trust(&id);
    assert_eq!(t.live_record().map(|r| r.seq), Some(1));
    assert!(t.damage().is_empty(), "{:?}", t.damage());
    assert!(!crate::repair_marker::is_pending(h.path(), &id));

    // The next human write does not enroll a second time.
    h.svc()
        .append_human(
            &id,
            &h.human,
            h.seal(&h.router_home()),
            &open_gate(),
            EventKind::Message,
            serde_json::json!({ "text": "after" }),
            None,
        )
        .unwrap();
    assert_eq!(
        kinds(&h, &id),
        [
            EventKind::Note,
            EventKind::OwnerKeyEnrolled,
            EventKind::Message
        ]
    );
}

/// Crash between the repair record and the owner record: the marker is
/// still down (the channel stays refused, never reads as a fresh unowned
/// chain), and a rerun redoes both lines.
#[test]
fn repair_writes_record_inside_lock() {
    let h = Home::new();
    let id = owned_channel(&h);
    damage(&h, &id);
    let rh = h.router_home();
    // Fail the owner record's head seal: the repair record is down, the
    // owner record's line is written but never sealed.
    let crashing = h
        .svc()
        .with_seal(std::sync::Arc::new(|i: &crate::store::SealInput<'_>| {
            if i.event.kind == EventKind::OwnerKeyEnrolled {
                anyhow::bail!("crash")
            }
            Ok(())
        }));
    let err = crashing
        .restart_chain(
            &id,
            Utc::now(),
            ChannelActor::System,
            signer(&h, &rh, true),
            || Ok(true),
            |_| prepare(&h, &id),
        )
        .unwrap_err();
    assert!(format!("{err:#}").contains("crash"), "{err:#}");
    assert!(crate::repair_marker::is_pending(h.path(), &id));
    assert!(crate::repair_marker::is_owned(h.path(), &id));

    // The rerun finds the marker, restarts from the top, carries the owner.
    let out = repair(&h, &id, true).unwrap().expect("rerun completes");
    assert!(out.owner_record.is_some());
    assert_eq!(
        kinds(&h, &id),
        [EventKind::Note, EventKind::OwnerKeyEnrolled]
    );
    assert!(!crate::repair_marker::is_pending(h.path(), &id));
    assert!(h.trust(&id).damage().is_empty());
    // The audit trail still names the ORIGINAL damaged log first.
    assert!(out.quarantined.is_some());
}

/// A rerun of an owned repair that was not handed the owner is refused,
/// touching nothing more: the marker's answer survives the moved log.
#[test]
fn resumed_owned_repair_without_owner_is_refused() {
    let h = Home::new();
    let id = owned_channel(&h);
    damage(&h, &id);
    let rh = h.router_home();
    let _ = h.svc().restart_chain(
        &id,
        Utc::now(),
        ChannelActor::System,
        signer(&h, &rh, true),
        || Ok(true),
        |_| anyhow::bail!("stop after the move"),
    );
    assert!(crate::repair_marker::is_owned(h.path(), &id));
    let err = repair(&h, &id, false).unwrap_err();
    assert!(format!("{err:#}").contains("human owner"), "{err:#}");
    assert!(crate::repair_marker::is_pending(h.path(), &id));
}

/// An owned channel repaired without the owner (e.g. from inside a sandbox)
/// is refused before the log moves.
#[test]
fn owned_repair_without_owner_touches_nothing() {
    let h = Home::new();
    let id = owned_channel(&h);
    damage(&h, &id);
    let before = h.log(&id);
    let err = repair(&h, &id, false).unwrap_err();
    assert!(format!("{err:#}").contains("human owner"), "{err:#}");
    assert_eq!(h.log(&id), before);
    assert!(!crate::repair_marker::is_pending(h.path(), &id));
}

#[test]
fn unprotected_identity_dir_refuses_before_the_move() {
    let h = Home::new();
    let id = owned_channel(&h);
    damage(&h, &id);
    let before = h.log(&id);
    let rh = h.router_home();
    let mut s = signer(&h, &rh, true);
    s.owner.as_mut().unwrap().identity_protected = false;
    let err = h
        .svc()
        .restart_chain(
            &id,
            Utc::now(),
            ChannelActor::System,
            s,
            || Ok(true),
            |_| record(),
        )
        .unwrap_err();
    assert!(format!("{err:#}").contains("identity/"), "{err:#}");
    assert_eq!(h.log(&id), before);
    assert!(!crate::repair_marker::is_pending(h.path(), &id));
}

/// The fresh chain of an owned channel is signed and sealed by the router.
#[test]
fn non_router_repair_signer_is_refused() {
    let h = Home::new();
    let id = owned_channel(&h);
    damage(&h, &id);
    let other = AgentIdentity::generate();
    let rh = h.router_home();
    let mut s = signer(&h, &rh, true);
    s.identity = &other;
    let err = h
        .svc()
        .restart_chain(
            &id,
            Utc::now(),
            ChannelActor::System,
            s,
            || Ok(true),
            |_| record(),
        )
        .unwrap_err();
    assert!(format!("{err:#}").contains("router"), "{err:#}");
}

/// A legacy channel restarts exactly as before, even when handed an owner:
/// enrollment is C1's job on the first human write.
#[test]
fn repair_on_legacy_channel_unchanged() {
    let h = Home::new();
    let id = h.legacy_channel(2);
    let out = repair(&h, &id, true).unwrap().expect("repaired");
    assert!(out.owner_record.is_none());
    assert_eq!(kinds(&h, &id), [EventKind::Note]);
    assert!(h.trust(&id).live_record().is_none());
    assert!(!crate::repair_marker::is_owned(h.path(), &id));
}

/// `restart_chain` never goes through `append_human`, so it never triggers
/// C1: a legacy restart writes no owner record and leaves the manifest's
/// owner state to the next human write.
#[test]
fn restart_does_not_trigger_c1() {
    let h = Home::new();
    let id = h.legacy_channel(1);
    let identity = AgentIdentity::generate();
    h.svc()
        .restart_chain(
            &id,
            Utc::now(),
            ChannelActor::System,
            RestartSigner {
                identity: &identity,
                kv: 1,
                anchor_home: None,
                owner: None,
            },
            || Ok(true),
            |_| record(),
        )
        .unwrap()
        .expect("repaired");
    assert!(
        !kinds(&h, &id).contains(&EventKind::OwnerKeyEnrolled),
        "a restart never enrolls"
    );
}
