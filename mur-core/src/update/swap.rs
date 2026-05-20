//! Atomically replace the running `mur` binary with a newly downloaded one.

use std::path::{Path, PathBuf};

/// Locate the running executable on disk.
pub fn current_exe() -> anyhow::Result<PathBuf> {
    std::env::current_exe().map_err(|e| anyhow::anyhow!("cannot resolve current exe: {e}"))
}

/// Atomically replace `target` with `new_binary`. On Unix this is `rename(2)`,
/// which is atomic when both paths live on the same filesystem. On Windows the
/// caller must use [`spawn_windows_swap_helper`] instead because the running
/// `.exe` is locked.
#[cfg(unix)]
pub fn swap(new_binary: &Path, target: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(new_binary, std::fs::Permissions::from_mode(0o755))?;
    std::fs::rename(new_binary, target)?;
    Ok(())
}

#[cfg(windows)]
pub fn swap(_new_binary: &Path, _target: &Path) -> anyhow::Result<()> {
    anyhow::bail!("Windows must use spawn_windows_swap_helper instead of swap")
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn swap_replaces_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("mur");
        let newbin = dir.path().join("mur.new");
        std::fs::write(&target, b"OLD").unwrap();
        std::fs::write(&newbin, b"NEWBIN").unwrap();
        swap(&newbin, &target).unwrap();
        assert_eq!(std::fs::read(&target).unwrap(), b"NEWBIN");
        assert!(!newbin.exists());
    }
}

/// Generate the PowerShell helper script content used on Windows to replace a
/// locked .exe after this process exits.
#[cfg(any(windows, test))]
pub fn windows_helper_script(new_exe: &Path, target_exe: &Path, self_path: &Path) -> String {
    fn escape(p: &Path) -> String {
        p.display().to_string().replace('\'', "''")
    }
    format!(
        "Start-Sleep -Seconds 2\n\
         Move-Item -Force -LiteralPath '{new}' -Destination '{target}'\n\
         Remove-Item -LiteralPath '{self_}'\n",
        new = escape(new_exe),
        target = escape(target_exe),
        self_ = escape(self_path),
    )
}

#[cfg(windows)]
pub fn spawn_windows_swap_helper(new_exe: &Path, target_exe: &Path) -> anyhow::Result<()> {
    use std::process::Command;
    let helper = std::env::temp_dir().join(format!("mur-update-{}.ps1", std::process::id()));
    let script = windows_helper_script(new_exe, target_exe, &helper);
    std::fs::write(&helper, script)?;
    Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-WindowStyle",
            "Hidden",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
        ])
        .arg(&helper)
        .spawn()
        .map_err(|e| {
            anyhow::anyhow!(
                "On Windows, mur update requires PowerShell to complete the update: {e}"
            )
        })?;
    Ok(())
}

#[cfg(test)]
mod helper_tests {
    use super::*;

    #[test]
    fn script_quotes_paths_and_self_deletes() {
        let s = windows_helper_script(
            Path::new("C:\\Temp\\mur.new.exe"),
            Path::new("C:\\Program Files\\mur\\mur.exe"),
            Path::new("C:\\Temp\\helper.ps1"),
        );
        assert!(s.contains("Start-Sleep -Seconds 2"));
        assert!(s.contains("'C:\\Temp\\mur.new.exe'"));
        assert!(s.contains("'C:\\Program Files\\mur\\mur.exe'"));
        assert!(s.contains("Remove-Item -LiteralPath 'C:\\Temp\\helper.ps1'"));
    }

    #[test]
    fn script_escapes_single_quotes_in_paths() {
        let s = windows_helper_script(
            Path::new("C:\\Temp\\bob's\\mur.new.exe"),
            Path::new("C:\\Apps\\mur.exe"),
            Path::new("C:\\Temp\\h.ps1"),
        );
        assert!(s.contains("'C:\\Temp\\bob''s\\mur.new.exe'"));
    }
}
