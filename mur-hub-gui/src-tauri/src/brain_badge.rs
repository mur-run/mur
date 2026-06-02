//! Backend state for the model-upgrade nudge (spec §16): current model label
//! and a durable "don't ask again" flag. No timers, ever.

use std::path::{Path, PathBuf};

const DISMISS_MARKER: &str = ".upgrade_nudge_dismissed";

pub fn dismiss_marker_path(mur_home: &Path) -> PathBuf {
    mur_home.join(DISMISS_MARKER)
}

pub fn is_nudge_dismissed(mur_home: &Path) -> bool {
    dismiss_marker_path(mur_home).exists()
}

pub fn dismiss_nudge(mur_home: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(mur_home)?;
    std::fs::write(dismiss_marker_path(mur_home), "")
}

/// Read the seed Mur agent's current model name from its profile, if present.
pub fn current_model_label(mur_home: &Path) -> Option<String> {
    let body = std::fs::read_to_string(mur_home.join("agents/mur/profile.yaml")).ok()?;
    let profile: mur_common::AgentProfile = serde_yaml_ng::from_str(&body).ok()?;
    Some(profile.model.name)
}

#[tauri::command]
pub fn nudge_status() -> (bool, Option<String>) {
    let home = crate::mur_home_path();
    (is_nudge_dismissed(&home), current_model_label(&home))
}

#[tauri::command]
pub fn nudge_dismiss() -> Result<(), String> {
    dismiss_nudge(&crate::mur_home_path()).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn dismiss_is_durable() {
        let home = TempDir::new().unwrap();
        assert!(!is_nudge_dismissed(home.path()));
        dismiss_nudge(home.path()).unwrap();
        assert!(is_nudge_dismissed(home.path()));
    }

    #[test]
    fn model_label_reads_from_profile() {
        let home = TempDir::new().unwrap();
        std::fs::create_dir_all(home.path().join("agents/mur")).unwrap();
        // Use a profile that deserializes as AgentProfile — the seed template,
        // but with a different model name to prove we read the right field.
        let profile_yaml = include_str!("../resources/mur-agent-template/profile.yaml");
        std::fs::write(home.path().join("agents/mur/profile.yaml"), profile_yaml).unwrap();
        assert_eq!(
            current_model_label(home.path()).as_deref(),
            Some("Qwen3.5-2B-MLX-4bit")
        );
    }
}
