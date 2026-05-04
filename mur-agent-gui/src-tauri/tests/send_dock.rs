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
#[test]
fn classify_path_routes_extensions_correctly() {
    use mur_agent_gui_lib::send::ShareKind;
    use mur_agent_gui_lib::send::dock::classify_path;
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
