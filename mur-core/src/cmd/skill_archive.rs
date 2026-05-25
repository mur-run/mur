//! `mur skill archive <name>` CLI handler (M5b).
//!
//! Flips `lifecycle_state` to `Archived` via `merge_in_place`. Idempotent:
//! archiving an already-archived skill is a no-op success.

use anyhow::Result;
use mur_common::skill::stats::{LifecycleState, SkillStats};

use super::agent::resolve_mur_home;

pub fn cmd_archive(name: &str, reason: Option<&str>) -> Result<()> {
    let home = resolve_mur_home()?;
    let path = SkillStats::path(&home, name);
    let reason_str = reason.map(|r| format!("archived: {r}")).unwrap_or_default();

    SkillStats::merge_in_place(
        &path,
        || SkillStats::new(name, "unknown", "", chrono::Utc::now()),
        |s| {
            s.lifecycle_state = LifecycleState::Archived;
            s.lifecycle_changed_at = chrono::Utc::now();
            if !reason_str.is_empty() {
                s.pinned_reason = reason_str.clone();
            }
            Ok(())
        },
    )?;
    println!("Archived {name}.");
    Ok(())
}
