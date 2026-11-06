//! §5 manifest gate, end to end through the append and repair paths
//! (at-mention A3).
use super::*;
use crate::ChannelService;
use crate::service::restart::RestartSigner;
use mur_common::channel::{CHANNEL_SCHEMA_VERSION, ChannelState, Goal};
use mur_common::identity::AgentIdentity;
use tempfile::TempDir;

fn sample_channel(id: &str) -> Channel {
    let now = Utc::now();
    Channel {
        v: CHANNEL_SCHEMA_VERSION,
        id: id.to_string(),
        title: "t".into(),
        goal: Goal::default(),
        state: ChannelState::Working,
        purpose: None,
        owner: ChannelActor::Human { name: "me".into() },
        participants: vec![],
        created_at: now,
        updated_at: now,
    }
}

fn say(store: &ChannelStore, id: &str, text: &str) -> Result<ChannelEvent> {
    store.append_event(
        id,
        ChannelActor::Human { name: "me".into() },
        EventKind::Message,
        serde_json::json!({ "text": text }),
        None,
        None,
        None,
    )
}

/// A channel with one event whose manifest was then deleted.
fn channel_without_manifest(tmp: &TempDir, id: &str) -> ChannelStore {
    let store = ChannelStore::new(tmp.path());
    store.create(&sample_channel(id)).unwrap();
    say(&store, id, "first").unwrap();
    std::fs::remove_file(store.manifest_path(id).unwrap()).unwrap();
    store
}

fn repair(tmp: &TempDir, id: &str) -> Result<()> {
    let identity = AgentIdentity::generate();
    ChannelService::open(tmp.path())?
        .restart_chain(
            id,
            Utc::now(),
            ChannelActor::System,
            RestartSigner {
                identity: &identity,
                kv: 1,
                anchor_home: None,
            },
            || Ok(true),
            |_| Ok(serde_json::json!({ "type": crate::repair_record::REPAIR_EVENT_TYPE })),
        )
        .map(|_| ())
}

fn manifest_err(err: &anyhow::Error) -> Option<&ManifestError> {
    err.downcast_ref::<ManifestError>()
}

/// An ordinary append on a channel that has events but lost its manifest
/// must not stamp a placeholder with THIS build's `v` (a v2 build would
/// silently downgrade a v3 channel). It errors and writes nothing.
#[test]
fn appending_to_a_channel_with_no_manifest_is_refused() {
    let tmp = TempDir::new().unwrap();
    let store = channel_without_manifest(&tmp, "fleet-x");
    let err = say(&store, "fleet-x", "second").unwrap_err();
    assert_eq!(manifest_err(&err), Some(&ManifestError::Missing), "{err}");
    assert!(
        !store.manifest_path("fleet-x").unwrap().exists(),
        "no placeholder written"
    );
    assert_eq!(
        store.load_events("fleet-x").unwrap().len(),
        1,
        "no event written"
    );
}

/// The repair is the one path allowed to restore a placeholder.
#[test]
fn restart_chain_restores_a_missing_manifest() {
    let tmp = TempDir::new().unwrap();
    let store = channel_without_manifest(&tmp, "fleet-x");
    repair(&tmp, "fleet-x").unwrap();
    let ch = store.load_manifest("fleet-x").unwrap();
    assert_eq!(ch.v, CHANNEL_SCHEMA_VERSION);
    let events = store.load_events("fleet-x").unwrap();
    assert_eq!(events.len(), 1, "the fresh chain holds the repair record");
    assert!(crate::repair_record::is_repair_record(&events[0]));
}

#[test]
fn unparseable_manifest_is_refused_on_append_and_on_repair() {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    store.create(&sample_channel("c1")).unwrap();
    say(&store, "c1", "before").unwrap();
    std::fs::write(store.manifest_path("c1").unwrap(), ": : :").unwrap();

    let err = say(&store, "c1", "x").unwrap_err();
    assert!(
        matches!(manifest_err(&err), Some(ManifestError::Unparseable(_))),
        "{err}"
    );
    let err = repair(&tmp, "c1").unwrap_err();
    assert!(
        matches!(manifest_err(&err), Some(ManifestError::Unparseable(_))),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(store.manifest_path("c1").unwrap()).unwrap(),
        ": : :",
        "manifest unmodified"
    );
    let events = store.load_events("c1").unwrap();
    assert_eq!(events.len(), 1, "log untouched by both refusals");
    assert_eq!(events[0].payload["text"], "before");
}

#[test]
fn manifest_above_build_version_is_too_new_on_load_and_append() {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    let mut ch = sample_channel("c1");
    store.create(&ch).unwrap();
    ch.v = CHANNEL_SCHEMA_VERSION + 1;
    store.save_manifest(&ch).unwrap();
    let before = std::fs::read_to_string(store.manifest_path("c1").unwrap()).unwrap();

    let err = store.load_manifest("c1").unwrap_err();
    assert!(
        matches!(manifest_err(&err), Some(ManifestError::TooNew { .. })),
        "{err}"
    );
    let err = store
        .append_event(
            "c1",
            ChannelActor::System,
            EventKind::Note,
            serde_json::json!({"text": "x"}),
            None,
            None,
            None,
        )
        .unwrap_err();
    assert!(
        matches!(manifest_err(&err), Some(ManifestError::TooNew { .. })),
        "{err}"
    );
    assert_eq!(
        std::fs::read_to_string(store.manifest_path("c1").unwrap()).unwrap(),
        before
    );
    assert!(store.load_events("c1").unwrap().is_empty());
}

/// A brand-new directory (no events file yet) is the create path: the first
/// append still gets its placeholder under the ordinary policy.
#[test]
fn first_append_into_a_fresh_directory_still_writes_a_placeholder() {
    let tmp = TempDir::new().unwrap();
    let store = ChannelStore::new(tmp.path());
    say(&store, "fresh", "hi").unwrap();
    assert_eq!(store.load_manifest("fresh").unwrap().id, "fresh");
}
