//! H1: non-human anchored writes on an owned channel.

use std::path::{Path, PathBuf};

use mur_common::channel::{ChannelActor, EventKind};
use mur_common::identity::AgentIdentity;

use super::super::test_home::*;
use crate::anchor::{heads_dir, is_dirty};
use crate::head::Head;
use crate::head_io::read_head;

struct Agent {
    id: AgentIdentity,
    home: PathBuf,
}

fn agent(h: &Home) -> Agent {
    let home = h.path().join("agents").join("qa");
    let id = AgentIdentity::from_secret_bytes(&[7u8; 32]);
    id.save(&home).unwrap();
    Agent { id, home }
}

fn head(h: &Home, id: &str) -> Option<Head> {
    read_head(&heads_dir(h.path()), id).unwrap()
}

fn reply(
    h: &Home,
    id: &str,
    a: &Agent,
    router: Option<&Path>,
) -> anyhow::Result<mur_common::channel::ChannelEvent> {
    h.svc().append_and_anchor_routed(
        id,
        &a.id,
        0,
        &a.home,
        router.map(|rh| h.seal(rh)),
        ChannelActor::Agent { id: "qa".into() },
        EventKind::Message,
        serde_json::json!({ "text": "reply" }),
        None,
    )
}

#[test]
fn unowned_channel_writer_seals_its_own_head() {
    let h = Home::new();
    let a = agent(&h);
    let id = h.legacy_channel(1);
    let rh = h.router_home();
    let ev = reply(&h, &id, &a, Some(&rh)).unwrap();
    let hd = head(&h, &id).expect("anchored");
    assert_eq!(hd.seq, ev.seq);
    assert!(
        hd.verify(&a.id.verifying_key_bytes()),
        "writer sealed, as today"
    );
    assert!(!hd.verify(&h.router.verifying_key_bytes()));
}

#[test]
fn owned_channel_agent_write_is_router_sealed() {
    let h = Home::new();
    let a = agent(&h);
    let id = h.legacy_channel(1);
    say(&h, &id, &open_gate(), "hi").unwrap();
    let rh = h.router_home();
    let ev = reply(&h, &id, &a, Some(&rh)).unwrap();
    let hd = head(&h, &id).unwrap();
    assert_eq!(hd.seq, ev.seq, "the head covers the agent line");
    assert!(hd.verify(&h.router.verifying_key_bytes()));
    assert!(!is_dirty(h.path(), &id));
    assert!(h.trust(&id).damage().is_empty());
}

#[test]
fn owned_channel_without_router_key_appends_unanchored() {
    let h = Home::new();
    let a = agent(&h);
    let id = h.legacy_channel(1);
    say(&h, &id, &open_gate(), "hi").unwrap();
    let before = head(&h, &id).unwrap();
    let ev = reply(&h, &id, &a, None).expect("written, not refused");
    let after = head(&h, &id).unwrap();
    assert_eq!(
        after, before,
        "the head is never sealed under the writer key"
    );
    assert!(ev.seq > after.seq, "the line sits past the head");
    assert!(
        !is_dirty(h.path(), &id),
        "unanchored by design leaves no marker"
    );
    assert!(h.trust(&id).damage().is_empty());
    // The next human write's router seal covers it.
    let next = say(&h, &id, &open_gate(), "again").unwrap();
    assert_eq!(head(&h, &id).unwrap().seq, next.seq);
}

#[test]
fn plain_append_and_anchor_on_owned_channel_does_not_fail() {
    let h = Home::new();
    let a = agent(&h);
    let id = h.legacy_channel(0);
    say(&h, &id, &open_gate(), "hi").unwrap();
    let before = head(&h, &id).unwrap();
    h.svc()
        .append_and_anchor(
            &id,
            &a.id,
            0,
            &a.home,
            ChannelActor::Agent { id: "qa".into() },
            EventKind::Note,
            serde_json::json!({ "text": "card" }),
            None,
        )
        .expect("an agent line on an owned channel is not an anchor error");
    assert_eq!(head(&h, &id).unwrap(), before);
}

/// A C1 crash leaves the record past the head; the next router-sealed
/// agent write covers it, as the next human write would.
#[test]
fn router_seal_heals_crash_lagged_record() {
    let h = Home::new();
    let a = agent(&h);
    let id = h.legacy_channel(1);
    say(&h, &id, &open_gate(), "hi").unwrap();
    std::fs::remove_file(crate::head_io::head_path(&heads_dir(h.path()), &id).unwrap()).unwrap();
    assert!(
        !h.trust(&id).damage().is_empty(),
        "fixture: owner_unanchored"
    );
    let rh = h.router_home();
    let ev = reply(&h, &id, &a, Some(&rh)).unwrap();
    let hd = head(&h, &id).expect("resealed");
    assert_eq!(hd.seq, ev.seq);
    assert!(hd.verify(&h.router.verifying_key_bytes()));
    assert!(h.trust(&id).damage().is_empty());
}

/// A commander directive: signed by a key that writes nothing else, so the
/// router seals its head on an unowned channel too, and the router's own
/// writes keep working after it.
fn directive(h: &Home, id: &str, commander: &Agent) -> mur_common::channel::ChannelEvent {
    let rh = h.router_home();
    h.svc()
        .append_and_anchor_v1(
            id,
            &commander.id,
            0,
            &commander.home,
            Some(h.seal(&rh)),
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({ "text": "directive" }),
            None,
        )
        .unwrap()
}

fn router_write(h: &Home, id: &str) -> anyhow::Result<mur_common::channel::ChannelEvent> {
    h.svc().append_and_anchor(
        id,
        &h.router,
        0,
        &h.router_home(),
        ChannelActor::System,
        EventKind::Note,
        serde_json::json!({ "text": "router" }),
        None,
    )
}

#[test]
fn directive_on_unowned_channel_is_router_sealed_either_order() {
    let h = Home::new();
    let c = agent(&h);
    for router_first in [true, false] {
        let id = h.legacy_channel(0);
        if router_first {
            router_write(&h, &id).unwrap();
        }
        let ev = directive(&h, &id, &c);
        let hd = head(&h, &id).unwrap();
        assert_eq!(hd.seq, ev.seq);
        assert!(hd.verify(&h.router.verifying_key_bytes()));
        assert!(!hd.verify(&c.id.verifying_key_bytes()));
        let next = router_write(&h, &id).expect("the router still advances its head");
        assert_eq!(head(&h, &id).unwrap().seq, next.seq);
        assert!(!is_dirty(h.path(), &id));
    }
}

#[test]
fn directive_on_owned_channel_is_router_sealed() {
    let h = Home::new();
    let c = agent(&h);
    let id = h.legacy_channel(1);
    say(&h, &id, &open_gate(), "hi").unwrap();
    let ev = directive(&h, &id, &c);
    let hd = head(&h, &id).unwrap();
    assert_eq!(hd.seq, ev.seq);
    assert!(hd.verify(&h.router.verifying_key_bytes()));
    assert!(h.trust(&id).damage().is_empty());
}

/// An agent's own line on a channel whose head the router owns (a fleet
/// router's synthesis): the agent signs it, the router seals, either order.
fn agent_line(h: &Home, id: &str, a: &Agent) -> mur_common::channel::ChannelEvent {
    let rh = h.router_home();
    h.svc()
        .append_and_anchor_router_sealed(
            id,
            &a.id,
            0,
            &a.home,
            h.seal(&rh),
            ChannelActor::Agent { id: "qa".into() },
            EventKind::Message,
            serde_json::json!({ "text": "report" }),
            None,
        )
        .unwrap()
}

#[test]
fn an_agent_line_is_router_sealed_on_an_unowned_channel_either_order() {
    let h = Home::new();
    let a = agent(&h);
    for router_first in [true, false] {
        let id = h.legacy_channel(0);
        if router_first {
            router_write(&h, &id).unwrap();
        }
        let ev = agent_line(&h, &id, &a);
        assert!(ev.sig.is_some(), "the agent signs its own line");
        let hd = head(&h, &id).unwrap();
        assert_eq!(hd.seq, ev.seq);
        assert!(hd.verify(&h.router.verifying_key_bytes()));
        assert!(!hd.verify(&a.id.verifying_key_bytes()));
        let next = router_write(&h, &id).expect("the router still advances its head");
        assert_eq!(head(&h, &id).unwrap().seq, next.seq);
        assert!(!is_dirty(h.path(), &id));
    }
}
