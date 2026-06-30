//! "Install command-line tools" — symlink the bundled `mur` into a PATH dir,
//! plus a passive version-skew check (nudge only; the Hub never auto-upgrades
//! the CLI — that would clobber a brew-managed binary with a symlink).

use std::path::{Path, PathBuf};
use std::process::Command;

/// Preferred PATH install dir: /opt/homebrew/bin if writable, else ~/.local/bin.
pub fn install_dir(homebrew_writable: bool, home: &Path) -> PathBuf {
    if homebrew_writable {
        PathBuf::from("/opt/homebrew/bin")
    } else {
        home.join(".local/bin")
    }
}

/// Bundled `mur` path given the Hub executable path (sibling in Contents/MacOS).
pub fn bundled_mur_path(hub_exe: &Path) -> Option<PathBuf> {
    Some(hub_exe.parent()?.join("mur"))
}

/// Symlink the bundled `mur` into a PATH dir. Returns the install path on
/// success. Surfaced to the UI and the tray menu.
#[tauri::command]
pub fn install_cli_tools() -> Result<String, String> {
    let hub_exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let src = bundled_mur_path(&hub_exe).ok_or("cannot locate bundled mur")?;
    if !src.exists() {
        return Err(format!("bundled mur not found at {}", src.display()));
    }
    let home = dirs::home_dir().ok_or("no home dir")?;
    let homebrew = Path::new("/opt/homebrew/bin");
    let writable = homebrew.exists()
        && std::fs::metadata(homebrew)
            .map(|m| !m.permissions().readonly())
            .unwrap_or(false);
    let dir = install_dir(writable, &home);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let dst = dir.join("mur");
    let _ = std::fs::remove_file(&dst);
    #[cfg(unix)]
    std::os::unix::fs::symlink(&src, &dst).map_err(|e| e.to_string())?;
    Ok(dst.display().to_string())
}

/// The PATH `mur` the user would invoke in a terminal. GUI apps don't inherit
/// the shell PATH, so we probe the canonical install locations directly (the
/// same two `install_cli_tools` targets, which also cover brew + build.sh).
fn path_mur(home: &Path) -> Option<PathBuf> {
    [
        PathBuf::from("/opt/homebrew/bin/mur"),
        home.join(".local/bin/mur"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// `mur --version` → "mur X.Y.Z" → "X.Y.Z".
fn read_cli_version(mur: &Path) -> Option<String> {
    let out = Command::new(mur).arg("--version").output().ok()?;
    let s = String::from_utf8_lossy(&out.stdout);
    s.split_whitespace().nth(1).map(str::to_string)
}

/// True when semver `a` is strictly older than `b`. Naive numeric X.Y.Z
/// compare — MUR versions have no pre-release tags.
// ponytail: numeric tuple compare, swap in a semver crate only if tags appear.
fn is_older(a: &str, b: &str) -> bool {
    fn parts(v: &str) -> Vec<u64> {
        v.split('.').map(|n| n.parse().unwrap_or(0)).collect()
    }
    parts(a) < parts(b)
}

#[derive(serde::Serialize)]
pub struct CliSkew {
    pub cli: String,
    pub hub: String,
}

/// Report a version skew only when the PATH `mur` is OLDER than this Hub, so
/// the UI can nudge the user to `brew upgrade mur`. Returns None when in sync,
/// newer, missing, or a Hub-managed symlink (which reports the bundled version
/// == the Hub version and so never skews). Read-only: never touches the CLI.
#[tauri::command]
pub fn cli_version_skew(app: tauri::AppHandle) -> Option<CliSkew> {
    let home = dirs::home_dir()?;
    let cli = read_cli_version(&path_mur(&home)?)?;
    let hub = app.package_info().version.to_string();
    is_older(&cli, &hub).then_some(CliSkew { cli, hub })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefers_homebrew_when_writable() {
        assert_eq!(
            install_dir(true, Path::new("/Users/x")),
            PathBuf::from("/opt/homebrew/bin")
        );
    }

    #[test]
    fn falls_back_to_local_bin() {
        assert_eq!(
            install_dir(false, Path::new("/Users/x")),
            PathBuf::from("/Users/x/.local/bin")
        );
    }

    #[test]
    fn bundled_mur_is_sibling_of_hub() {
        let exe = Path::new("/Applications/MUR Hub.app/Contents/MacOS/mur-hub-gui");
        assert_eq!(
            bundled_mur_path(exe).unwrap(),
            PathBuf::from("/Applications/MUR Hub.app/Contents/MacOS/mur")
        );
    }

    #[test]
    fn skew_only_when_cli_is_older() {
        assert!(is_older("2.31.1", "2.32.0")); // CLI lagging → nudge
        assert!(is_older("2.31.1", "2.31.2"));
        assert!(!is_older("2.32.0", "2.32.0")); // in sync → no nudge
        assert!(!is_older("2.33.0", "2.32.0")); // CLI ahead → no nudge
    }
}
