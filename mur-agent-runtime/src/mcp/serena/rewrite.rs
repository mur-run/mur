//! Detect serena rewriting its own config across a spawn (#1688, code-nav 3.7).
//!
//! serena re-saves `serena_config.yml` as a "migration" when a field it maps
//! is absent. The generator (`mur code-nav setup`) writes a complete file so
//! that should not happen; a hand-edited file can still trigger it. The
//! spawn gate re-runs the preflight on every spawn, so a rewrite cannot
//! smuggle a C1–C9 violation past the *next* start — but the file MUR just
//! validated no longer matches what is on disk, and the operator should know.
//!
//! The hash is taken right before spawn and compared once `initialize`
//! succeeds: serena loads (and migrates) its config before it answers.
//! Warn-only: a rewrite never fails the spawn.

use super::serena_paths;
use mur_common::agent::{McpServerEntry, McpServerKind};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// The config file's content hash at spawn time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigFingerprint {
    file: PathBuf,
    /// `None` when the file could not be read (the spawn gate will have
    /// refused already; kept so a later appearance still counts as a change).
    digest: Option<[u8; 32]>,
}

/// Hash the config of a `kind: serena` entry. `None` for any other entry,
/// or when there is no agent home (the spawn gate refuses that case).
pub fn fingerprint(entry: &McpServerEntry, agent_home: Option<&Path>) -> Option<ConfigFingerprint> {
    if entry.kind != Some(McpServerKind::Serena) {
        return None;
    }
    let file = serena_paths(agent_home?).config_file;
    Some(ConfigFingerprint {
        digest: digest(&file),
        file,
    })
}

impl ConfigFingerprint {
    /// `Some(warning)` when the file on disk no longer matches the spawn-time hash.
    pub fn rewritten(&self) -> Option<String> {
        if digest(&self.file) == self.digest {
            return None;
        }
        Some(format!(
            "serena rewrote {} after spawn (a missing field triggers its config \
             migration); the next spawn re-checks it. Re-run `mur code-nav setup` \
             to restore the MUR-managed file.",
            self.file.display()
        ))
    }
}

fn digest(file: &Path) -> Option<[u8; 32]> {
    std::fs::read(file).ok().map(|b| Sha256::digest(b).into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn serena_entry() -> McpServerEntry {
        McpServerEntry {
            name: "serena".into(),
            kind: Some(McpServerKind::Serena),
            ..Default::default()
        }
    }

    fn setup() -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let cfg = serena_paths(home.path()).config_file;
        std::fs::create_dir_all(cfg.parent().unwrap()).unwrap();
        std::fs::write(&cfg, "projects: []\n").unwrap();
        (home, cfg)
    }

    #[test]
    fn untouched_config_is_quiet() {
        let (home, _) = setup();
        let fp = fingerprint(&serena_entry(), Some(home.path())).unwrap();
        assert_eq!(fp.rewritten(), None);
    }

    #[test]
    fn rewritten_config_warns_with_path() {
        let (home, cfg) = setup();
        let fp = fingerprint(&serena_entry(), Some(home.path())).unwrap();
        std::fs::write(&cfg, "projects: []\nweb_dashboard: true\n").unwrap();
        let msg = fp.rewritten().expect("rewrite must warn");
        assert!(msg.contains(&cfg.display().to_string()), "{msg}");
    }

    #[test]
    fn same_bytes_rewritten_is_quiet() {
        // serena re-saving identical content is not a change worth a warning.
        let (home, cfg) = setup();
        let fp = fingerprint(&serena_entry(), Some(home.path())).unwrap();
        std::fs::write(&cfg, "projects: []\n").unwrap();
        assert_eq!(fp.rewritten(), None);
    }

    #[test]
    fn deleted_config_warns() {
        let (home, cfg) = setup();
        let fp = fingerprint(&serena_entry(), Some(home.path())).unwrap();
        std::fs::remove_file(&cfg).unwrap();
        assert!(fp.rewritten().is_some());
    }

    #[test]
    fn non_serena_entry_and_missing_home_are_skipped() {
        let (home, _) = setup();
        let plain = McpServerEntry {
            kind: None,
            ..serena_entry()
        };
        assert_eq!(fingerprint(&plain, Some(home.path())), None);
        assert_eq!(fingerprint(&serena_entry(), None), None);
    }
}
