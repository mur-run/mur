//! Local-runtime detection for `mur init`'s model-setup flow.
//!
//! Detects which local LLM runtimes are present on the host so
//! `crate::discovery` can probe the right endpoints:
//!
//!   oMLX.app (Apple Silicon)  >  Ollama

use std::path::Path;

#[derive(Debug, Clone, Copy)]
pub struct LocalRuntimes {
    pub ollama_running: bool,
    pub omlx_installed: bool,
}

pub fn detect_local_runtimes() -> LocalRuntimes {
    let apple_silicon = cfg!(target_os = "macos") && cfg!(target_arch = "aarch64");
    LocalRuntimes {
        ollama_running: ollama_running(),
        omlx_installed: apple_silicon && omlx_installed(),
    }
}

fn ollama_running() -> bool {
    std::process::Command::new("ollama")
        .arg("list")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// oMLX is a native macOS app (https://omlx.ai). Look for the bundle in
/// either the system or user Applications folder.
fn omlx_installed() -> bool {
    if Path::new("/Applications/oMLX.app").exists() {
        return true;
    }
    if let Some(home) = dirs::home_dir()
        && home.join("Applications/oMLX.app").exists()
    {
        return true;
    }
    false
}
