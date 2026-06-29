//! Parallel tracks config — extends fleet.yaml `parallel:` section.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ParallelMode {
    Speculative,
    Partition,
}

impl Default for ParallelMode {
    fn default() -> Self {
        Self::Speculative
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrackConfig {
    pub name: String,
    pub approach: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rubric {
    #[serde(default = "default_correctness")]
    pub correctness: f32,
    #[serde(default = "default_design")]
    pub design: f32,
    #[serde(default = "default_maintainability")]
    pub maintainability: f32,
    #[serde(default = "default_security")]
    pub security: f32,
}

fn default_correctness() -> f32 {
    0.40
}
fn default_design() -> f32 {
    0.30
}
fn default_maintainability() -> f32 {
    0.20
}
fn default_security() -> f32 {
    0.10
}

impl Default for Rubric {
    fn default() -> Self {
        Self {
            correctness: default_correctness(),
            design: default_design(),
            maintainability: default_maintainability(),
            security: default_security(),
        }
    }
}

impl Rubric {
    pub fn version(&self) -> String {
        format!(
            "c{:.2}d{:.2}m{:.2}s{:.2}",
            self.correctness, self.design, self.maintainability, self.security
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct JudgeConfig {
    pub model: String,
    #[serde(default)]
    pub rubric: Rubric,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PreFilterKind {
    CargoCheck,
    CargoClippyDeny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParallelConfig {
    #[serde(default)]
    pub mode: ParallelMode,
    pub tracks: Vec<TrackConfig>,
    pub judge: JudgeConfig,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pre_filter: Vec<PreFilterKind>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rubric_version_is_stable() {
        let r = Rubric::default();
        assert_eq!(r.version(), "c0.40d0.30m0.20s0.10");
    }

    #[test]
    fn parallel_config_roundtrips_yaml() {
        let yaml = r#"
mode: speculative
judge:
  model: claude-opus-4-8
tracks:
  - name: track-a
    approach: "functional style"
  - name: track-b
    approach: "performance first"
"#;
        let cfg: ParallelConfig = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(cfg.tracks.len(), 2);
        assert_eq!(cfg.mode, ParallelMode::Speculative);
        let back = serde_yaml::to_string(&cfg).unwrap();
        let cfg2: ParallelConfig = serde_yaml::from_str(&back).unwrap();
        assert_eq!(cfg, cfg2);
    }
}
