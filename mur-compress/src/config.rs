use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompressConfig {
    pub enabled: bool,
    pub tokenizer: String,
    pub target_ratio: f32,
    pub bloat_threshold: f32,
    pub protect_head_lines: usize,
    pub protect_tail_lines: usize,
    pub retrieve_top_k: usize,
    pub retrieve_score_threshold: f32,
    pub detect: DetectCfg,
    pub store: StoreCfg,
    pub stats: StatsCfg,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DetectCfg {
    pub search_min_ratio: f32,
    pub log_min_ratio: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StoreCfg {
    pub dir: Option<String>,
    pub ttl_days: u64,
    pub max_entries: usize,
    pub max_bytes: u64,
    pub compress_at_rest: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatsCfg {
    pub cost_per_mtok_usd: f64,
}

impl Default for CompressConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            tokenizer: "cl100k_base".into(),
            target_ratio: 0.30,
            bloat_threshold: 0.20,
            protect_head_lines: 20,
            protect_tail_lines: 10,
            retrieve_top_k: 20,
            retrieve_score_threshold: 0.30,
            detect: DetectCfg::default(),
            store: StoreCfg::default(),
            stats: StatsCfg::default(),
        }
    }
}

impl Default for DetectCfg {
    fn default() -> Self {
        Self {
            search_min_ratio: 0.6,
            log_min_ratio: 0.5,
        }
    }
}

impl Default for StoreCfg {
    fn default() -> Self {
        Self {
            dir: None,
            ttl_days: 7,
            max_entries: 2000,
            max_bytes: 536_870_912,
            compress_at_rest: true,
        }
    }
}

impl Default for StatsCfg {
    fn default() -> Self {
        Self {
            cost_per_mtok_usd: 3.0,
        }
    }
}

impl CompressConfig {
    /// Load config from a directory, falling back to defaults if no file is found.
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("compress.yaml");
        if let Ok(text) = std::fs::read_to_string(&path) {
            serde_yaml::from_str(&text).unwrap_or_default()
        } else {
            Self::default()
        }
    }

    /// TTL in seconds derived from `store.ttl_days`.
    pub fn ttl_secs(&self) -> u64 {
        self.store.ttl_days * 86_400
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_sane() {
        let c = CompressConfig::default();
        assert_eq!(c.store.ttl_days, 7);
        assert_eq!(c.ttl_secs(), 7 * 86_400);
        assert_eq!(c.retrieve_top_k, 20);
        assert!(c.store.compress_at_rest);
    }

    #[test]
    fn missing_config_file_yields_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let c = CompressConfig::load(dir.path());
        assert_eq!(c.protect_head_lines, 20);
    }
}
