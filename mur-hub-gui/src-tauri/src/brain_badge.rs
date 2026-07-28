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

/// True while the concierge is still running the brain it shipped with — i.e.
/// the user has never pointed it at a registry model. `model_ref` is the
/// codebase-wide marker for "the user chose this" (see
/// `seed_mur::ensure_concierge_model`, which refuses to touch a profile that
/// has one), so its absence is what the upgrade nudge is actually about.
/// A profile that cannot be read is treated as "not stock": never nag about an
/// agent we failed to inspect.
pub fn is_stock_brain(mur_home: &Path) -> bool {
    let Ok(body) = std::fs::read_to_string(mur_home.join("agents/mur/profile.yaml")) else {
        return false;
    };
    match serde_yaml_ng::from_str::<mur_common::AgentProfile>(&body) {
        Ok(profile) => profile.model_ref.is_none(),
        Err(_) => false,
    }
}

#[derive(serde::Serialize)]
pub struct NudgeStatus {
    /// The user pressed "no thanks" at some point — never nag again.
    pub dismissed: bool,
    /// Human-readable name of the concierge's current model, for display.
    pub model: Option<String>,
    /// Whether the nudge has anything to offer: only true while the concierge
    /// is still on its stock brain.
    pub stock_brain: bool,
}

#[tauri::command]
pub fn nudge_status() -> NudgeStatus {
    let home = crate::mur_home_path();
    NudgeStatus {
        dismissed: is_nudge_dismissed(&home),
        model: current_model_label(&home),
        stock_brain: is_stock_brain(&home),
    }
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

    #[test]
    fn stock_brain_only_until_a_model_ref_is_chosen() {
        let home = TempDir::new().unwrap();
        // No profile at all → nothing to nag about.
        assert!(!is_stock_brain(home.path()));

        std::fs::create_dir_all(home.path().join("agents/mur")).unwrap();
        let path = home.path().join("agents/mur/profile.yaml");
        let stock = include_str!("../resources/mur-agent-template/profile.yaml");
        std::fs::write(&path, stock).unwrap();
        assert!(
            is_stock_brain(home.path()),
            "seed template has no model_ref — the nudge should offer an upgrade"
        );

        let mut profile: mur_common::AgentProfile = serde_yaml_ng::from_str(stock).unwrap();
        profile.model_ref = Some("anthropic_opus_5".into());
        std::fs::write(&path, serde_yaml_ng::to_string(&profile).unwrap()).unwrap();
        assert!(
            !is_stock_brain(home.path()),
            "the user picked a brain — never nag again"
        );
    }
}
