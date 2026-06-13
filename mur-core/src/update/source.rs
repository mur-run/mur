//! Detect how `mur` was installed.

use std::path::Path;
use std::process::Command;

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum InstallSource {
    Homebrew,
    Cargo,
    Other,
}

impl InstallSource {
    /// Human-readable upgrade instruction, or `None` if self-update should run.
    pub fn upgrade_hint(self) -> Option<&'static str> {
        match self {
            InstallSource::Homebrew => Some("Installed via Homebrew. Run: brew upgrade mur"),
            InstallSource::Cargo => Some("Installed via cargo. Run: cargo install mur --force"),
            InstallSource::Other => None,
        }
    }
}

/// Detect by querying system package managers. Lives behind a small layer so
/// tests can inject fake command outputs via [`detect_from_outputs`].
pub fn detect() -> InstallSource {
    // The install method of the *running* binary is determined by where that
    // binary actually lives — not by whether `brew`/`cargo` happen to also have
    // a `mur` installed. (A manually-installed binary can shadow a brew one on
    // PATH; `brew list mur` would still succeed and falsely advise `brew upgrade`,
    // which could downgrade to the older Cellar version.)
    let exe = std::env::current_exe()
        .ok()
        .and_then(|p| std::fs::canonicalize(&p).ok().or(Some(p)));

    let brew = Command::new("brew").args(["list", "mur"]).output().ok();
    let cargo = Command::new("cargo")
        .args(["install", "--list"])
        .output()
        .ok();

    detect_from_outputs(
        exe.as_deref(),
        brew.as_ref()
            .map(|o| (o.status.success(), o.stdout.as_slice())),
        cargo
            .as_ref()
            .map(|o| (o.status.success(), o.stdout.as_slice())),
    )
}

pub fn detect_from_outputs(
    exe: Option<&Path>,
    brew: Option<(bool, &[u8])>,
    cargo: Option<(bool, &[u8])>,
) -> InstallSource {
    // Primary signal: the canonical path of the running executable.
    if let Some(p) = exe {
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

    // Fallback only when the running exe path is unavailable: package-manager queries.
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
        );
        assert_eq!(s, InstallSource::Homebrew);
    }

    #[test]
    fn exe_in_cargo_bin_is_cargo() {
        let s = detect_from_outputs(
            Some(Path::new("/Users/x/.cargo/bin/mur")),
            None,
            Some((true, b"mur v2.16.0:\n")),
        );
        assert_eq!(s, InstallSource::Cargo);
    }

    #[test]
    fn manual_binary_shadowing_brew_is_other_not_homebrew() {
        // Regression: a manually-installed binary at /opt/homebrew/bin/mur (a real
        // file, NOT a Cellar symlink) must NOT be reported as Homebrew just because
        // `brew list mur` succeeds — else `mur update --check` advises a downgrade.
        let s = detect_from_outputs(
            Some(Path::new("/opt/homebrew/bin/mur")),
            Some((true, b"mur")),
            None,
        );
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn fallback_brew_success_wins_when_exe_unknown() {
        let s = detect_from_outputs(None, Some((true, b"mur")), Some((true, b"mur v2.16.0:\n")));
        assert_eq!(s, InstallSource::Homebrew);
    }

    #[test]
    fn fallback_cargo_when_brew_absent_and_exe_unknown() {
        let s = detect_from_outputs(None, Some((false, b"")), Some((true, b"mur v2.16.0:\n")));
        assert_eq!(s, InstallSource::Cargo);
    }

    #[test]
    fn fallback_cargo_list_must_mention_mur() {
        let s = detect_from_outputs(None, None, Some((true, b"ripgrep v14.0.0:\n")));
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn other_when_all_missing() {
        let s = detect_from_outputs(None, None, None);
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn hints_are_shaped() {
        assert!(
            InstallSource::Homebrew
                .upgrade_hint()
                .unwrap()
                .contains("brew upgrade")
        );
        assert!(
            InstallSource::Cargo
                .upgrade_hint()
                .unwrap()
                .contains("cargo install")
        );
        assert!(InstallSource::Other.upgrade_hint().is_none());
    }
}
