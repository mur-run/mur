// MUR Core v2 — store module
//
// YAML files are the source of truth for workflows and pipelines; skills live
// under `~/.mur/skills/<name>/skill.yaml` via `mur_common::skill`.

pub mod config;
pub mod embedding;
pub mod entitlements_pin;
pub mod pipeline_yaml;
pub mod spot_rate;
pub mod vector;
pub mod versioned;
pub mod workflow_yaml;

/// Default MUR root directory: `~/.mur/` (honours `MUR_HOME`).
pub fn default_mur_dir() -> std::path::PathBuf {
    mur_common::home::mur_home_lossy()
}
