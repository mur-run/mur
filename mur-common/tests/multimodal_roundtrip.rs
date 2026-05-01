use chrono::Utc;
use mur_common::multimodal::{ArtifactKind, MultimodalArtifact, ProvenanceEntry};

#[test]
fn artifact_yaml_roundtrip() {
    let a = MultimodalArtifact {
        sha256: "0".repeat(64),
        kind: ArtifactKind::Image,
        mime: "image/png".into(),
        size_bytes: 4096,
        ocr_text: Some("hello world".into()),
        page_count: None,
        created_at: Utc::now(),
        decoder_version: "image-rs/0.25 + libheif-rs/1.0".into(),
        ocr_engine_version: Some("Vision.framework/14.5".into()),
    };
    let s = serde_json::to_string(&a).unwrap();
    assert!(s.contains("\"kind\":\"image\""));
    let back: MultimodalArtifact = serde_json::from_str(&s).unwrap();
    assert_eq!(back.sha256, a.sha256);
    assert_eq!(back.ocr_text, a.ocr_text);
}

#[test]
fn provenance_entry_jsonl_roundtrip() {
    let p = ProvenanceEntry {
        sha256: "0".repeat(64),
        source: "user_drop".into(),
        decoder_version: "image-rs/0.25".into(),
        ocr_engine_version: Some("vision/14.5".into()),
        turn_id: 42,
        recorded_at: Utc::now(),
    };
    let line = serde_json::to_string(&p).unwrap();
    assert!(!line.contains('\n'), "jsonl entries must be single-line");
    let back: ProvenanceEntry = serde_json::from_str(&line).unwrap();
    assert_eq!(back.turn_id, 42);
}
