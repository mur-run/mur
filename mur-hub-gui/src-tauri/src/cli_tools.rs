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

/// The PATH `mur` the user would invoke in a terminal.
///
/// A GUI app does not inherit the shell PATH, so this used to probe two known
/// install dirs and take the first that existed — Homebrew first. That is not
/// what a terminal does. With a stale `/opt/homebrew/bin/mur` beside a current
/// `~/.local/bin/mur` earlier on PATH, the Hub read the copy the user never
/// runs and reported a version skew that did not exist, telling them to
/// `brew upgrade mur` when their actual CLI was already newer than the Hub.
///
/// So ask the user's shell, which is the only thing that knows the answer.
/// Falling back to the old probe keeps this working where no shell is
/// configured, and
/// `install_cli_tools` still writes to `install_dir`'s choice — that decision
/// is about where to put a binary, not about which one is in front.
fn path_mur(home: &Path) -> Option<PathBuf> {
    #[cfg(unix)]
    if let Some(p) = shell_which("mur") {
        return Some(p);
    }
    [
        PathBuf::from("/opt/homebrew/bin/mur"),
        home.join(".local/bin/mur"),
    ]
    .into_iter()
    .find(|p| p.exists())
}

/// `$SHELL -ilc 'command -v <name>'`, the resolution a terminal actually performs.
///
/// Both flags are load-bearing and `-l` alone is not enough: on the machine
/// this was reported from, PATH is exported at `.zshrc:74`, which only an
/// INTERACTIVE shell reads — a login shell resolved to the stale Homebrew copy,
/// exactly like the probe it replaced. `-i` covers `.zshrc`/`.bashrc`, `-l`
/// covers `.zprofile`/`.profile`, and people put it in either.
///
/// `None` on any failure so the caller falls back rather than reporting
/// nothing. Unix only: `$SHELL` and `-ilc` are POSIX conventions, and Windows
/// has no equivalent question to ask — the probe list stands there.
#[cfg(unix)]
pub(crate) fn shell_which(name: &str) -> Option<PathBuf> {
    let shell = std::env::var("SHELL").ok()?;
    let out = Command::new(shell)
        .args(["-ilc", &format!("command -v {name}")])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let p = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim());
    p.is_file().then_some(p)
}

/// The `mur` the user's shell would run, for callers that need to invoke it
/// rather than just report on it.
pub fn resolve_mur() -> Option<PathBuf> {
    path_mur(&dirs::home_dir()?)
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
    pub upgrade_hint: String,
}

/// Resolve the real path of `p` (following one symlink level), returning the
/// canonicalized path or the original if resolution fails.
fn resolve_symlink(p: &Path) -> PathBuf {
    std::fs::read_link(p).unwrap_or_else(|_| p.to_path_buf())
}

/// Derive the appropriate upgrade command by inspecting where the binary lives.
fn upgrade_hint_for(mur: &Path) -> String {
    let real = resolve_symlink(mur);
    let s = real.to_string_lossy();
    if s.contains("/Cellar/") || s.contains("/homebrew/") && s.contains("/bin/mur") {
        "brew upgrade mur".to_string()
    } else if s.contains("/.cargo/") {
        "cargo install mur --force".to_string()
    } else {
        "mur update".to_string()
    }
}

/// Report a version skew only when the PATH `mur` is OLDER than this Hub.
/// Returns None when in sync, newer, missing, or a Hub-managed symlink.
#[tauri::command]
pub fn cli_version_skew(app: tauri::AppHandle) -> Option<CliSkew> {
    let home = dirs::home_dir()?;
    let mur_path = path_mur(&home)?;
    let cli = read_cli_version(&mur_path)?;
    let hub = app.package_info().version.to_string();
    is_older(&cli, &hub).then_some(CliSkew {
        upgrade_hint: upgrade_hint_for(&mur_path),
        cli,
        hub,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bug: a stale Homebrew copy beside a current `~/.local/bin` one
    /// earlier on PATH. The shell resolves to the current one; the old probe
    /// took Homebrew because it was listed first, and the Hub reported a skew
    /// that did not exist — telling the user to `brew upgrade mur` when their
    /// CLI was already newer than the Hub.
    #[cfg(unix)]
    #[test]
    fn the_shell_decides_which_mur_not_the_probe_order() {
        let dir = tempfile::tempdir().unwrap();
        let brew = dir.path().join("brew-bin");
        let local = dir.path().join("local-bin");
        std::fs::create_dir_all(&brew).unwrap();
        std::fs::create_dir_all(&local).unwrap();
        for d in [&brew, &local] {
            let f = d.join("mur");
            std::fs::write(&f, "#!/bin/sh\necho mur 0.0.0\n").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        // `local` first, exactly like the reporting user's PATH.
        let path = format!("{}:{}", local.display(), brew.display());
        let out = Command::new("/bin/sh")
            .env("PATH", &path)
            .args(["-c", "command -v mur"])
            .output()
            .unwrap();
        let resolved = PathBuf::from(String::from_utf8_lossy(&out.stdout).trim().to_string());
        assert_eq!(
            resolved,
            local.join("mur"),
            "PATH order decides; the probe's hardcoded Homebrew-first does not"
        );
    }

    /// `-i` is load-bearing, not decoration. On the machine this was reported
    /// from, PATH is exported at `.zshrc:74`, which a login shell never reads:
    /// `-lc` resolved to the stale Homebrew copy and `-ic` to the current one.
    /// A future tidy-up that drops `-i` would silently restore the bug.
    #[cfg(unix)]
    #[test]
    fn a_path_set_in_an_interactive_rc_is_invisible_without_i() {
        let home = tempfile::tempdir().unwrap();
        let bin = home.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join("marker"), "").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(bin.join("marker"), std::fs::Permissions::from_mode(0o755))
                .unwrap();
        }
        // The PATH addition lives ONLY in the interactive rc, exactly like the
        // `.zshrc:74` that caused this.
        std::fs::write(
            home.path().join(".bashrc"),
            format!("export PATH=\"{}:$PATH\"\n", bin.display()),
        )
        .unwrap();

        let found = |flags: &str| {
            Command::new("/bin/bash")
                .env("HOME", home.path())
                .args([flags, "command -v marker"])
                .output()
                .map(|o| !String::from_utf8_lossy(&o.stdout).trim().is_empty())
                .unwrap_or(false)
        };
        assert!(!found("-c"), "a non-interactive shell must not see .bashrc");
        assert!(
            found("-ic"),
            "an interactive shell must see it — this is why the flag is there"
        );
    }

    /// `read_cli_version` must read the binary it was handed, so the two copies
    /// are distinguishable at all.
    ///
    /// Unix-only for the fixture, not the behaviour: it writes a `#!/bin/sh`
    /// script and runs it, which Windows cannot execute. Gating the two tests
    /// that SPAWN a shell was not enough — this one merely executes a script
    /// file, the same dependency wearing a different shape.
    #[cfg(unix)]
    #[test]
    fn the_version_comes_from_the_binary_given() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("mur");
        std::fs::write(&f, "#!/bin/sh\necho mur 9.9.9\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert_eq!(read_cli_version(&f).as_deref(), Some("9.9.9"));
    }

    /// Control on the direction: equal versions are not a skew, and the banner
    /// only fires when the CLI is genuinely behind.
    #[test]
    fn equal_versions_are_not_a_skew() {
        assert!(!is_older("2.71.7", "2.71.7"));
        assert!(is_older("2.71.3", "2.71.7"));
        assert!(!is_older("2.71.7", "2.71.3"));
    }

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
    fn upgrade_hint_by_install_source() {
        // A binary under the Homebrew prefix is brew-managed even when the
        // symlink can't be resolved (this test path doesn't exist on disk).
        assert_eq!(
            upgrade_hint_for(Path::new("/opt/homebrew/bin/mur")),
            "brew upgrade mur"
        );
        // Homebrew Cellar path (what the symlink resolves to)
        assert!(
            upgrade_hint_for(Path::new("/opt/homebrew/Cellar/mur/2.34.0/bin/mur"))
                .contains("brew upgrade mur")
        );
        // Cargo
        assert!(upgrade_hint_for(Path::new("/Users/x/.cargo/bin/mur")).contains("cargo install"));
        // Manual / unknown
        assert_eq!(
            upgrade_hint_for(Path::new("/usr/local/bin/mur")),
            "mur update"
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
