//! Per-turn copy-on-write track tuning (`turn_track:`).

use super::*;

/// Commands (first word) that trigger the warm clone.
pub const TURN_TRACK_DEFAULT_TRIGGERS: &[&str] = &["cargo"];
/// Project-relative build-cache dirs that get a per-turn target.
pub const TURN_TRACK_DEFAULT_CACHE_DIRS: &[&str] = &["target"];
/// Per-profile subdirs the warm clone leaves out (path-keyed, useless in a track).
pub const TURN_TRACK_DEFAULT_SKIP_SUBDIRS: &[&str] = &["incremental"];
/// Below this many GiB free, the warm clone is skipped (the build runs cold).
pub const TURN_TRACK_DEFAULT_MIN_FREE_GIB: u64 = 20;
/// `<project>/target/<this>/<track-name>/` holds each turn's target dir.
pub const TURN_TRACK_DEFAULT_TURN_TARGET_SUBDIR: &str = "turns";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TurnTrackConfig {
    #[serde(default)]
    pub warm_build_cache: WarmBuildCacheConfig,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WarmBuildCacheConfig {
    #[serde(default = "turn_track_default_enabled")]
    pub enabled: bool,
    #[serde(default = "turn_track_default_triggers")]
    pub triggers: Vec<String>,
    #[serde(default = "turn_track_default_cache_dirs")]
    pub cache_dirs: Vec<String>,
    #[serde(default = "turn_track_default_skip_subdirs")]
    pub skip_subdirs: Vec<String>,
    #[serde(default = "turn_track_default_min_free_gib")]
    pub min_free_gib: u64,
    /// Joined under `<project>/target/`; must be one normal path component.
    /// Read it through [`WarmBuildCacheConfig::validated_subdir`].
    #[serde(default = "turn_track_default_turn_target_subdir")]
    pub turn_target_subdir: String,
}

impl WarmBuildCacheConfig {
    /// `turn_target_subdir` if it is a single normal path component,
    /// otherwise the default — a bad value must not escape `target/`.
    pub fn validated_subdir(&self) -> &str {
        let mut parts = std::path::Path::new(&self.turn_target_subdir).components();
        match (parts.next(), parts.next()) {
            (Some(std::path::Component::Normal(_)), None) => &self.turn_target_subdir,
            _ => TURN_TRACK_DEFAULT_TURN_TARGET_SUBDIR,
        }
    }
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

fn turn_track_default_enabled() -> bool {
    true
}

fn turn_track_default_triggers() -> Vec<String> {
    strings(TURN_TRACK_DEFAULT_TRIGGERS)
}

fn turn_track_default_cache_dirs() -> Vec<String> {
    strings(TURN_TRACK_DEFAULT_CACHE_DIRS)
}

fn turn_track_default_skip_subdirs() -> Vec<String> {
    strings(TURN_TRACK_DEFAULT_SKIP_SUBDIRS)
}

fn turn_track_default_min_free_gib() -> u64 {
    TURN_TRACK_DEFAULT_MIN_FREE_GIB
}

fn turn_track_default_turn_target_subdir() -> String {
    TURN_TRACK_DEFAULT_TURN_TARGET_SUBDIR.to_string()
}

impl Default for WarmBuildCacheConfig {
    fn default() -> Self {
        Self {
            enabled: turn_track_default_enabled(),
            triggers: turn_track_default_triggers(),
            cache_dirs: turn_track_default_cache_dirs(),
            skip_subdirs: turn_track_default_skip_subdirs(),
            min_free_gib: turn_track_default_min_free_gib(),
            turn_target_subdir: turn_track_default_turn_target_subdir(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_track_defaults_when_absent() {
        let c: Config = serde_yaml::from_str("{}").unwrap();
        let w = &c.turn_track.warm_build_cache;
        assert!(w.enabled);
        assert_eq!(w.triggers, vec!["cargo".to_string()]);
        assert_eq!(w.cache_dirs, vec!["target".to_string()]);
        assert_eq!(w.skip_subdirs, vec!["incremental".to_string()]);
        assert_eq!(w.min_free_gib, 20);
        assert_eq!(w.turn_target_subdir, "turns");
    }

    #[test]
    fn turn_track_partial_override() {
        let c: Config =
            serde_yaml::from_str("turn_track:\n  warm_build_cache:\n    min_free_gib: 5\n")
                .unwrap();
        let w = &c.turn_track.warm_build_cache;
        assert_eq!(w.min_free_gib, 5);
        assert!(w.enabled);
        assert_eq!(w.triggers, vec!["cargo".to_string()]);
        assert_eq!(w.cache_dirs, vec!["target".to_string()]);
        assert_eq!(w.skip_subdirs, vec!["incremental".to_string()]);
        assert_eq!(w.turn_target_subdir, "turns");
    }

    #[test]
    fn turn_target_subdir_rejects_path_escape() {
        let with = |s: &str| WarmBuildCacheConfig {
            turn_target_subdir: s.to_string(),
            ..Default::default()
        };
        for bad in ["", "..", ".", "a/b", "/abs", "../x"] {
            assert_eq!(with(bad).validated_subdir(), "turns", "input {bad:?}");
        }
        assert_eq!(with("x").validated_subdir(), "x");
    }
}
