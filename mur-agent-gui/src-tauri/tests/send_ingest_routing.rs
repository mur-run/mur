//! M-c3.0.2: `DefaultIngestor::ingest` routing.
//!
//! Adaptation note (Track C3 plan): the original M-c3.0.2 plan called
//! for a separate `share.jsonl` ledger. We collapsed share onto the
//! existing `telemetry/inputs.jsonl` (with a `--- share` sidecar
//! marker) so B0SafetyHook only has one ledger to scan.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use mur_agent_gui_lib::send::{
    DefaultIngestor, SendIngestor, ShareEmitter, ShareKind, SharePayload,
};

struct FakeEmitter {
    count: Arc<AtomicUsize>,
}

impl ShareEmitter for FakeEmitter {
    fn emit_received(&self, _p: &SharePayload) -> anyhow::Result<()> {
        self.count.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn ingest_text_writes_share_sidecar_and_ledger() {
    let tmp = tempfile::tempdir().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let ing = DefaultIngestor {
        agent_home: tmp.path().to_path_buf(),
        emitter: Arc::new(FakeEmitter {
            count: count.clone(),
        }),
    };
    let payload = SharePayload {
        source: "url_scheme".into(),
        kind: ShareKind::Text("hello world".into()),
        metadata: serde_json::json!({}),
    };
    ing.ingest(payload).await.unwrap();

    // Emitter fires exactly once.
    assert_eq!(count.load(Ordering::SeqCst), 1);

    // Ledger entry lives on the shared inputs.jsonl (NOT a separate
    // share.jsonl — see adaptation note above).
    let ledger = std::fs::read_to_string(tmp.path().join("telemetry/inputs.jsonl")).unwrap();
    assert!(
        ledger.contains("\"source\":\"share:url_scheme\""),
        "ledger missing share-tagged source: {ledger}"
    );
    assert_eq!(ledger.lines().count(), 1, "expected exactly one entry");

    // Sidecar starts with the `--- share` marker the B0 hook keys off.
    let inputs_dir = tmp.path().join("telemetry/inputs");
    let mut sidecars: Vec<PathBuf> = std::fs::read_dir(&inputs_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("txt"))
        .collect();
    assert_eq!(sidecars.len(), 1, "expected exactly one sidecar");
    let sidecar = sidecars.pop().unwrap();
    let body = std::fs::read_to_string(&sidecar).unwrap();
    assert!(
        body.starts_with("--- share\n"),
        "sidecar missing marker: {body}"
    );
    assert!(body.contains("hello world"));
}

#[tokio::test]
async fn ingest_url_kind_uses_share_marker() {
    let tmp = tempfile::tempdir().unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let ing = DefaultIngestor {
        agent_home: tmp.path().to_path_buf(),
        emitter: Arc::new(FakeEmitter {
            count: count.clone(),
        }),
    };
    let payload = SharePayload {
        source: "hotkey".into(),
        kind: ShareKind::Url("https://attacker.example/x".into()),
        metadata: serde_json::json!({}),
    };
    ing.ingest(payload).await.unwrap();

    assert_eq!(count.load(Ordering::SeqCst), 1);
    let ledger = std::fs::read_to_string(tmp.path().join("telemetry/inputs.jsonl")).unwrap();
    assert!(ledger.contains("\"source\":\"share:hotkey\""));
}

#[tokio::test]
async fn ingest_image_routes_through_process_artifact() {
    let tmp = tempfile::tempdir().unwrap();
    // Stage a tiny "image" file on disk; mime_guess infers from the
    // .png extension. process_artifact's image branch writes an empty
    // sidecar, which is fine — the routing assertion is the source
    // tag, not the body.
    let img_path = tmp.path().join("foo.png");
    std::fs::write(&img_path, b"\x89PNG\r\n\x1a\n").unwrap();

    let count = Arc::new(AtomicUsize::new(0));
    let ing = DefaultIngestor {
        agent_home: tmp.path().to_path_buf(),
        emitter: Arc::new(FakeEmitter {
            count: count.clone(),
        }),
    };
    let payload = SharePayload {
        source: "dock".into(),
        kind: ShareKind::Image(img_path),
        metadata: serde_json::json!({}),
    };
    ing.ingest(payload).await.unwrap();

    assert_eq!(count.load(Ordering::SeqCst), 1);
    // process_artifact emits `user_drop` source (not `share:*`) — that
    // confirms we routed via the binary path, not the share path.
    let ledger = std::fs::read_to_string(tmp.path().join("telemetry/inputs.jsonl")).unwrap();
    assert!(
        ledger.contains("\"source\":\"user_drop\""),
        "ledger missing user_drop tag: {ledger}"
    );
}
