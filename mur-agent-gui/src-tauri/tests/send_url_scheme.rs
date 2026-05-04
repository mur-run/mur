//! Track C3 — M-c3.1 URL scheme channel tests.
//!
//! Covers:
//! - tauri.conf.json carries a per-agent `muragent-<slug>` URL scheme
//!   placeholder (M-c3.1.1)
//! - `parse_share_url` decodes deep-link URLs into `SharePayload`
//!   (M-c3.1.2)
//! - End-to-end: deep-link event reaches the ingestor via the
//!   pure-Rust `MockApp` test harness (M-c3.1.3)

use base64::Engine;
use mur_agent_gui_lib::send::ShareKind;
use mur_agent_gui_lib::send::url_scheme::parse_share_url;

fn b64(body: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(body)
}

#[test]
fn parse_text_share() {
    let body = "hello world";
    let url = format!("muragent-coach://share?text={}&type=text", b64(body));
    let p = parse_share_url(&url, "coach").unwrap();
    assert_eq!(p.source, "url_scheme");
    match p.kind {
        ShareKind::Text(t) => assert_eq!(t, body),
        other => panic!("expected ShareKind::Text, got {other:?}"),
    }
}

#[test]
fn parse_url_share() {
    let body = "https://example.com/post/42";
    let url = format!("muragent-coach://share?text={}&type=url", b64(body));
    let p = parse_share_url(&url, "coach").unwrap();
    match p.kind {
        ShareKind::Url(u) => assert_eq!(u, body),
        other => panic!("expected ShareKind::Url, got {other:?}"),
    }
}

#[test]
fn parse_defaults_type_to_text() {
    let body = "no type query";
    let url = format!("muragent-coach://share?text={}", b64(body));
    let p = parse_share_url(&url, "coach").unwrap();
    assert!(matches!(p.kind, ShareKind::Text(ref t) if t == body));
}

#[test]
fn rejects_wrong_slug() {
    let body = "hello";
    let url = format!("muragent-other://share?text={}&type=text", b64(body));
    assert!(
        parse_share_url(&url, "coach").is_err(),
        "must reject scheme that targets a different agent"
    );
}

#[test]
fn rejects_wrong_host() {
    let body = "hello";
    let url = format!("muragent-coach://exec?text={}", b64(body));
    assert!(
        parse_share_url(&url, "coach").is_err(),
        "must reject hosts other than `share`"
    );
}

#[test]
fn rejects_missing_text_param() {
    let url = "muragent-coach://share?type=text";
    assert!(
        parse_share_url(url, "coach").is_err(),
        "must reject URLs missing the `text=` query parameter"
    );
}

#[test]
fn rejects_invalid_base64() {
    let url = "muragent-coach://share?text=%21%21not-base64%21%21&type=text";
    assert!(
        parse_share_url(url, "coach").is_err(),
        "must reject `text=` payloads that aren't URL_SAFE_NO_PAD base64"
    );
}

#[tokio::test]
async fn deep_link_event_reaches_ingestor() {
    let tmp = tempfile::tempdir().unwrap();
    let app = mur_agent_gui_lib::test_harness::mock_app(tmp.path(), "coach").await;
    let body = "hello";
    let url = format!("muragent-coach://share?text={}&type=text", b64(body));
    app.simulate_open_url(&url).await.unwrap();
    let payloads = app.captured_payloads();
    assert_eq!(payloads.len(), 1, "exactly one payload should be recorded");
    assert_eq!(payloads[0].source, "url_scheme");
    match &payloads[0].kind {
        ShareKind::Text(t) => assert_eq!(t, body),
        other => panic!("expected ShareKind::Text, got {other:?}"),
    }
}

#[tokio::test]
async fn deep_link_invalid_url_propagates_error() {
    let tmp = tempfile::tempdir().unwrap();
    let app = mur_agent_gui_lib::test_harness::mock_app(tmp.path(), "coach").await;
    // Wrong slug — must NOT reach the ingestor.
    let url = format!(
        "muragent-other://share?text={}&type=text",
        b64("attacker payload")
    );
    let res = app.simulate_open_url(&url).await;
    assert!(res.is_err(), "wrong-slug URLs must error, not silently route");
    assert!(
        app.captured_payloads().is_empty(),
        "ingestor must not record anything on parse failure"
    );
}

#[test]
fn url_schemes_present_in_tauri_conf() {
    // Tauri 2 rejects a top-level `bundle.macOS.urlSchemes` key, and
    // its `bundle.macOS.infoPlist` field expects a path (not inline
    // JSON). The canonical home for desktop URL scheme registration in
    // Tauri 2 is the `tauri-plugin-deep-link` plugin config under
    // `plugins.deep-link.desktop.schemes` — the plugin handles the
    // `CFBundleURLTypes` Info.plist injection at bundle time on macOS.
    //
    // This test asserts the templated `muragent-{{AGENT_SLUG}}` scheme
    // is reachable so `phase_4_rewrite_tauri_conf` (M-c3.1.4) has
    // somewhere to substitute the real per-agent slug.
    let raw = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tauri.conf.json"),
    )
    .unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let schemes = v
        .pointer("/plugins/deep-link/desktop/schemes")
        .and_then(|x| x.as_array())
        .expect("deep-link desktop schemes array missing under plugins.deep-link.desktop.schemes");
    assert!(
        schemes
            .iter()
            .any(|s| s.as_str().unwrap_or("").starts_with("muragent-")),
        "muragent-<slug> URL scheme must be templated in"
    );
}
