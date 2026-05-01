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
