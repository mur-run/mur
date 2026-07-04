//! Settings › Models slot commands — thin wrappers over
//! mur_core::model_setup::slots (single source of truth for slot writes).

use mur_core::model_setup::slots::{ModelSlotsView, SlotId, SlotSelection};

#[tauri::command]
pub fn model_slots_get() -> Result<ModelSlotsView, String> {
    mur_core::model_setup::slots::get_slots().map_err(|e| e.to_string())
}

#[tauri::command]
pub fn model_slots_set(slot: SlotId, sel: SlotSelection) -> Result<ModelSlotsView, String> {
    mur_core::model_setup::slots::set_slot(slot, &sel).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct SetupStatus {
    pub needs_setup: bool,
}

#[tauri::command]
pub fn model_setup_status() -> Result<SetupStatus, String> {
    let cfg = mur_core::store::config::load_config().map_err(|e| e.to_string())?;
    let reg_empty = mur_common::model::ModelRegistry::default_path()
        .and_then(|p| mur_common::model::ModelRegistry::load_from(&p))
        .map(|r| r.models.is_empty())
        .unwrap_or(true);
    Ok(SetupStatus {
        needs_setup: reg_empty && mur_core::model_setup::is_factory_default_models(&cfg),
    })
}

#[derive(serde::Serialize)]
pub struct SetupPreview {
    pub summary: String,
    pub has_plan: bool,
}

async fn build_plan() -> Result<mur_core::model_setup::ModelSetupPlan, String> {
    let discovered = mur_core::discovery::run_all().await.unwrap_or_default();
    let mut keys = mur_core::model_setup::probe_env_keys();
    if let Ok(p) = mur_common::model::ModelRegistry::default_path()
        && let Ok(reg) = mur_common::model::ModelRegistry::load_from(&p)
    {
        keys.extend(mur_core::model_setup::keychain_key_sources(&reg));
    }
    Ok(mur_core::model_setup::recommend(&discovered, &keys))
}

#[tauri::command]
pub async fn model_setup_preview() -> Result<SetupPreview, String> {
    let plan = build_plan().await?;
    Ok(SetupPreview { summary: plan.summary.clone(), has_plan: plan.smart.is_some() })
}

#[tauri::command]
pub async fn model_setup_apply_recommended() -> Result<SetupPreview, String> {
    let plan = build_plan().await?;
    if plan.smart.is_some() {
        let mut cfg = mur_core::store::config::load_config().map_err(|e| e.to_string())?;
        mur_core::model_setup::apply(&plan, &mut cfg);
        mur_core::store::config::save_config(&cfg).map_err(|e| e.to_string())?;
    }
    Ok(SetupPreview { summary: plan.summary.clone(), has_plan: plan.smart.is_some() })
}
