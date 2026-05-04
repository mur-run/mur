//! Track C3 — M-c3.1 URL scheme channel tests.
//!
//! Covers:
//! - tauri.conf.json carries a per-agent `muragent-<slug>` URL scheme
//!   placeholder (M-c3.1.1)
//! - `parse_share_url` decodes deep-link URLs into `SharePayload`
//!   (M-c3.1.2)
//! - End-to-end: deep-link event reaches the ingestor via the
//!   pure-Rust `MockApp` test harness (M-c3.1.3)

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
