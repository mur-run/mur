//! Track C3 / M-c3.1.4 — verify `rewrite_url_scheme` substitutes the
//! per-agent slug into the `muragent-{{AGENT_SLUG}}` placeholder under
//! `plugins.deep-link.desktop.schemes`.

use mur_core::cmd::agent_export_gui::rewrite_url_scheme;

#[test]
fn rewrite_substitutes_agent_slug() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("tauri.conf.json"),
        r#"{
            "plugins": {
                "deep-link": {
                    "desktop": {
                        "schemes": ["muragent-{{AGENT_SLUG}}"]
                    }
                }
            }
        }"#,
    )
    .unwrap();
    rewrite_url_scheme(tmp.path(), "coach").unwrap();
    let raw = std::fs::read_to_string(tmp.path().join("tauri.conf.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v.pointer("/plugins/deep-link/desktop/schemes/0")
            .and_then(|x| x.as_str()),
        Some("muragent-coach"),
    );
}

#[test]
fn rewrite_is_idempotent() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("tauri.conf.json"),
        r#"{
            "plugins": {
                "deep-link": {
                    "desktop": {
                        "schemes": ["muragent-{{AGENT_SLUG}}"]
                    }
                }
            }
        }"#,
    )
    .unwrap();
    rewrite_url_scheme(tmp.path(), "coach").unwrap();
    rewrite_url_scheme(tmp.path(), "coach").unwrap();
    let raw = std::fs::read_to_string(tmp.path().join("tauri.conf.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    assert_eq!(
        v.pointer("/plugins/deep-link/desktop/schemes/0")
            .and_then(|x| x.as_str()),
        Some("muragent-coach"),
    );
}

#[test]
fn rewrite_tolerates_missing_plugins_block() {
    let tmp = tempfile::tempdir().unwrap();
    let raw = r#"{ "productName": "MurAgent" }"#;
    std::fs::write(tmp.path().join("tauri.conf.json"), raw).unwrap();
    // Should be a no-op, not an error.
    rewrite_url_scheme(tmp.path(), "coach").unwrap();
    let after = std::fs::read_to_string(tmp.path().join("tauri.conf.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&after).unwrap();
    assert_eq!(
        v.get("productName").and_then(|x| x.as_str()),
        Some("MurAgent")
    );
}

#[test]
fn rewrite_handles_multiple_schemes() {
    let tmp = tempfile::tempdir().unwrap();
    std::fs::write(
        tmp.path().join("tauri.conf.json"),
        r#"{
            "plugins": {
                "deep-link": {
                    "desktop": {
                        "schemes": [
                            "muragent-{{AGENT_SLUG}}",
                            "muragent-{{AGENT_SLUG}}-dev"
                        ]
                    }
                }
            }
        }"#,
    )
    .unwrap();
    rewrite_url_scheme(tmp.path(), "draft").unwrap();
    let raw = std::fs::read_to_string(tmp.path().join("tauri.conf.json")).unwrap();
    let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let schemes = v
        .pointer("/plugins/deep-link/desktop/schemes")
        .and_then(|s| s.as_array())
        .unwrap();
    assert_eq!(schemes[0].as_str(), Some("muragent-draft"));
    assert_eq!(schemes[1].as_str(), Some("muragent-draft-dev"));
}
