//! Track C3 — M-c3.2 global hotkey channel tests.
//!
//! Layered like the URL scheme suite:
//! - M-c3.2.1: pure `default_combo_for(slug)` derives a per-agent
//!   `CommandOrControl+Shift+M+<FIRST_LETTER>` combo.
//! - M-c3.2.2: `Clipboard` trait + `synthesize_from_clipboard`
//!   reads text/url/image and produces a `SharePayload`.
//! - M-c3.2.3: `resolve_combo(slug, user_override)` lets the user
//!   override the per-agent default in companion settings.
//! - M-c3.2.4: end-to-end via `MockApp::trigger_shortcut` — a hotkey
//!   firing reaches the ingestor with the clipboard contents.

use mur_agent_gui_lib::send::ShareKind;
use mur_agent_gui_lib::send::hotkey::{
    FakeClipboard, default_combo_for, resolve_combo, synthesize_from_clipboard,
};
use mur_agent_gui_lib::test_harness::MockApp;

#[test]
fn default_hotkey_combo_for_slug() {
    assert_eq!(default_combo_for("coach"), "CommandOrControl+Shift+M+C");
    assert_eq!(default_combo_for("draft"), "CommandOrControl+Shift+M+D");
}

#[test]
fn collision_when_two_agents_share_first_letter() {
    // Documented limitation: two agents starting with "c" will collide
    // on the default combo. The user has to override one of them via
    // the companion settings escape hatch — see M-c3.2.3.
    assert_eq!(default_combo_for("coach"), default_combo_for("creator"));
}

#[tokio::test]
async fn hotkey_handler_reads_text() {
    let cb = FakeClipboard::with_text("hello hotkey");
    let payload = synthesize_from_clipboard(&cb).await.unwrap();
    assert_eq!(payload.source, "hotkey");
    match payload.kind {
        ShareKind::Text(t) => assert_eq!(t, "hello hotkey"),
        other => panic!("expected ShareKind::Text, got {other:?}"),
    }
}

#[tokio::test]
async fn hotkey_handler_classifies_url_text_as_url() {
    let cb = FakeClipboard::with_text("https://example.com/article");
    let payload = synthesize_from_clipboard(&cb).await.unwrap();
    match payload.kind {
        ShareKind::Url(u) => assert_eq!(u, "https://example.com/article"),
        other => panic!("expected ShareKind::Url, got {other:?}"),
    }
}

#[tokio::test]
async fn hotkey_handler_reads_image() {
    let bytes = std::fs::read("tests/fixtures/tiny.png").unwrap();
    let cb = FakeClipboard::with_image(bytes.clone());
    let payload = synthesize_from_clipboard(&cb).await.unwrap();
    assert_eq!(payload.source, "hotkey");
    match payload.kind {
        ShareKind::Image(path) => {
            let written = std::fs::read(&path).unwrap();
            assert_eq!(written, bytes);
            // Cleanup the persisted temp file the synthesizer kept.
            let _ = std::fs::remove_file(&path);
        }
        other => panic!("expected ShareKind::Image, got {other:?}"),
    }
}

#[test]
fn user_override_takes_precedence_over_default() {
    // Companion settings (`share.hotkey`) win when set; the per-agent
    // default only applies when no override is configured.
    assert_eq!(
        resolve_combo("coach", Some("CommandOrControl+Alt+K")),
        "CommandOrControl+Alt+K"
    );
    assert_eq!(
        resolve_combo("coach", None),
        default_combo_for("coach"),
        "None must defer to default_combo_for"
    );
}

#[tokio::test]
async fn empty_clipboard_returns_err() {
    let cb = FakeClipboard::empty();
    let err = synthesize_from_clipboard(&cb).await.unwrap_err();
    assert!(
        err.to_string().contains("nothing to share"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn hotkey_event_reaches_ingestor() {
    let tmp = tempfile::tempdir().unwrap();
    let app = MockApp::new(tmp.path(), "coach");

    app.set_clipboard_text("snippet from any app");
    app.trigger_shortcut().await.unwrap();

    let captured = app.captured_payloads();
    assert_eq!(captured.len(), 1, "exactly one payload should be ingested");
    let p = &captured[0];
    assert_eq!(p.source, "hotkey");
    match &p.kind {
        ShareKind::Text(t) => assert_eq!(t, "snippet from any app"),
        other => panic!("expected ShareKind::Text, got {other:?}"),
    }
}

#[tokio::test]
async fn hotkey_event_classifies_url_clipboard() {
    let tmp = tempfile::tempdir().unwrap();
    let app = MockApp::new(tmp.path(), "coach");

    app.set_clipboard_text("https://example.com/post/42");
    app.trigger_shortcut().await.unwrap();

    let captured = app.captured_payloads();
    assert_eq!(captured.len(), 1);
    match &captured[0].kind {
        ShareKind::Url(u) => assert_eq!(u, "https://example.com/post/42"),
        other => panic!("expected ShareKind::Url, got {other:?}"),
    }
}

#[tokio::test]
async fn hotkey_with_empty_clipboard_does_not_record() {
    let tmp = tempfile::tempdir().unwrap();
    let app = MockApp::new(tmp.path(), "coach");

    let res = app.trigger_shortcut().await;
    assert!(res.is_err(), "trigger on empty clipboard must error");
    assert!(
        app.captured_payloads().is_empty(),
        "ingestor must not record anything when synthesis fails"
    );
}
