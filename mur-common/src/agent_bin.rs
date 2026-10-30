//! Where per-agent `mur_agent_<name>` symlinks live.
//!
//! Shared by `mur-core` (which creates the links) and `mur-gui-core` (which
//! must recognise a runtime launched through one), so the two can't drift.

use std::path::PathBuf;

/// Env override for the per-agent symlink directory.
pub const BIN_DIR_ENV: &str = "MUR_AGENT_BIN_DIR";

/// The directory holding the `mur_agent_<name>` symlinks: `$MUR_AGENT_BIN_DIR`
/// when set, otherwise `~/.local/bin`. `None` when neither resolves.
pub fn bin_dir() -> Option<PathBuf> {
    if let Some(v) = std::env::var_os(BIN_DIR_ENV) {
        return Some(PathBuf::from(v));
    }
    dirs::home_dir().map(|home| home.join(".local/bin"))
}

/// The symlink name for agent `slug`.
pub fn link_name(slug: &str) -> String {
    format!("mur_agent_{slug}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_env::EnvGuard;

    #[test]
    fn env_override_wins() {
        let _g = EnvGuard::set([(BIN_DIR_ENV, "/x/bin")]);
        assert_eq!(bin_dir(), Some(PathBuf::from("/x/bin")));
    }

    #[test]
    fn link_name_prefixes_the_slug() {
        assert_eq!(link_name("mur"), "mur_agent_mur");
    }
}
