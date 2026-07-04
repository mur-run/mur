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
