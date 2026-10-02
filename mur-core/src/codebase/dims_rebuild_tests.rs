use super::*;

#[test]
fn dims_mismatch_forces_rebuild_legacy_does_not() {
    assert!(dims_require_rebuild(Some(2560), 1024));
    assert!(!dims_require_rebuild(Some(1024), 1024));
    // Legacy metadata without recorded dims: stamp, don't rebuild.
    assert!(!dims_require_rebuild(None, 1024));
}

/// Legacy meta.json (no `dimensions` key) must parse to None.
#[test]
fn legacy_meta_json_parses_dimensions_none() {
    let legacy = r#"{"project_path":"/p","files":{},"last_indexed":"2026-01-01T00:00:00Z"}"#;
    let meta: IndexMetadata = serde_json::from_str(legacy).unwrap();
    assert_eq!(meta.dimensions, None);
    // And a stamped one round-trips.
    let stamped = IndexMetadata {
        dimensions: Some(1024),
        ..IndexMetadata::default()
    };
    let json = serde_json::to_string(&stamped).unwrap();
    let back: IndexMetadata = serde_json::from_str(&json).unwrap();
    assert_eq!(back.dimensions, Some(1024));
}
