//! Track C3 — M-c3.3 macOS Services menu channel tests.
//!
//! The whole channel is gated `#[cfg(target_os = "macos")]` because it
//! depends on `objc2-app-kit` (NSPasteboard) and the Cocoa runtime.
//! Linux / Windows builds compile a no-op stub so the integration test
//! still links.
//!
//! - M-c3.3.1: type exists + module compiles when objc2 deps are in
//!   place.
//! - M-c3.3.2: `extract_payload_from_pasteboard` decodes text/url/image
//!   into a `SharePayload`.
//! - M-c3.3.3: `Info.plist` injection (covered in
//!   `mur-core/tests/agent_export_gui_nsservices.rs`).
//! - M-c3.3.4: `MockApp::invoke_services_selector` end-to-end via
//!   harness, bypasses NSApplication round-trip.

#![cfg(target_os = "macos")]

use mur_agent_gui_lib::send::ShareKind;
use mur_agent_gui_lib::send::services::test_helpers::{
    empty_pasteboard, pasteboard_with_image, pasteboard_with_text,
};
use mur_agent_gui_lib::send::services::{ServicesProvider, extract_payload_from_pasteboard};
use mur_agent_gui_lib::test_harness::MockApp;

#[test]
fn services_module_compiles_on_macos() {
    // The type only exists on macOS; this test asserts the module is
    // wired up and the objc2 deps resolve.
    let _ = std::any::TypeId::of::<ServicesProvider>();
}

#[test]
fn pasteboard_text_extraction_yields_text_payload() {
    let pb = pasteboard_with_text("from-services");
    let p = extract_payload_from_pasteboard(&pb).unwrap();
    assert_eq!(p.source, "services");
    match p.kind {
        ShareKind::Text(t) => assert_eq!(t, "from-services"),
        other => panic!("expected ShareKind::Text, got {other:?}"),
    }
}

#[test]
fn pasteboard_url_text_classifies_as_url() {
    let pb = pasteboard_with_text("https://example.com/post/9");
    let p = extract_payload_from_pasteboard(&pb).unwrap();
    match p.kind {
        ShareKind::Url(u) => assert_eq!(u, "https://example.com/post/9"),
        other => panic!("expected ShareKind::Url, got {other:?}"),
    }
}

#[test]
fn pasteboard_image_extraction_writes_temp_file() {
    let bytes = std::fs::read("tests/fixtures/tiny.png").unwrap();
    let pb = pasteboard_with_image(&bytes);
    let p = extract_payload_from_pasteboard(&pb).unwrap();
    let path = match p.kind {
        ShareKind::Image(ref pp) => pp.clone(),
        other => panic!("expected ShareKind::Image, got {other:?}"),
    };
    let written = std::fs::read(&path).unwrap();
    assert_eq!(written, bytes);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn empty_pasteboard_returns_err() {
    let pb = empty_pasteboard();
    let err = extract_payload_from_pasteboard(&pb).unwrap_err();
    assert!(
        err.to_string().contains("nothing to share"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn services_selector_invokes_ingestor_with_text() {
    let tmp = tempfile::tempdir().unwrap();
    let app = MockApp::new(tmp.path(), "coach");
    let pb = pasteboard_with_text("services-text");

    app.invoke_services_selector(&pb).await.unwrap();

    let captured = app.captured_payloads();
    assert_eq!(captured.len(), 1, "expected one payload, got {captured:?}");
    assert_eq!(captured[0].source, "services");
    match &captured[0].kind {
        ShareKind::Text(t) => assert_eq!(t, "services-text"),
        other => panic!("expected ShareKind::Text, got {other:?}"),
    }
}

#[tokio::test]
async fn services_selector_routes_image_through_ingestor() {
    let tmp = tempfile::tempdir().unwrap();
    let app = MockApp::new(tmp.path(), "coach");
    let bytes = std::fs::read("tests/fixtures/tiny.png").unwrap();
    let pb = pasteboard_with_image(&bytes);

    app.invoke_services_selector(&pb).await.unwrap();

    let captured = app.captured_payloads();
    assert_eq!(captured.len(), 1);
    match &captured[0].kind {
        ShareKind::Image(path) => {
            let written = std::fs::read(path).unwrap();
            assert_eq!(written, bytes);
            let _ = std::fs::remove_file(path);
        }
        other => panic!("expected ShareKind::Image, got {other:?}"),
    }
}

#[tokio::test]
async fn services_selector_with_empty_pasteboard_does_not_record() {
    let tmp = tempfile::tempdir().unwrap();
    let app = MockApp::new(tmp.path(), "coach");
    let pb = empty_pasteboard();

    let res = app.invoke_services_selector(&pb).await;
    assert!(res.is_err(), "empty pasteboard must propagate error");
    assert!(
        app.captured_payloads().is_empty(),
        "ingestor must not record anything when extraction fails"
    );
}
