use super::*;
use crate::channel_writer::{CONTROL_PLANE_AGENT, plant_identity_for, plant_writer_identity};
use mur_channel::anchor::heads_dir;
use mur_channel::head_io::read_head;
use mur_channel::verify::human::HumanTrust;
use tempfile::TempDir;

fn router_key(home: &Path) -> [u8; 32] {
    AgentIdentity::load(&home.join("agents").join(CONTROL_PLANE_AGENT))
        .unwrap()
        .verifying_key_bytes()
}

fn trust(home: &Path, id: &str) -> HumanTrust {
    let log = std::fs::read_to_string(
        mur_channel::ChannelStore::new(home)
            .events_path(id)
            .unwrap(),
    )
    .unwrap();
    let head = read_head(&heads_dir(home), id).unwrap();
    HumanTrust::from_log(home, id, &log, head.as_ref())
}

/// A legacy session channel the way a pre-phase-2 `murmur` leaves it: the
/// agent's reply anchored under the agent's own key.
fn legacy_session(home: &Path) -> String {
    plant_writer_identity(home);
    plant_identity_for(home, "qa");
    let svc = ChannelService::open(home).unwrap();
    let ch = svc.create_for_agent("qa", ChannelOwner::Legacy).unwrap();
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        &ch.id,
        crate::channel_writer::Authorship::agent("qa"),
        EventKind::Message,
        serde_json::json!({ "text": "ready" }),
        None,
    )
    .unwrap();
    ch.id
}

#[test]
fn first_human_write_enrolls_signs_and_router_seals() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let id = legacy_session(home);
    let svc = ChannelService::open(home).unwrap();
    let ev = append_as_human(
        &svc,
        home,
        &id,
        EventKind::Message,
        serde_json::json!({ "text": "hi" }),
        None,
    )
    .unwrap();

    let pk = mur_common::identity::human::load_pubkey(home).unwrap();
    let ChannelActor::Human { key: Some(k), .. } = &ev.actor else {
        panic!("keyed human actor: {:?}", ev.actor)
    };
    assert_eq!(mur_common::identity::decode_pubkey(k).unwrap(), pk);
    assert!(mur_channel::sign::verify_one(&id, &ev, &pk, true));

    let t = trust(home, &id);
    assert!(t.live_record().is_some(), "enrolled on first write");
    assert!(t.damage().is_empty(), "{:?}", t.damage());
    let head = read_head(&heads_dir(home), &id).unwrap().expect("anchored");
    assert_eq!(head.seq, ev.seq);
    assert!(head.verify(&router_key(home)), "router seals the head");
    assert!(channel_integrity::check(home, &id).unwrap().is_intact());
}

#[test]
fn second_write_does_not_enroll_again() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let id = legacy_session(home);
    let svc = ChannelService::open(home).unwrap();
    for t in ["one", "two"] {
        append_as_human(
            &svc,
            home,
            &id,
            EventKind::Message,
            serde_json::json!({ "text": t }),
            None,
        )
        .unwrap();
    }
    let records = svc
        .load_events(&id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::OwnerKeyEnrolled)
        .count();
    assert_eq!(records, 1);
}

/// A pre-phase-2 session's typed lines were signed by the session agent as
/// a keyless `Human`; they fail under the router and are not damage (D-7).
#[test]
fn writer_signed_human_history_does_not_block_enrollment() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let id = legacy_session(home);
    let svc = ChannelService::open(home).unwrap();
    crate::channel_writer::append_as_writer(
        &svc,
        home,
        &id,
        crate::channel_writer::Authorship::out_of_role(
            crate::channel_writer::Signer::Agent("qa"),
            ChannelActor::local_human(),
        ),
        EventKind::Message,
        serde_json::json!({ "text": "old turn" }),
        None,
    )
    .unwrap();
    append_as_human(
        &svc,
        home,
        &id,
        EventKind::Message,
        serde_json::json!({ "text": "new turn" }),
        None,
    )
    .unwrap();
    assert!(trust(home, &id).live_record().is_some());
}

#[test]
fn damaged_log_is_refused_and_nothing_written() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let id = legacy_session(home);
    let path = mur_channel::ChannelStore::new(home)
        .events_path(&id)
        .unwrap();
    let mut log = std::fs::read_to_string(&path).unwrap();
    log.push_str("not json\n");
    std::fs::write(&path, &log).unwrap();
    let svc = ChannelService::open(home).unwrap();
    let err = append_as_human(
        &svc,
        home,
        &id,
        EventKind::Message,
        serde_json::json!({ "text": "hi" }),
        None,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("damaged log"), "{err:#}");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), log);
}

/// D-4: no unsigned fallback. A human key that cannot be loaded is an
/// error, and nothing is appended.
#[test]
fn human_write_without_identity_errors() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let id = legacy_session(home);
    crate::identity_enroll::enroll(home).unwrap();
    std::fs::remove_file(mur_common::identity::human::human_private_key_path(home)).unwrap();
    let svc = ChannelService::open(home).unwrap();
    let before = svc.load_events(&id).unwrap().len();
    let err = append_as_human(
        &svc,
        home,
        &id,
        EventKind::Message,
        serde_json::json!({ "text": "hi" }),
        None,
    )
    .unwrap_err();
    assert!(format!("{err:#}").contains("private key"), "{err:#}");
    assert_eq!(svc.load_events(&id).unwrap().len(), before);
}

#[test]
fn human_actor_names_the_enrolled_key() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    assert_eq!(human_actor(home), ChannelActor::local_human());
    let pk = crate::identity_enroll::ensure_enrolled(home).unwrap();
    let ChannelActor::Human { key: Some(k), .. } = human_actor(home) else {
        panic!("keyed")
    };
    assert_eq!(mur_common::identity::decode_pubkey(&k).unwrap(), pk);
}
