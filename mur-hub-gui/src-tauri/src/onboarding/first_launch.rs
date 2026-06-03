//! First-launch onboarding checks for the Hub.
//!
//! On macOS: verifies the app is running from /Applications, registers the
//! .muragent handler via lsregister, and tracks completion via a marker file.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

const MARKER: &str = ".hub_onboarded";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FirstLaunchStatus {
    /// True if this is the first launch (marker not yet written).
    pub is_first_launch: bool,
    /// True if running from /Applications (or non-macOS where this is N/A).
    pub in_applications: bool,
}

/// Returns first-launch status and, on macOS first launch, registers the
/// .muragent handler with LaunchServices.
#[tauri::command]
pub fn check_first_launch() -> FirstLaunchStatus {
    let mur_home = mur_home_path();
    let is_first_launch = !mur_home.join(MARKER).exists();
    let in_applications = check_in_applications();

    if is_first_launch {
        #[cfg(target_os = "macos")]
        run_lsregister();
    }

    FirstLaunchStatus {
        is_first_launch,
        in_applications,
    }
}

/// Write the marker file so future launches are not treated as first-launch.
#[tauri::command]
pub fn mark_first_launch_done() {
    let mur_home = mur_home_path();
    let _ = std::fs::create_dir_all(&mur_home);
    let _ = std::fs::write(mur_home.join(MARKER), "");
}

// ─── Helpers ──────────────────────────────────────────────────────────────

fn check_in_applications() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| {
                p.starts_with("/Applications/")
                    || p.to_str()
                        .map(|s| s.contains("/Applications/"))
                        .unwrap_or(false)
            })
            .unwrap_or(true) // assume fine if we can't tell
    }
    #[cfg(not(target_os = "macos"))]
    {
        true
    }
}

#[cfg(target_os = "macos")]
fn bundle_path() -> Option<PathBuf> {
    // binary: /Applications/MUR Hub.app/Contents/MacOS/mur-hub-gui
    // bundle: /Applications/MUR Hub.app
    let exe = std::env::current_exe().ok()?;
    let bundle = exe.parent()?.parent()?.parent()?;
    if bundle.extension().and_then(|e| e.to_str()) == Some("app") {
        Some(bundle.to_path_buf())
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn run_lsregister() {
    const LSREG: &str = "/System/Library/Frameworks/CoreServices.framework/\
        Frameworks/LaunchServices.framework/Support/lsregister";
    let Some(bundle) = bundle_path() else { return };
    let _ = std::process::Command::new(LSREG)
        .args(["-f", bundle.to_str().unwrap_or("")])
        .status();
}

fn mur_home_path() -> PathBuf {
    std::env::var("MUR_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".mur")
        })
}

pub fn marker_path(mur_home: &Path) -> PathBuf {
    mur_home.join(MARKER)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn first_launch_writes_and_reads_marker() {
        let tmp = TempDir::new().unwrap();
        assert!(!tmp.path().join(MARKER).exists());
        std::fs::write(marker_path(tmp.path()), "").unwrap();
        assert!(tmp.path().join(MARKER).exists());
    }

    #[test]
    fn in_applications_returns_bool() {
        // Just verify it doesn't panic.
        let _ = check_in_applications();
    }
}
