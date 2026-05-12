//! MuR Hub — Tauri 2 desktop app.
//!
//! M-h0: boots a single empty dashboard window. Real popover, multi-agent
//! discovery, and pet windows arrive in later milestones (see
//! `docs/superpowers/specs/2026-05-11-mur-hub-companion-design.md`).

use tracing_subscriber::EnvFilter;

pub fn run() {
    init_tracing();
    tracing::info!(version = mur_gui_core::CRATE_VERSION, "starting mur-hub-gui");

    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

fn init_tracing() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .try_init();
}

#[cfg(test)]
mod tests {
    #[test]
    fn lib_links() {
        // Smoke test: the crate compiles and the lib symbol exists.
        // (We cannot actually run tauri::Builder in a unit test without a
        //  windowing context — that lives in a later integration test.)
        let _ = super::init_tracing;
    }
}
