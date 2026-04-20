//! Real-Ollama smoke test (Phase 2C). Requires a running Ollama on
//! http://localhost:11434 with the configured extractive/abstractive models
//! pulled. Run locally with:
//!   cargo test -p mur-core --features ollama-live-smoke -- --ignored
//! NOT run by default in CI — would flake without Ollama.
#![cfg(feature = "ollama-live-smoke")]

use std::process::Command;

#[ignore]
#[test]
fn compact_against_real_ollama() {
    let tmp = tempfile::tempdir().unwrap();
    let yesterday = (chrono::Utc::now().date_naive() - chrono::Duration::days(1))
        .format("%Y-%m-%d")
        .to_string();
    let mur_home = tmp.path().join(".mur");
    let raw = mur_home.join("conversations/raw").join(&yesterday);
    std::fs::create_dir_all(&raw).unwrap();

    // Seed a single message with substance — the extractive LLM should pick it.
    let line = serde_json::json!({
        "v": 1,
        "ts": format!("{yesterday}T10:00:00Z"),
        "src": "claude-code",
        "conv": "smoke",
        "role": "user",
        "content": {"t": "text", "v": "We decided to use RaBitQ compression for the vector index because it achieves 32x reduction with under 1% recall loss at k=10."},
        "meta": {},
        "refs": []
    });
    std::fs::write(
        raw.join("cc_smoke.jsonl"),
        serde_json::to_string(&line).unwrap() + "\n",
    )
    .unwrap();

    // Run real binary against real Ollama (no MUR_OLLAMA_MOCK).
    let out = Command::new(env!("CARGO_BIN_EXE_mur"))
        .args(["conversations", "compact", "--date", &yesterday])
        .env("MUR_HOME", &mur_home)
        .output()
        .expect("failed to run mur");

    if !out.status.success() {
        eprintln!(
            "mur compact failed — is Ollama running with qwen3:14b + command-r:latest pulled?\nstderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        panic!("mur compact did not exit successfully");
    }

    // Verify the written summary has real content.
    let md = mur_home
        .join("conversations/summary")
        .join(format!("{yesterday}.md"));
    assert!(md.exists(), "summary not written at {md:?}");
    let body = std::fs::read_to_string(&md).unwrap();
    assert!(
        body.contains("## Extractive spans"),
        "summary missing extractive spans section; got:\n{body}"
    );
    assert!(
        body.contains("## Abstractive narrative"),
        "summary missing abstractive narrative section; got:\n{body}"
    );
    // A real LLM narrative should mention RaBitQ (topic of the seeded span).
    // This is a weak assertion — model output varies, but every reasonable
    // summary of a "RaBitQ compression decision" should mention RaBitQ.
    assert!(
        body.to_lowercase().contains("rabitq")
            || body.to_lowercase().contains("compression")
            || body.to_lowercase().contains("vector"),
        "narrative should reference the seeded topic (RaBitQ/compression/vector); got:\n{body}"
    );
}
