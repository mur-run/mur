use chrono::Utc;
use mur_common::multimodal::{ProvenanceEntry, ProvenanceLedger};
use tempfile::TempDir;

#[test]
fn ledger_append_then_read_back() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("inputs.jsonl");

    let l = ProvenanceLedger::new(&path);
    l.append(&ProvenanceEntry {
        sha256: "a".repeat(64),
        source: "user_drop".into(),
        decoder_version: "test".into(),
        ocr_engine_version: None,
        turn_id: 1,
        recorded_at: Utc::now(),
    })
    .unwrap();
    l.append(&ProvenanceEntry {
        sha256: "b".repeat(64),
        source: "user_paste".into(),
        decoder_version: "test".into(),
        ocr_engine_version: None,
        turn_id: 1,
        recorded_at: Utc::now(),
    })
    .unwrap();

    let entries = ProvenanceLedger::new(&path).read_turn(1).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].sha256, "a".repeat(64));
    assert_eq!(entries[1].sha256, "b".repeat(64));

    // Wrong turn returns empty.
    assert!(
        ProvenanceLedger::new(&path)
            .read_turn(2)
            .unwrap()
            .is_empty()
    );
}

#[test]
fn ledger_atomic_append_handles_corrupt_lines() {
    // A truncated line (e.g., from a crash mid-write) must not poison
    // subsequent reads — read_turn skips malformed lines.
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("inputs.jsonl");
    std::fs::write(&path, "{ partial json\n").unwrap();
    let l = ProvenanceLedger::new(&path);
    l.append(&ProvenanceEntry {
        sha256: "c".repeat(64),
        source: "user_drop".into(),
        decoder_version: "test".into(),
        ocr_engine_version: None,
        turn_id: 5,
        recorded_at: Utc::now(),
    })
    .unwrap();
    let entries = l.read_turn(5).unwrap();
    assert_eq!(entries.len(), 1, "corrupt line skipped, valid line read");
}

#[test]
fn ledger_missing_file_returns_empty() {
    let tmp = TempDir::new().unwrap();
    let path = tmp.path().join("nonexistent.jsonl");
    let l = ProvenanceLedger::new(&path);
    let entries = l.read_turn(1).unwrap();
    assert!(entries.is_empty());
}
