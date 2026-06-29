//! Per-track execution driver — P1 types.

pub mod diversity;
pub mod filter;
pub mod worktree;

use anyhow::Result;
use mur_common::parallel::TrackConfig;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Track {
    pub config: TrackConfig,
    pub worktree_path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrackSet {
    pub tracks: Vec<Track>,
}

impl TrackSet {
    pub fn save(&self, dir: &Path) -> Result<()> {
        let path = dir.join("tracks.json");
        let json = serde_json::to_string_pretty(self)?;
        std::fs::write(path, json)?;
        Ok(())
    }

    pub fn load(dir: &Path) -> Result<Self> {
        let path = dir.join("tracks.json");
        let json = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("cannot read tracks.json at {path:?}: {e}"))?;
        Ok(serde_json::from_str(&json)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::parallel::TrackConfig;

    #[test]
    fn trackset_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let ts = TrackSet {
            tracks: vec![
                Track {
                    config: TrackConfig {
                        name: "track-a".into(),
                        approach: "functional".into(),
                        model: None,
                    },
                    worktree_path: PathBuf::from("/tmp/track-a"),
                },
                Track {
                    config: TrackConfig {
                        name: "track-b".into(),
                        approach: "performance".into(),
                        model: None,
                    },
                    worktree_path: PathBuf::from("/tmp/track-b"),
                },
            ],
        };
        ts.save(dir.path()).unwrap();
        let loaded = TrackSet::load(dir.path()).unwrap();
        assert_eq!(loaded.tracks.len(), 2);
        assert_eq!(loaded.tracks[0].config.name, "track-a");
        assert_eq!(
            loaded.tracks[1].worktree_path,
            PathBuf::from("/tmp/track-b")
        );
    }
}
