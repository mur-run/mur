//! The gate's integrity read of a fleet channel that carries a commander
//! directive: a directive is not damage, so the gate must not refuse on it.

use super::*;
use crate::channel_writer::{
    CONTROL_PLANE_AGENT, append_with_key, plant_writer_identity, writer_key,
};
use tempfile::TempDir;

#[test]
fn a_commander_directive_does_not_read_as_log_damage() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    plant_writer_identity(home);
    mur_common::identity::AgentIdentity::generate()
        .save(&home.join(mur_common::commander::COMMANDER_DIR))
        .unwrap();
    let svc = ChannelService::open(home).unwrap();
    svc.create_for_fleet(
        "dev",
        CONTROL_PLANE_AGENT,
        &["pm".into()],
        mur_channel::service::ChannelOwner::Legacy,
    )
    .unwrap();
    let key = writer_key(home, CONTROL_PLANE_AGENT).unwrap().unwrap();
    append_with_key(
        &svc,
        home,
        "fleet-dev",
        key,
        &home.join("agents").join(CONTROL_PLANE_AGENT),
        ChannelActor::System,
        EventKind::Note,
        serde_json::json!({ "text": "run started" }),
        None,
    )
    .unwrap();
    drop(svc);

    // Asserted on the log, not on the command's result, so this test says
    // one thing: what the gate reads afterwards.
    let _ = crate::cmd::commander::cmd_commander_directive(home, "dev", "kill", None, 1000);

    let checked = log_integrity::load_checked(home, "fleet-dev").unwrap();
    assert_eq!(checked.damage, None);
    assert_eq!(checked.events.len(), 2);
}
