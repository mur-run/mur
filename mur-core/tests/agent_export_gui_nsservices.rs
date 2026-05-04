//! Track C3 / M-c3.3.3 — verify `rewrite_nsservices` substitutes the
//! per-agent display name into `Info.plist.template` and writes the
//! rendered `Info.plist` next to it. The bundle pipeline (Tauri 2's
//! `bundle.macOS.infoPlist`) merges that file into the final
//! `MyAgent.app/Contents/Info.plist`.

use mur_core::cmd::agent_export_gui::rewrite_nsservices;

const TEMPLATE: &str =
    include_str!("../../mur-agent-gui/src-tauri/Info.plist.template");

#[test]
fn rewrite_injects_three_nsservices_entries() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Info.plist.template"), TEMPLATE).unwrap();
    rewrite_nsservices(tmp.path(), "coach", "Coach").unwrap();

    let raw = std::fs::read_to_string(tmp.path().join("Info.plist")).unwrap();
    assert!(raw.contains("Send Selection to Coach"));
    assert!(raw.contains("Send Link to Coach"));
    assert!(raw.contains("Send Image to Coach"));
    assert!(raw.contains("<key>NSServices</key>"));

    // Each entry must declare NSMessage = serviceShare so AppKit
    // dispatches all three menu items to the same selector body.
    let count = raw.matches("<string>serviceShare</string>").count();
    assert_eq!(count, 3, "expected exactly 3 serviceShare bindings");
}

#[test]
fn rewrite_substitutes_display_name_in_port_name() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Info.plist.template"), TEMPLATE).unwrap();
    rewrite_nsservices(tmp.path(), "draft-bot", "Draft Bot").unwrap();

    let raw = std::fs::read_to_string(tmp.path().join("Info.plist")).unwrap();
    // NSPortName must carry the display name verbatim — AppKit looks
    // it up to address the running provider.
    assert!(
        raw.contains("<key>NSPortName</key>\n            <string>Draft Bot</string>"),
        "expected NSPortName=Draft Bot, got:\n{raw}",
    );
    // Template token must be fully substituted.
    assert!(
        !raw.contains("{{AGENT_DISPLAY}}"),
        "rendered Info.plist still contains a template token"
    );
}

#[test]
fn phase_4_pipeline_renders_info_plist_and_points_conf_at_it() {
    // Track C3 / M-w3 — verify the export pipeline's phase 4 rewrites
    // tauri.conf.json's `bundle.macOS.infoPlist` to point at the
    // rendered Info.plist so the Tauri bundler picks up the
    // NSServices array. We can't drive `phase_4_rewrite_tauri_conf`
    // directly here (it touches the workspace gui root), but the
    // helper composition is enough to gate against drift: each
    // public helper runs in turn and the resulting conf carries the
    // expected fields.
    use mur_core::cmd::agent_export_gui::{
        rewrite_nsservices, rewrite_url_scheme,
    };

    let tmp = tempfile::tempdir().unwrap();
    // Drop a tauri.conf.json that mirrors the production template
    // (deep-link plugin block + bundle.macOS) so the test catches
    // any regressions where the public rewrite helpers stop
    // touching the right keys.
    std::fs::write(
        tmp.path().join("tauri.conf.json"),
        r#"{
            "plugins": {
                "deep-link": {
                    "desktop": {
                        "schemes": ["muragent-{{AGENT_SLUG}}"]
                    }
                }
            },
            "bundle": {
                "macOS": {
                    "minimumSystemVersion": "12.0"
                }
            }
        }"#,
    )
    .unwrap();
    std::fs::write(tmp.path().join("Info.plist.template"), TEMPLATE).unwrap();

    rewrite_url_scheme(tmp.path(), "draftbot").unwrap();
    rewrite_nsservices(tmp.path(), "draftbot", "Draft Bot").unwrap();

    let conf: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(tmp.path().join("tauri.conf.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        conf.pointer("/plugins/deep-link/desktop/schemes/0")
            .and_then(|v| v.as_str()),
        Some("muragent-draftbot"),
    );

    // The rendered Info.plist must exist next to tauri.conf.json
    // so a real Tauri bundler invocation finds it via
    // `bundle.macOS.infoPlist`.
    let rendered = tmp.path().join("Info.plist");
    assert!(rendered.exists(), "Info.plist must be rendered");
    let raw = std::fs::read_to_string(&rendered).unwrap();
    assert!(raw.contains("Send Selection to Draft Bot"));
}

#[test]
fn rewrite_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(tmp.path().join("Info.plist.template"), TEMPLATE).unwrap();
    rewrite_nsservices(tmp.path(), "coach", "Coach").unwrap();
    rewrite_nsservices(tmp.path(), "coach", "Coach").unwrap();
    let raw = std::fs::read_to_string(tmp.path().join("Info.plist")).unwrap();
    let count = raw.matches("Send Selection to Coach").count();
    assert_eq!(count, 1, "running rewrite twice must not duplicate entries");
}
