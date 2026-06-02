//! "Install command-line tools" — symlink the bundled `mur` into a PATH dir.

use std::path::{Path, PathBuf};

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
        let exe = Path::new("/Applications/MuR Hub.app/Contents/MacOS/mur-hub-gui");
        assert_eq!(
            bundled_mur_path(exe).unwrap(),
            PathBuf::from("/Applications/MuR Hub.app/Contents/MacOS/mur")
        );
    }
}
