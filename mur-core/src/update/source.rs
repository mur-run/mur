//! Detect how `mur` was installed.

use std::path::Path;
use std::process::Command;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InstallSource {
    Homebrew,
    Cargo,
    Pkg,
    Other,
}

impl InstallSource {
    /// Human-readable upgrade instruction, or `None` if self-update should run.
    pub fn upgrade_hint(self) -> Option<&'static str> {
        match self {
            InstallSource::Homebrew => Some("Installed via Homebrew. Run: brew upgrade mur"),
            InstallSource::Cargo => Some("Installed via cargo. Run: cargo install mur --force"),
            InstallSource::Pkg => Some("Installed via FreeBSD pkg. Run: pkg upgrade mur"),
            InstallSource::Other => None,
        }
    }
}

/// Detect by querying system package managers. Lives behind a small layer so
/// tests can inject fake command outputs via [`detect_from_outputs`].
pub fn detect() -> InstallSource {
    // The install method of the *running* binary is determined by where that
    // binary actually lives — not by whether a package manager happens to also
    // have another `mur` installed.
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));

    let brew = Command::new("brew").args(["list", "mur"]).output().ok();
    let cargo = Command::new("cargo")
        .args(["install", "--list"])
        .output()
        .ok();

    // Unlike the PATH-presence checks above, this asks pkg whether the exact
    // canonical executable belongs to an installed package.
    #[cfg(target_os = "freebsd")]
    let pkg_owner = exe.as_deref().and_then(|path| {
        Command::new("pkg")
            .arg("which")
            .arg("-q")
            .arg(path)
            .output()
            .ok()
            .map(|o| (o.status.success(), o.stdout))
    });
    #[cfg(not(target_os = "freebsd"))]
    let pkg_owner: Option<(bool, Vec<u8>)> = None;

    detect_from_outputs(
        exe.as_deref(),
        brew.as_ref()
            .map(|o| (o.status.success(), o.stdout.as_slice())),
        cargo
            .as_ref()
            .map(|o| (o.status.success(), o.stdout.as_slice())),
        pkg_owner
            .as_ref()
            .map(|(success, stdout)| (*success, stdout.as_slice())),
    )
}

pub fn detect_from_outputs(
    exe: Option<&Path>,
    brew: Option<(bool, &[u8])>,
    cargo: Option<(bool, &[u8])>,
    pkg_owner: Option<(bool, &[u8])>,
) -> InstallSource {
    // Primary signal: the canonical path and ownership of the running executable.
    if let Some(p) = exe {
        if matches!(pkg_owner, Some((true, _))) {
            return InstallSource::Pkg;
        }
        let s = p.to_string_lossy();
        // Homebrew (incl. Linuxbrew) always resolves binaries under a Cellar.
        if s.contains("/Cellar/") {
            return InstallSource::Homebrew;
        }
        // `cargo install` places binaries under <CARGO_HOME>/bin.
        if s.contains("/.cargo/bin/") {
            return InstallSource::Cargo;
        }
        // A real binary in any other location is a manual / self-managed install.
        return InstallSource::Other;
    }

    // Fallback only when the running exe path is unavailable. pkg ownership is
    // intentionally excluded because there is then no current executable to query.
    if let Some((true, _)) = brew {
        return InstallSource::Homebrew;
    }
    if let Some((true, stdout)) = cargo {
        let s = std::str::from_utf8(stdout).unwrap_or("");
        if s.lines()
            .any(|l| l.starts_with("mur ") || l.starts_with("mur-core "))
        {
            return InstallSource::Cargo;
        }
    }
    InstallSource::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exe_in_cellar_is_homebrew() {
        let s = detect_from_outputs(
            Some(Path::new("/opt/homebrew/Cellar/mur/2.24.0/bin/mur")),
            Some((true, b"mur")),
            None,
            None,
        );
        assert_eq!(s, InstallSource::Homebrew);
    }

    #[test]
    fn exe_in_cargo_bin_is_cargo() {
        let s = detect_from_outputs(
            Some(Path::new("/Users/x/.cargo/bin/mur")),
            None,
            Some((true, b"mur v2.16.0:\n")),
            None,
        );
        assert_eq!(s, InstallSource::Cargo);
    }

    #[test]
    fn pkg_owned_freebsd_binary_is_pkg() {
        let s = detect_from_outputs(
            Some(Path::new("/usr/local/bin/mur")),
            None,
            None,
            Some((true, b"mur-2.24.0")),
        );
        assert_eq!(s, InstallSource::Pkg);
    }

    #[test]
    fn unowned_freebsd_binary_is_other() {
        let s = detect_from_outputs(
            Some(Path::new("/usr/local/bin/mur")),
            None,
            None,
            Some((false, b"")),
        );
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn non_freebsd_fixtures_do_not_become_pkg() {
        for path in [
            "/opt/homebrew/bin/mur",
            "/usr/local/bin/mur-linux",
            r"C:\Program Files\MUR\mur.exe",
        ] {
            assert_ne!(
                detect_from_outputs(Some(Path::new(path)), None, None, None),
                InstallSource::Pkg
            );
        }
    }

    #[test]
    fn pkg_without_current_executable_is_not_enough() {
        assert_eq!(
            detect_from_outputs(None, None, None, Some((true, b"mur-2.24.0"))),
            InstallSource::Other
        );
    }

    #[test]
    fn manual_binary_shadowing_brew_is_other_not_homebrew() {
        let s = detect_from_outputs(
            Some(Path::new("/opt/homebrew/bin/mur")),
            Some((true, b"mur")),
            None,
            None,
        );
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn fallback_brew_success_wins_when_exe_unknown() {
        let s = detect_from_outputs(
            None,
            Some((true, b"mur")),
            Some((true, b"mur v2.16.0:\n")),
            None,
        );
        assert_eq!(s, InstallSource::Homebrew);
    }

    #[test]
    fn fallback_cargo_when_brew_absent_and_exe_unknown() {
        let s = detect_from_outputs(
            None,
            Some((false, b"")),
            Some((true, b"mur v2.16.0:\n")),
            None,
        );
        assert_eq!(s, InstallSource::Cargo);
    }

    #[test]
    fn fallback_cargo_list_must_mention_mur() {
        let s = detect_from_outputs(None, None, Some((true, b"ripgrep v14.0.0:\n")), None);
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn other_when_all_missing() {
        let s = detect_from_outputs(None, None, None, None);
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn hints_are_exact() {
        assert_eq!(
            InstallSource::Homebrew.upgrade_hint(),
            Some("Installed via Homebrew. Run: brew upgrade mur")
        );
        assert_eq!(
            InstallSource::Cargo.upgrade_hint(),
            Some("Installed via cargo. Run: cargo install mur --force")
        );
        assert_eq!(
            InstallSource::Pkg.upgrade_hint(),
            Some("Installed via FreeBSD pkg. Run: pkg upgrade mur")
        );
        assert!(InstallSource::Other.upgrade_hint().is_none());
    }
}
