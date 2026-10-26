//! #2142: an ABSENT writer key under `MUR_CHANNEL_REQUIRE_SIG` must refuse
//! the write, the same way an unreadable one does.
//!
//! Before the fix, `NotFound` returned `Ok(None)` unconditionally, so the
//! event was appended unsigned and then silently dropped by the verifying
//! reader — the write looked successful and the event simply vanished.

use super::*;
use tempfile::TempDir;

fn sidecar(home: &Path) -> std::path::PathBuf {
    home.join("channels").join("write-refusals.jsonl")
}

#[test]
fn a_missing_writer_key_is_refused_when_signatures_are_required() {
    // Records a refusal, so it bumps the process-wide counter: hold the
    // same lock as the exact-count tests or it lands inside their window.
    let _guard = super::tests::COUNTER
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    let mark = refusal_count();

    let err = match writer_key_with(home, ROUTER_AGENT, true) {
        Err(e) => e,
        Ok(k) => panic!(
            "require_sig with no key must refuse, got Ok(is_some={})",
            k.is_some()
        ),
    };
    let msg = format!("{err:#}");
    assert!(
        msg.contains("does not exist") && msg.contains("MUR_CHANNEL_REQUIRE_SIG"),
        "the error must name the cause and the setting: {msg}"
    );

    // The Err is discarded by several call sites, so the refusal must also
    // leave a trace that outlives this call.
    let body = std::fs::read_to_string(sidecar(home)).expect("refusal sidecar must exist");
    let rec: serde_json::Value = serde_json::from_str(body.trim()).expect("one JSON line");
    assert_eq!(rec["router_agent"], ROUTER_AGENT);
    assert!(
        rec["reason"].as_str().unwrap().contains("does not exist"),
        "{rec}"
    );
    assert_eq!(refusals_since(mark), 1, "the refusal must be counted");
}

#[test]
fn a_missing_writer_key_still_writes_unsigned_when_signatures_are_optional() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();

    let key = writer_key_with(home, ROUTER_AGENT, false).expect("bootstrap path must not fail");
    assert!(key.is_none(), "no key and no enforcement means unsigned");
    assert!(
        !sidecar(home).exists(),
        "the optional path is not a refusal and must not record one"
    );
}

#[test]
fn a_present_writer_key_signs_when_signatures_are_required() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path();
    plant_writer_identity(home);

    let key = writer_key_with(home, ROUTER_AGENT, true)
        .expect("a present key must not be refused")
        .expect("a present key must be returned");
    assert_eq!(key.source, KeySource::Disk);
    assert!(!sidecar(home).exists(), "nothing was refused");
}
