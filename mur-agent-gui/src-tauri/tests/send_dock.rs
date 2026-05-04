//! Track C3 — M-c3.4 drag-to-dock channel tests.
//!
//! Layered like the previous channel suites:
//! - M-c3.4.1: `tauri.conf.json` declares `fileAssociations` for
//!   text/url/image/pdf so the dock icon highlights for the right
//!   kinds.
//! - M-c3.4.2: pure `classify_path(&Path)` routes file extensions to
//!   `ShareKind::Image` vs `ShareKind::File`.
//! - M-c3.4.3: end-to-end via `MockApp::simulate_opened` — a synthetic
//!   `RunEvent::Opened { urls }` reaches the ingestor as a sequence of
//!   `SharePayload`s with `source = "dock"`.

#[cfg(target_os = "macos")]
use mur_agent_gui_lib::send::ShareKind;
#[cfg(target_os = "macos")]
use mur_agent_gui_lib::send::dock::classify_path;
#[cfg(target_os = "macos")]
use mur_agent_gui_lib::test_harness::MockApp;

#[cfg(target_os = "macos")]
#[test]
fn classify_path_routes_extensions_correctly() {
    use std::path::PathBuf;

    assert!(matches!(
        classify_path(&PathBuf::from("/tmp/a.png")),
        ShareKind::Image(_)
    ));
    assert!(matches!(
        classify_path(&PathBuf::from("/tmp/a.txt")),
        ShareKind::File(_)
    ));
    assert!(matches!(
        classify_path(&PathBuf::from("/tmp/a.pdf")),
        ShareKind::File(_)
    ));
    assert!(matches!(
        classify_path(&PathBuf::from("/tmp/Photo.HEIC")),
        ShareKind::Image(_)
    ));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn opened_event_routes_each_url_through_ingestor() {
    let tmp = tempfile::tempdir().unwrap();
    let app = MockApp::new(tmp.path(), "coach");

    let p1 = tmp.path().join("a.png");
    let p2 = tmp.path().join("b.txt");
    let p3 = tmp.path().join("c.pdf");
    std::fs::write(&p1, b"\x89PNG\r\n\x1a\n").unwrap();
    std::fs::write(&p2, b"hi").unwrap();
    std::fs::write(&p3, b"%PDF-1.4").unwrap();

    app.simulate_opened(&[p1.clone(), p2.clone(), p3.clone()])
        .await
        .unwrap();

    let captured = app.captured_payloads();
    assert_eq!(captured.len(), 3, "three drops → three ingest calls");
    for p in &captured {
        assert_eq!(p.source, "dock");
    }
    assert!(matches!(captured[0].kind, ShareKind::Image(_)));
    assert!(matches!(captured[1].kind, ShareKind::File(_)));
    assert!(matches!(captured[2].kind, ShareKind::File(_)));
}

#[cfg(target_os = "macos")]
#[tokio::test]
async fn opened_event_with_empty_url_list_is_a_noop() {
    let tmp = tempfile::tempdir().unwrap();
    let app = MockApp::new(tmp.path(), "coach");

    app.simulate_opened(&[]).await.unwrap();

    assert!(
        app.captured_payloads().is_empty(),
        "empty drag must not synthesize phantom payloads"
    );
}

#[test]
fn file_associations_cover_text_url_image_pdf() {
    let raw = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"),
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    // Tauri 2 mounts `fileAssociations` at the top level of `bundle`,
    // not under `bundle.macOS`. The bundle pipeline still emits the
    // matching macOS Info.plist `CFBundleDocumentTypes` from this list.
    let assocs = v
        .pointer("/bundle/fileAssociations")
        .and_then(|x| x.as_array())
        .expect("bundle.fileAssociations must be present");
    let names: Vec<String> = assocs
        .iter()
        .filter_map(|a| a.get("name").and_then(|n| n.as_str().map(String::from)))
        .collect();
    for want in ["text", "url", "image", "png", "jpeg", "pdf"] {
        assert!(
            names.iter().any(|n| n == want),
            "missing fileAssociation for {want} (saw {names:?})"
        );
    }
}
