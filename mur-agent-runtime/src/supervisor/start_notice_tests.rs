use super::*;
use mur_common::agent::HITL_CHANNEL_FILE;
use mur_common::ssh_proxy_status::SshProxyState;

fn audit_only() -> SshProxyStatus {
    SshProxyStatus {
        state: SshProxyState::AuditOnly,
        code: Some("unrestricted_network".into()),
        explanation: "network is unrestricted".into(),
        at_unix_ms: 1_700_000_000_000,
    }
}

#[test]
fn the_notice_reads_like_the_status_line() {
    assert_eq!(
        ssh_proxy_text(&audit_only()),
        "ssh_proxy: audit-only (unrestricted_network) — network is unrestricted"
    );
}

#[test]
fn one_signed_note_lands_in_the_remembered_start_channel() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = AgentIdentity::generate();
    let signer = Signer {
        mur_home: tmp.path(),
        agent: "probe",
        identity: &identity,
        key_version: 1,
    };

    let first = append_ssh_proxy(&signer, &audit_only()).unwrap();
    let later = SshProxyStatus {
        at_unix_ms: 1_700_000_000_001,
        ..audit_only()
    };
    let second = append_ssh_proxy(&signer, &later).unwrap();
    assert_eq!(first, second, "every start writes to the same channel");

    let svc = ChannelService::open(tmp.path()).unwrap();
    let events = svc.load_events(&first).unwrap();
    let notes: Vec<_> = events
        .iter()
        .filter(|e| e.kind == EventKind::Note)
        .collect();
    assert_eq!(notes.len(), 2, "one event per start");

    let ev = notes[0];
    assert_eq!(ev.payload["notice"], SSH_PROXY_NOTICE);
    assert_eq!(ev.payload["code"], "unrestricted_network");
    assert_eq!(ev.payload["state"], "audit_only");
    assert_eq!(ev.payload["text"], ssh_proxy_text(&audit_only()));
    assert!(
        mur_channel::sign::verify_one(&first, ev, &identity.verifying_key_bytes(), true),
        "the notice is signed by the agent"
    );
}

#[test]
fn the_same_start_is_not_posted_twice() {
    let tmp = tempfile::tempdir().unwrap();
    let identity = AgentIdentity::generate();
    let signer = Signer {
        mur_home: tmp.path(),
        agent: "probe",
        identity: &identity,
        key_version: 1,
    };
    let id = append_ssh_proxy(&signer, &audit_only()).unwrap();
    // A retry of the same start carries the same classification time.
    let _ = append_ssh_proxy(&signer, &audit_only());
    let svc = ChannelService::open(tmp.path()).unwrap();
    let notes = svc
        .load_events(&id)
        .unwrap()
        .into_iter()
        .filter(|e| e.kind == EventKind::Note)
        .count();
    assert_eq!(notes, 1);
}

#[test]
fn the_hitl_channel_is_left_alone() {
    // The HITL channel's signed head is what the gate checks for tampering;
    // a start notice must never be the thing that moves it.
    let tmp = tempfile::tempdir().unwrap();
    let identity = AgentIdentity::generate();
    let signer = Signer {
        mur_home: tmp.path(),
        agent: "probe",
        identity: &identity,
        key_version: 1,
    };
    append_ssh_proxy(&signer, &audit_only()).unwrap();
    let home = tmp.path().join("agents").join("probe");
    assert!(home.join(START_CHANNEL_FILE).exists());
    assert!(!home.join(HITL_CHANNEL_FILE).exists());
}

#[test]
fn a_marker_naming_a_gone_channel_is_replaced() {
    let tmp = tempfile::tempdir().unwrap();
    let svc = ChannelService::open(tmp.path()).unwrap();
    let dir = tmp.path().join("agents").join("probe");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(START_CHANNEL_FILE), "no-such-channel").unwrap();
    let id = start_channel(&svc, tmp.path(), "probe").unwrap();
    assert_ne!(id, "no-such-channel");
    assert!(svc.exists(&id));
}
