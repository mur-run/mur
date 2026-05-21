//! Legacy `~/.mur/trust.json` → `~/.mur/trust/trust.yaml` migration.
//!
//! Runs exactly once: on first access to the trust store, if legacy JSON
//! exists and the new YAML store does not. v1 is a stub — it discards the
//! legacy content and starts the new YAML store fresh, after preserving
//! the legacy file as `.mur/trust.json.legacy.bak` for forensic recovery.

use crate::muragent::MuragentError;
use std::path::Path;

pub fn migrate_legacy(legacy_path: &Path, new_path: &Path) -> Result<(), MuragentError> {
    if let Some(parent) = new_path.parent() {
        std::fs::create_dir_all(parent).map_err(MuragentError::Io)?;
    }

    let empty = super::TrustStore::default();
    let yaml = serde_yaml_ng::to_string(&empty)
        .map_err(|e| MuragentError::Other(format!("trust store serialize: {e}")))?;
    std::fs::write(new_path, &yaml).map_err(MuragentError::Io)?;

    // Move the legacy file aside rather than deleting outright — preserves
    // the user's ability to recover if migration drops data they cared about.
    let backup = legacy_path.with_extension("json.legacy.bak");
    if let Err(e) = std::fs::rename(legacy_path, &backup) {
        tracing::warn!(
            "could not move legacy trust.json to {}: {e}",
            backup.display()
        );
    }

    tracing::info!("migrated legacy trust.json → trust/trust.yaml (legacy preserved as .bak)");
    Ok(())
}
