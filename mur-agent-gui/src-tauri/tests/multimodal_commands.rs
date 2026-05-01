//! Integration tests for the M3.6 Tauri commands that wire the
//! drag-drop / paste pipeline into the React frontend.
//!
//! Each test exercises the `_impl` helper that backs the
//! `#[tauri::command]` wrapper, so we don't need a real webview.
//! `MUR_HOME` is flipped via the shared `MurHomeGuard`.

use mur_agent_gui_lib::commands::multimodal_drop_impl;
use std::path::PathBuf;
use tempfile::TempDir;

mod common;
use common::MurHomeGuard;

fn fixture_with_name(name: &str) -> String {
    let candidates = [
        "../../mur-common/tests/fixtures/profile_p0a_minimal.yaml",
        "../mur-common/tests/fixtures/profile_p0a_minimal.yaml",
        "mur-common/tests/fixtures/profile_p0a_minimal.yaml",
    ];
    let yaml = candidates
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
        .unwrap_or_else(|| {
            let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .ancestors()
                .nth(2)
                .unwrap()
                .join("mur-common/tests/fixtures/profile_p0a_minimal.yaml");
            std::fs::read_to_string(p).expect("fixture must exist")
        });
    yaml.replace("name: agent_test", &format!("name: {name}"))
}

fn seed(home: &std::path::Path, name: &str) {
    let dir = home.join(format!("agents/{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::create_dir_all(dir.join("telemetry")).unwrap();
    std::fs::write(dir.join("profile.yaml"), fixture_with_name(name)).unwrap();
}

// ─── M3.6.1 — `multimodal_drop` ────────────────────────────────────

#[tokio::test]
async fn multimodal_drop_one_png_succeeds() {
    unsafe {
        std::env::set_var(
            "MUR_AGENT_DECODER_BIN",
            env!("CARGO_BIN_EXE_mur-agent-decoder"),
        );
    }
    let tmp = TempDir::new().unwrap();
    let _g = MurHomeGuard::set(tmp.path());
    seed(tmp.path(), "drop");
    let png = tmp.path().join("hello.png");
    std::fs::write(&png, include_bytes!("fixtures/tiny.png")).unwrap();

    let artifacts = multimodal_drop_impl("drop", vec![png], 1).await.unwrap();

    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].mime, "image/png");
    assert_eq!(artifacts[0].sha256.len(), 64);
}

#[tokio::test]
async fn multimodal_drop_more_than_10_files_rejects() {
    let tmp = TempDir::new().unwrap();
    let _g = MurHomeGuard::set(tmp.path());
    seed(tmp.path(), "many");
    let paths: Vec<PathBuf> = (0..11)
        .map(|i| {
            let p = tmp.path().join(format!("f{i}.png"));
            std::fs::write(&p, include_bytes!("fixtures/tiny.png")).unwrap();
            p
        })
        .collect();

    let err = multimodal_drop_impl("many", paths, 1)
        .await
        .expect_err("too many files");
    assert!(
        err.to_string().contains("max 10"),
        "unexpected error: {err}"
    );
}

#[tokio::test]
async fn multimodal_drop_size_cap_rejects() {
    let tmp = TempDir::new().unwrap();
    let _g = MurHomeGuard::set(tmp.path());
    seed(tmp.path(), "huge");
    // Two 16 MB files → 32 MB total > 30 MB cap.
    let buf = vec![0u8; 16 * 1024 * 1024];
    let p1 = tmp.path().join("a.bin");
    let p2 = tmp.path().join("b.bin");
    std::fs::write(&p1, &buf).unwrap();
    std::fs::write(&p2, &buf).unwrap();

    let err = multimodal_drop_impl("huge", vec![p1, p2], 1)
        .await
        .expect_err("> 30 MB");
    let msg = err.to_string();
    assert!(
        msg.contains("30 MB") || msg.contains("> 30"),
        "unexpected error: {msg}"
    );
}
