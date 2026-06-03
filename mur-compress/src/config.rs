use serde::{Deserialize, Serialize};
use std::path::Path;

/// Deep-merge `overlay` onto `base` (user values win; missing keys keep base).
fn merge_yaml(base: &mut serde_yaml::Value, overlay: serde_yaml::Value) {
    use serde_yaml::Value;
    match (base, overlay) {
        (Value::Mapping(b), Value::Mapping(o)) => {
            for (k, v) in o {
                match b.get_mut(&k) {
                    Some(bv) => merge_yaml(bv, v),
                    None => {
                        b.insert(k, v);
                    }
                }
            }
        }
        (b, o) => *b = o,
    }
}

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
    ///
    /// A partial `compress.yaml` is merged *onto* the defaults: any field the
    /// user omits keeps its default value, rather than silently reverting the
    /// whole config. Only a syntactically malformed file or a field with an
    /// invalid value falls back to full defaults (with a stderr warning).
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("compress.yaml");
        let Ok(text) = std::fs::read_to_string(&path) else {
            return Self::default();
        };
        let overlay: serde_yaml::Value = match serde_yaml::from_str(&text) {
            Ok(v) => v,
            Err(e) => {
                eprintln!("[mur-compress] ignoring malformed {}: {e}", path.display());
                return Self::default();
            }
        };
        let mut merged =
            serde_yaml::to_value(Self::default()).expect("CompressConfig is serializable");
        merge_yaml(&mut merged, overlay);
        match serde_yaml::from_value(merged) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "[mur-compress] {} has invalid values, using defaults: {e}",
                    path.display()
                );
                Self::default()
            }
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

    #[test]
    fn partial_config_merges_onto_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("compress.yaml"),
            "target_ratio: 0.5\nstore:\n  ttl_days: 30\n",
        )
        .unwrap();
        let c = CompressConfig::load(dir.path());
        // Overridden fields take the user value...
        assert_eq!(c.target_ratio, 0.5);
        assert_eq!(c.store.ttl_days, 30);
        // ...while omitted fields keep their defaults instead of reverting.
        assert_eq!(c.protect_head_lines, 20);
        assert_eq!(c.store.max_entries, 2000);
        assert!(c.enabled);
    }

    #[test]
    fn malformed_config_falls_back_to_defaults() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("compress.yaml"), "this: [is: not: valid").unwrap();
        let c = CompressConfig::load(dir.path());
        assert_eq!(c.protect_head_lines, 20);
    }
}
