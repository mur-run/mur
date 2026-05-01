use mur_agent_gui_lib::multimodal::pipeline::{MultimodalPipeline, PipelineInput};
use mur_common::multimodal::{ArtifactKind, ProvenanceLedger};
use tempfile::TempDir;

fn set_decoder_env() {
    unsafe {
        std::env::set_var(
            "MUR_AGENT_DECODER_BIN",
            env!("CARGO_BIN_EXE_mur-agent-decoder"),
        );
    }
    // Prefer the repo-local pdfium-binaries drop if it exists. CI runs
    // can override PDFIUM_DYNAMIC_LIB_PATH directly; this just keeps the
    // local dev workflow zero-config.
    if std::env::var_os("PDFIUM_DYNAMIC_LIB_PATH").is_none() {
        let manifest_dir = env!("CARGO_MANIFEST_DIR");
        // mur-agent-gui/src-tauri/.. -> mur-agent-gui ; ../.. -> repo root
        let repo_root = std::path::Path::new(manifest_dir)
            .parent()
            .and_then(|p| p.parent());
        if let Some(root) = repo_root {
            let candidate = root.join(".pdfium-bin").join("lib");
            if candidate.exists() {
                unsafe {
                    std::env::set_var("PDFIUM_DYNAMIC_LIB_PATH", &candidate);
                }
            }
        }
    }
}

#[tokio::test]
async fn drop_pdf_produces_pdf_artifact_with_page_count() {
    set_decoder_env();
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("telemetry")).unwrap();
    let pdf = include_bytes!("fixtures/prompt-injection.pdf").to_vec();
    let pipeline = MultimodalPipeline::new(tmp.path().to_path_buf(), 1);
    let a = pipeline
        .process(PipelineInput::Bytes {
            bytes: pdf,
            mime_hint: "application/pdf".into(),
            source: "user_drop".into(),
        })
        .await
        .unwrap();
    assert_eq!(a.kind, ArtifactKind::Pdf);
    assert_eq!(a.mime, "application/pdf");
    assert_eq!(a.page_count, Some(1));
    let text = a.ocr_text.as_deref().unwrap_or_default();
    assert!(text.contains("Hello!"), "visible text present: {text}");
    assert!(
        text.contains("ignore previous instructions"),
        "invisible (quarantined) text still extracted: {text}",
    );
    // The page-N separator format includes the quarantined marker.
    assert!(
        text.contains("quarantined"),
        "quarantine marker in joined text: {text}"
    );

    // Ledger entry written.
    let ledger = ProvenanceLedger::new(tmp.path().join("telemetry/inputs.jsonl"));
    let entries = ledger.read_turn(1).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].source, "user_drop");
}

#[tokio::test]
async fn pipeline_pdf_path_input_works() {
    set_decoder_env();
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("telemetry")).unwrap();
    let pdf_path = tmp.path().join("hello.pdf");
    std::fs::write(&pdf_path, include_bytes!("fixtures/prompt-injection.pdf")).unwrap();
    let pipeline = MultimodalPipeline::new(tmp.path().to_path_buf(), 2);
    let a = pipeline
        .process(PipelineInput::Path {
            path: pdf_path,
            source: "user_drop".into(),
        })
        .await
        .unwrap();
    assert_eq!(a.kind, ArtifactKind::Pdf);
    assert_eq!(a.mime, "application/pdf");
    assert_eq!(a.page_count, Some(1));
}
