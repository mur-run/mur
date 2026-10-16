//! #1900: the gate's pause/resume transitions go through the router writer,
//! so on a disk-keyed home they are signed and the sealed head stays on the
//! last log line instead of falling behind it.

use super::*;
use tempfile::TempDir;

fn auto_yes() -> GatePolicy {
    GatePolicy {
        yes: true,
        unanswered: Unanswered::Wait,
        auto_approve_tiers: vec![],
    }
}

fn destructive() -> ActionRequest {
    ActionRequest {
        tier: RiskTier::Destructive,
        tool_name: "bash".into(),
        tool_input: serde_json::json!({ "cmd": "rm -rf ./x" }),
        step_or_call_id: "s0".into(),
        agent_id: "mur".into(),
        summary: "rm".into(),
    }
}

#[tokio::test]
async fn gate_transitions_are_signed_and_keep_the_head_on_the_log() {
    let tmp = TempDir::new().unwrap();
    let router = crate::channel_writer::plant_writer_identity(tmp.path());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let ch = svc.create_for_workflow("g").unwrap();
    drop(svc);

    // `yes` runs the gate to completion: pause, auto-answer, resume.
    gate(
        tmp.path(),
        &ch.id,
        &destructive(),
        &auto_yes(),
        None,
        Some("run-sealed"),
    )
    .await
    .unwrap();

    let svc = ChannelService::open(tmp.path()).unwrap();
    let events = svc.load_events(&ch.id).unwrap();
    let pubkey = router.verifying_key_bytes();
    let transitions: Vec<_> = events
        .iter()
        .filter(|e| e.kind == EventKind::StateChange)
        .filter(|e| e.payload.get("run_id").and_then(|v| v.as_str()) == Some("run-sealed"))
        .collect();
    assert_eq!(transitions.len(), 2, "pause + resume: {transitions:?}");
    for e in &transitions {
        assert!(e.sig.is_some(), "gate transition left unsigned: {e:?}");
        assert!(
            mur_channel::sign::verify_one(&ch.id, e, &pubkey, true),
            "gate transition does not verify against the router key: {e:?}"
        );
    }

    let log = std::fs::read(
        tmp.path()
            .join("channels")
            .join(&ch.id)
            .join("events.jsonl"),
    )
    .unwrap();
    let head = mur_channel::head_io::read_head(&mur_channel::anchor::heads_dir(tmp.path()), &ch.id)
        .unwrap()
        .expect("disk-keyed gate seals the head");
    assert_eq!(
        head.lines,
        mur_channel::head::chain_log(&log).lines,
        "head fell behind the log after the gate's transitions"
    );
    assert!(head.verify(&pubkey));
    assert!(!mur_channel::anchor::is_dirty(tmp.path(), &ch.id));
}
