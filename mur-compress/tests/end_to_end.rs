use mur_compress::{CompressConfig, CompressEngine, RetrieveResult};

fn engine(dir: &std::path::Path) -> CompressEngine {
    let mut cfg = CompressConfig {
        protect_head_lines: 2,
        protect_tail_lines: 1,
        ..Default::default()
    };
    cfg.store.compress_at_rest = false;
    CompressEngine::new(dir, cfg).unwrap()
}

#[test]
fn search_compress_then_retrieve_is_reversible() {
    let dir = tempfile::tempdir().unwrap();
    let eng = engine(dir.path());
    let mut lines = Vec::new();
    for i in 0..40 {
        lines.push(format!("src/file{i}.rs:{i}:some content number {i}"));
    }
    let input = lines.join("\n");

    let res = eng.compress(&input, Some("content number 7"));
    assert!(res.hash.is_some(), "large search output should offload");
    assert!(res.tokens_saved > 0);

    // Full retrieve reproduces the original exactly.
    match eng.retrieve(res.hash.as_ref().unwrap(), None) {
        RetrieveResult::Full {
            original_content,
            item_count,
            ..
        } => {
            assert_eq!(original_content, input);
            assert_eq!(item_count, 40);
        }
        _ => panic!("expected Full"),
    }

    // Query-filtered retrieve returns relevant items.
    match eng.retrieve(res.hash.as_ref().unwrap(), Some("number 7")) {
        RetrieveResult::Filtered { count, results, .. } => {
            assert!(count > 0);
            assert!(results.iter().any(|r| r.contains("number 7")));
        }
        _ => panic!("expected Filtered"),
    }
}

#[test]
fn fail_safe_passthrough_on_generic_prose() {
    let dir = tempfile::tempdir().unwrap();
    let eng = engine(dir.path());
    let input = "This is ordinary prose that should not be aggressively compressed.";
    let res = eng.compress(input, None);
    // Generic compressor tries to offload, but the bloat guard rejects it
    // (marker overhead > original for short text). Result: passthrough.
    assert!(res.hash.is_none());
    assert!(res.compressed.contains("ordinary prose"));
}

#[test]
fn retrieve_unknown_hash_is_not_found() {
    let dir = tempfile::tempdir().unwrap();
    let eng = engine(dir.path());
    assert!(matches!(
        eng.retrieve("nope", None),
        RetrieveResult::NotFound
    ));
}

#[test]
fn json_array_roundtrips_through_store() {
    let dir = tempfile::tempdir().unwrap();
    let eng = engine(dir.path());
    // A large array of fat rows: the schema+sample+offload form is genuinely
    // smaller than the original, so the bloat guard keeps the compressed result.
    let rows: Vec<String> = (0..60)
        .map(|i| format!(r#"{{"id":{i},"name":"item number {i}","value":{i}}}"#))
        .collect();
    let input = format!("[{}]", rows.join(","));
    let res = eng.compress(&input, None);
    assert!(res.hash.is_some());
    assert!(res.tokens_saved > 0);
    match eng.retrieve(res.hash.as_ref().unwrap(), None) {
        RetrieveResult::Full {
            original_content, ..
        } => assert_eq!(original_content, input),
        _ => panic!("expected Full"),
    }
}
