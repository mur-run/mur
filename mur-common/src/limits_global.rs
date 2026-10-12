//! Where the GLOBAL `limits:` scope lives on disk, and the one loader every
//! reader goes through (issue #1794).
//!
//! The global scope used to be a block inside `<mur_home>/config.yaml`. That
//! file is granted to a sealed agent as a single FILE, and Landlock pins the
//! inode that existed at seal time: every atomic temp-file-and-rename edit
//! made after the seal was unreadable on Linux, so a running agent never saw
//! a new global limit. The scope now lives in its own file under a DIRECTORY
//! grant (`<mur_home>/limits/`), the same shape that keeps `profile.yaml`
//! readable under `agent_home`.
//!
//! Migration is read-side only: when the new file is absent the legacy
//! `config.yaml limits:` block is still honoured. Nothing here ever writes
//! `config.yaml` — the first `mur limits --global` write creates the new file
//! and the legacy block simply stops being read.

use std::path::{Path, PathBuf};

use super::Limits;

/// Directory under `mur_home` holding the global scope. Granted read-only to
/// sealed agents as a directory, so edits that replace the file stay visible.
pub const GLOBAL_LIMITS_DIR: &str = "limits";
/// The global scope's file inside [`GLOBAL_LIMITS_DIR`]: a bare `Limits`
/// mapping, no `limits:` wrapper.
pub const GLOBAL_LIMITS_FILE: &str = "global.yaml";
/// The user config that carried the global scope before #1794.
pub const LEGACY_CONFIG_FILE: &str = "config.yaml";

pub fn global_limits_dir(mur_home: &Path) -> PathBuf {
    mur_home.join(GLOBAL_LIMITS_DIR)
}

pub fn global_limits_path(mur_home: &Path) -> PathBuf {
    global_limits_dir(mur_home).join(GLOBAL_LIMITS_FILE)
}

pub fn legacy_config_path(mur_home: &Path) -> PathBuf {
    mur_home.join(LEGACY_CONFIG_FILE)
}

/// Which file the global scope was read from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlobalSource {
    /// `<mur_home>/limits/global.yaml`.
    File,
    /// No new file; `config.yaml limits:` (possibly empty) was used.
    Legacy,
}

impl GlobalSource {
    pub fn label(self) -> &'static str {
        match self {
            GlobalSource::File => "limits/global.yaml",
            GlobalSource::Legacy => "config.yaml limits (legacy)",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct GlobalLimits {
    pub limits: Limits,
    pub source: GlobalSource,
}

/// The new file alone. `Ok(None)` when it does not exist; an empty file is
/// an empty scope (it still shadows the legacy block). A file that is
/// present and does not parse is an error, never the defaults.
pub fn read_global_file(path: &Path) -> Result<Option<Limits>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Some(Limits::default()));
    }
    let parsed: Option<Limits> =
        serde_yaml_ng::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(Some(parsed.unwrap_or_default()))
}

/// The global scope for CLI readers. The new file is strict; the legacy
/// fallback keeps the old lenient `Config::load_or_default` behaviour, so a
/// `config.yaml` broken somewhere unrelated still reads as "no global
/// limits" exactly as before #1794.
pub fn load_global(mur_home: &Path) -> Result<GlobalLimits, String> {
    load_with(mur_home, |p| {
        Ok(crate::config::Config::load_or_default(p).limits)
    })
}

/// The global scope for a reader that already holds a good copy and must not
/// trade it for defaults (the agent runtime's hot reload): both files strict.
pub fn load_global_strict(mur_home: &Path) -> Result<GlobalLimits, String> {
    load_with(mur_home, |p| {
        crate::config::Config::try_load(p).map(|c| c.limits)
    })
}

fn load_with(
    mur_home: &Path,
    legacy: impl FnOnce(&Path) -> Result<Limits, String>,
) -> Result<GlobalLimits, String> {
    if let Some(limits) = read_global_file(&global_limits_path(mur_home))? {
        return Ok(GlobalLimits {
            limits,
            source: GlobalSource::File,
        });
    }
    Ok(GlobalLimits {
        limits: legacy(&legacy_config_path(mur_home))?,
        source: GlobalSource::Legacy,
    })
}

/// Whether `config.yaml` still carries a non-empty `limits:` block. Doctor
/// uses it to say the block is ignored once the new file exists; MUR never
/// removes it — that file is the user's.
pub fn legacy_block_present(mur_home: &Path) -> bool {
    !crate::config::Config::load_or_default(&legacy_config_path(mur_home))
        .limits
        .is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    fn write_new(h: &Path, text: &str) {
        std::fs::create_dir_all(global_limits_dir(h)).unwrap();
        std::fs::write(global_limits_path(h), text).unwrap();
    }

    fn write_legacy(h: &Path, text: &str) {
        std::fs::write(legacy_config_path(h), text).unwrap();
    }

    #[test]
    fn nothing_on_disk_is_an_empty_legacy_scope() {
        let h = home();
        let g = load_global(h.path()).unwrap();
        assert_eq!(g.source, GlobalSource::Legacy);
        assert!(g.limits.is_empty());
    }

    #[test]
    fn new_file_only() {
        let h = home();
        write_new(h.path(), "deadline: 2h\nstuck: off\n");
        let g = load_global_strict(h.path()).unwrap();
        assert_eq!(g.source, GlobalSource::File);
        assert_eq!(g.limits.deadline.as_deref(), Some("2h"));
        assert_eq!(g.limits.stuck.as_deref(), Some("off"));
    }

    #[test]
    fn legacy_only_falls_back_to_config_yaml() {
        let h = home();
        write_legacy(h.path(), "limits:\n  cost_usd: 3\n");
        let g = load_global(h.path()).unwrap();
        assert_eq!(g.source, GlobalSource::Legacy);
        assert_eq!(g.limits.cost_usd, Some(3.0));
        assert!(legacy_block_present(h.path()));
    }

    #[test]
    fn both_present_the_new_file_wins_outright() {
        let h = home();
        write_legacy(h.path(), "limits:\n  deadline: 9h\n  cost_usd: 3\n");
        write_new(h.path(), "deadline: 1h\n");
        let g = load_global(h.path()).unwrap();
        assert_eq!(g.source, GlobalSource::File);
        assert_eq!(g.limits.deadline.as_deref(), Some("1h"));
        // Replace, not merge: the legacy cost cap does not leak through.
        assert_eq!(g.limits.cost_usd, None);
        assert!(legacy_block_present(h.path()));
    }

    #[test]
    fn an_empty_new_file_still_shadows_the_legacy_block() {
        let h = home();
        write_legacy(h.path(), "limits:\n  deadline: 9h\n");
        write_new(h.path(), "");
        let g = load_global(h.path()).unwrap();
        assert_eq!(g.source, GlobalSource::File);
        assert!(g.limits.is_empty());
    }

    #[test]
    fn a_broken_new_file_is_an_error_not_the_defaults() {
        let h = home();
        write_legacy(h.path(), "limits:\n  deadline: 9h\n");
        write_new(h.path(), "deadline: [unclosed\n");
        assert!(load_global(h.path()).is_err());
        write_new(h.path(), "max_iterations: 3\n");
        let e = load_global_strict(h.path()).unwrap_err();
        assert!(e.contains("max_iterations"), "{e}");
    }

    #[test]
    fn legacy_strictness_differs_between_the_two_loaders() {
        let h = home();
        write_legacy(h.path(), "limits: [unclosed\n");
        assert!(load_global(h.path()).unwrap().limits.is_empty());
        assert!(load_global_strict(h.path()).is_err());
    }
}
