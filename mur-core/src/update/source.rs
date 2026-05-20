//! Detect how `mur` was installed.

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
            InstallSource::Cargo => {
                Some("Installed via cargo. Run: cargo install mur --force")
            }
            InstallSource::Other => None,
        }
    }
}

/// Detect by querying system package managers. Lives behind a small layer so
/// tests can inject fake command outputs via [`detect_from_outputs`].
pub fn detect() -> InstallSource {
    let brew = Command::new("brew").args(["list", "mur"]).output().ok();
    let cargo = Command::new("cargo").args(["install", "--list"]).output().ok();

    detect_from_outputs(
        brew.as_ref().map(|o| (o.status.success(), o.stdout.as_slice())),
        cargo.as_ref().map(|o| (o.status.success(), o.stdout.as_slice())),
    )
}

pub fn detect_from_outputs(
    brew: Option<(bool, &[u8])>,
    cargo: Option<(bool, &[u8])>,
) -> InstallSource {
    if let Some((true, _)) = brew {
        return InstallSource::Homebrew;
    }
    if let Some((true, stdout)) = cargo {
        let s = std::str::from_utf8(stdout).unwrap_or("");
        if s.lines().any(|l| l.starts_with("mur ") || l.starts_with("mur-core ")) {
            return InstallSource::Cargo;
        }
    }
    InstallSource::Other
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn brew_success_wins() {
        let s = detect_from_outputs(Some((true, b"mur")), Some((true, b"mur v2.16.0:\n")));
        assert_eq!(s, InstallSource::Homebrew);
    }

    #[test]
    fn cargo_when_brew_absent_or_failed() {
        let s = detect_from_outputs(Some((false, b"")), Some((true, b"mur v2.16.0:\n")));
        assert_eq!(s, InstallSource::Cargo);
        let s = detect_from_outputs(None, Some((true, b"mur v2.16.0:\n")));
        assert_eq!(s, InstallSource::Cargo);
    }

    #[test]
    fn cargo_list_must_mention_mur() {
        let s = detect_from_outputs(None, Some((true, b"ripgrep v14.0.0:\n")));
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn other_when_both_missing() {
        let s = detect_from_outputs(None, None);
        assert_eq!(s, InstallSource::Other);
    }

    #[test]
    fn hints_are_shaped() {
        assert!(InstallSource::Homebrew.upgrade_hint().unwrap().contains("brew upgrade"));
        assert!(InstallSource::Cargo.upgrade_hint().unwrap().contains("cargo install"));
        assert!(InstallSource::Other.upgrade_hint().is_none());
    }
}
