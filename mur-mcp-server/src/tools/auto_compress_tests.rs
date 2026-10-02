use super::*;
use mur_compress::{CompressConfig, CompressEngine};
use serde_json::json;

fn engine() -> (tempfile::TempDir, CompressEngine) {
    let dir = tempfile::tempdir().unwrap();
    let eng = CompressEngine::new(dir.path().to_path_buf(), CompressConfig::default()).unwrap();
    (dir, eng)
}

fn big_search_output() -> Value {
    let results: Vec<Value> = (0..3000)
            .map(|i| json!({"file": format!("src/f{i}.rs"), "score": 0.5, "content": format!("fn item_{i}() {{}}")}))
            .collect();
    json!({"results": results, "count": 3000})
}

#[test]
fn skips_compression_tools() {
    let (_dir, eng) = engine();
    let auto = AutoCfg {
        enabled: true,
        min_tokens: 1,
        mcp: true,
        agent_runtime: true,
        claude_hook: true,
    };
    let big = big_search_output();
    let out = apply_auto_compress(&eng, &auto, "mur_compress", &json!({}), big.clone());
    assert_eq!(out, big, "compression tools must pass through");
}

#[test]
fn small_output_unchanged() {
    let (_dir, eng) = engine();
    let auto = AutoCfg::default();
    let small = json!({"results": ["a", "b"], "count": 2});
    let out = apply_auto_compress(
        &eng,
        &auto,
        "mur_project_search",
        &json!({"query": "x"}),
        small.clone(),
    );
    assert_eq!(out, small);
}

#[test]
fn large_search_output_compressed_with_query() {
    let (_dir, eng) = engine();
    let auto = AutoCfg {
        enabled: true,
        min_tokens: 50,
        mcp: true,
        agent_runtime: true,
        claude_hook: true,
    };
    let out = apply_auto_compress(
        &eng,
        &auto,
        "mur_project_search",
        &json!({"query": "item"}),
        big_search_output(),
    );
    assert_eq!(out["count"], json!(3000));
    assert_eq!(out["results"]["compressed"], json!(true));
    assert!(out["results"]["hash"].as_str().is_some());
    assert!(
        out["results"]["note"]
            .as_str()
            .unwrap()
            .contains("mur_retrieve")
    );
}

#[test]
fn disabled_auto_passes_through() {
    let (_dir, eng) = engine();
    let auto = AutoCfg {
        enabled: false,
        ..AutoCfg::default()
    };
    let big = big_search_output();
    let out = apply_auto_compress(&eng, &auto, "mur_project_search", &json!({}), big.clone());
    assert_eq!(out, big);
}
