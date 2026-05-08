use mur_agent_gui_lib::multimodal::pipeline::{MultimodalPipeline, PipelineInput};
use mur_common::multimodal::{ArtifactKind, ProvenanceLedger};
use tempfile::TempDir;

#[tokio::test]
async fn drop_png_produces_artifact_and_ledger_entry() {
    // Set MUR_AGENT_DECODER_BIN so DecoderClient finds the test build.
    unsafe {
        std::env::set_var(
            "MUR_AGENT_DECODER_BIN",
            env!("CARGO_BIN_EXE_mur-agent-decoder"),
        );
    }

    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path();
    std::fs::create_dir_all(agent_home.join("telemetry")).unwrap();

    let png = include_bytes!("fixtures/tiny.png").to_vec();

    let pipeline = MultimodalPipeline::new(agent_home.to_path_buf(), 1);
    let artifact = pipeline
        .process(PipelineInput::Bytes {
            bytes: png,
            mime_hint: "image/png".into(),
            source: "user_drop".into(),
        })
        .await
        .unwrap();

    assert_eq!(artifact.kind, ArtifactKind::Image);
    assert_eq!(artifact.mime, "image/png");
    assert_eq!(artifact.sha256.len(), 64);
    assert!(artifact.ocr_text.is_none(), "OCR lands in M3.5");
    assert!(artifact.size_bytes > 0);

    // Ledger entry written.
    let ledger = ProvenanceLedger::new(agent_home.join("telemetry/inputs.jsonl"));
    let entries = ledger.read_turn(1).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].sha256, artifact.sha256);
    assert_eq!(entries[0].source, "user_drop");
    assert_eq!(entries[0].turn_id, 1);
}

#[tokio::test]
async fn pipeline_from_path_reads_file_then_processes() {
    unsafe {
        std::env::set_var(
            "MUR_AGENT_DECODER_BIN",
            env!("CARGO_BIN_EXE_mur-agent-decoder"),
        );
    }

    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("telemetry")).unwrap();
    let png_path = tmp.path().join("hello.png");
    std::fs::write(&png_path, include_bytes!("fixtures/tiny.png")).unwrap();

    let pipeline = MultimodalPipeline::new(tmp.path().to_path_buf(), 7);
    let a = pipeline
        .process(PipelineInput::Path {
            path: png_path,
            source: "user_drop".into(),
        })
        .await
        .unwrap();
    assert_eq!(a.kind, ArtifactKind::Image);
    assert_eq!(a.mime, "image/png");
}

#[tokio::test]
async fn pipeline_image_records_platform_ocr_engine_version() {
    unsafe {
        std::env::set_var(
            "MUR_AGENT_DECODER_BIN",
            env!("CARGO_BIN_EXE_mur-agent-decoder"),
        );
    }
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("telemetry")).unwrap();
    let png = include_bytes!("fixtures/tiny.png").to_vec();
    let pipeline = MultimodalPipeline::new(tmp.path().to_path_buf(), 1);
    let a = pipeline
        .process(PipelineInput::Bytes {
            bytes: png,
            mime_hint: "image/png".into(),
            source: "user_drop".into(),
        })
        .await
        .unwrap();
    // A 1×1 transparent PNG has no text; OCR should return None or empty
    // (pipeline collapses empty → None).
    assert!(a.ocr_text.is_none());
    // engine_version is always set by the platform engine (M3.5.2/M3.5.3).
    let ev = a
        .ocr_engine_version
        .as_deref()
        .expect("engine version must be set");
    #[cfg(target_os = "macos")]
    assert!(
        ev.starts_with("macos-vision/"),
        "expected macos-vision prefix, got {ev}"
    );
    #[cfg(not(target_os = "macos"))]
    assert!(
        ev.starts_with("tesseract-cli/") || ev == "noop/1.0",
        "unexpected engine version: {ev}"
    );

    // Provenance entry's ocr_engine_version is also recorded.
    let ledger =
        mur_common::multimodal::ProvenanceLedger::new(tmp.path().join("telemetry/inputs.jsonl"));
    let entries = ledger.read_turn(1).unwrap();
    assert_eq!(entries.len(), 1);
    let pev = entries[0]
        .ocr_engine_version
        .as_deref()
        .expect("ledger engine version must be set");
    assert_eq!(pev, ev, "ledger engine version must match artifact");
}

#[tokio::test]
async fn pipeline_image_writes_text_sidecar() {
    unsafe {
        std::env::set_var(
            "MUR_AGENT_DECODER_BIN",
            env!("CARGO_BIN_EXE_mur-agent-decoder"),
        );
    }
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("telemetry")).unwrap();
    let png = include_bytes!("fixtures/tiny.png").to_vec();
    let pipeline = MultimodalPipeline::new(tmp.path().to_path_buf(), 1);
    let a = pipeline
        .process(PipelineInput::Bytes {
            bytes: png,
            mime_hint: "image/png".into(),
            source: "user_drop".into(),
        })
        .await
        .unwrap();
    let txt_path = tmp
        .path()
        .join(format!("telemetry/inputs/{}.txt", a.sha256));
    assert!(
        txt_path.exists(),
        "text sidecar should exist at {}",
        txt_path.display()
    );
    // For Noop OCR, file is empty (still created so M3.8.1 doesn't error).
    let body = std::fs::read_to_string(&txt_path).unwrap();
    assert_eq!(body, "");
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn pipeline_heic_normalizes_then_decodes() {
    unsafe {
        std::env::set_var(
            "MUR_AGENT_DECODER_BIN",
            env!("CARGO_BIN_EXE_mur-agent-decoder"),
        );
    }
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("telemetry")).unwrap();
    // Reuse the M3.3.3 fixture (804-byte HEIC produced via sips).
    let heic = include_bytes!("fixtures/exif-gps.heic").to_vec();
    let pipeline = MultimodalPipeline::new(tmp.path().to_path_buf(), 9);
    let a = pipeline
        .process(PipelineInput::Bytes {
            bytes: heic,
            mime_hint: "image/heic".into(),
            source: "user_drop".into(),
        })
        .await
        .unwrap();
    assert_eq!(a.kind, ArtifactKind::Image);
    // After HEIC → PNG normalization, the artifact's MIME is
    // canonicalized to image/png (we re-encode through image-rs).
    assert_eq!(a.mime, "image/png");
    assert!(a.size_bytes > 0);

    // Provenance entry recorded under turn_id 9.
    let ledger = ProvenanceLedger::new(tmp.path().join("telemetry/inputs.jsonl"));
    let entries = ledger.read_turn(9).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, "user_drop");
}
