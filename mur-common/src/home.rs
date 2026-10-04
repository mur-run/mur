//! The one resolver for MUR's data root (`~/.mur`, or `$MUR_HOME`).
//!
//! Every surface — CLI, daemon, agent runtime, MCP server — must agree on
//! where MUR's state lives. When call sites built `home_dir().join(".mur")`
//! by hand, setting `MUR_HOME` moved config and agents but left patterns,
//! indexes and the daemon behind in `~/.mur`: a silently split store (#1696).
//! Resolve the root here; never join `.mur` onto a home directory elsewhere.
//!
//! Semantics: a non-empty `MUR_HOME` wins; an empty one counts as unset;
//! otherwise `<home>/.mur`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// Environment variable that relocates MUR's data root.
pub const MUR_HOME_ENV: &str = "MUR_HOME";

/// Name of the data directory under the user's home when `MUR_HOME` is unset.
pub const MUR_DIR_NAME: &str = ".mur";

/// Pure resolution rule, separated from the process environment so it can be
/// tested without mutating env vars.
pub fn resolve_from(env: Option<OsString>, home: Option<&Path>) -> Option<PathBuf> {
    if let Some(v) = env
        && !v.is_empty()
    {
        return Some(PathBuf::from(v));
    }
    home.map(|h| h.join(MUR_DIR_NAME))
}

/// The `MUR_HOME` override in effect, if any (non-empty). Service installers
/// (launchd / systemd) bake this into the unit: a daemon started by the OS
/// does not inherit the installing shell's environment, so without it the
/// daemon would fall back to `~/.mur` while the CLI uses `$MUR_HOME`.
pub fn env_override() -> Option<PathBuf> {
    std::env::var_os(MUR_HOME_ENV)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// MUR's data root, or `None` when `MUR_HOME` is unset and there is no home
/// directory.
pub fn try_mur_home() -> Option<PathBuf> {
    resolve_from(std::env::var_os(MUR_HOME_ENV), dirs::home_dir().as_deref())
}

/// MUR's data root, as an error when it cannot be resolved.
pub fn mur_home_or_err() -> anyhow::Result<PathBuf> {
    try_mur_home().ok_or_else(|| {
        anyhow::anyhow!("cannot resolve MUR home: {MUR_HOME_ENV} unset and no home directory")
    })
}

/// MUR's data root for callers that must not fail. When nothing resolves
/// (no `MUR_HOME`, no home directory) this is a relative `.mur` — the same
/// degenerate result the old `home_dir().unwrap_or_default().join(".mur")`
/// call sites produced.
pub fn mur_home_lossy() -> PathBuf {
    try_mur_home().unwrap_or_else(|| PathBuf::from(MUR_DIR_NAME))
}

/// MUR's data root. Panics when it cannot be resolved; prefer
/// [`mur_home_or_err`] on paths that can report an error.
pub fn mur_home() -> PathBuf {
    try_mur_home().expect("cannot resolve MUR home: MUR_HOME unset and no home directory")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_wins_over_home() {
        let got = resolve_from(Some("/x/mur".into()), Some(Path::new("/home/u")));
        assert_eq!(got, Some(PathBuf::from("/x/mur")));
    }

    #[test]
    fn empty_env_counts_as_unset() {
        let got = resolve_from(Some(OsString::new()), Some(Path::new("/home/u")));
        assert_eq!(got, Some(PathBuf::from("/home/u").join(MUR_DIR_NAME)));
    }

    #[test]
    fn falls_back_to_home_dot_mur() {
        let got = resolve_from(None, Some(Path::new("/home/u")));
        assert_eq!(got, Some(PathBuf::from("/home/u/.mur")));
    }

    #[test]
    fn none_without_env_or_home() {
        assert_eq!(resolve_from(None, None), None);
    }
}
