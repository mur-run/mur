//! Fleet-sync (Pro) — manifest management, change builders, apply, push/pull.
//! See spec: `docs/superpowers/specs/2026-05-29-fleet-sync-pro-design.md`.

use anyhow::Result;
use mur_common::sync_types::FleetChange;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::Path;

/// `{ logical_id: { content_hash, version } }` keyed per entity type.
type FleetManifest = BTreeMap<String, FleetManifestEntry>;

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct FleetManifestEntry {
    content_hash: String,
    #[serde(default)]
    version: i64,
}

fn load_manifest(path: &Path) -> FleetManifest {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

fn hash(s: &str) -> String {
    let mut h = Sha256::new();
    h.update(s.as_bytes());
    format!("{:x}", h.finalize())
}

/// Read the `id:` field from a profile.yaml body (its stable logical id).
fn profile_logical_id(body: &str) -> Option<String> {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("id:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Build fleet changes for agent profiles by diffing `~/.mur/agents/*/profile.yaml`
/// against the manifest. Never reads `identity.key`.
pub fn build_fleet_profile_changes(mur_dir: &Path, manifest_path: &Path) -> Result<Vec<FleetChange>> {
    let manifest = load_manifest(manifest_path);
    let agents_dir = mur_dir.join("agents");
    let mut changes = Vec::new();
    if !agents_dir.exists() {
        return Ok(changes);
    }
    for entry in std::fs::read_dir(&agents_dir)? {
        let dir = entry?.path();
        let profile = dir.join("profile.yaml");
        if !profile.is_file() {
            continue;
        }
        let body = std::fs::read_to_string(&profile)?;
        let Some(id) = profile_logical_id(&body) else {
            continue;
        };
        let ch = hash(&body);
        if manifest.get(&id).map(|m| m.content_hash.as_str()) != Some(ch.as_str()) {
            changes.push(FleetChange {
                action: "upsert".into(),
                logical_id: id,
                content_hash: ch,
                payload: Some(body),
            });
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn build_changes_detects_new_and_changed_profiles() {
        let mur = tempdir().unwrap();
        let agents = mur.path().join("agents");
        fs::create_dir_all(agents.join("scout")).unwrap();
        fs::write(
            agents.join("scout/profile.yaml"),
            "id: agent-scout\nname: scout\n",
        )
        .unwrap();
        // also a stray private key that must NOT be included
        fs::write(agents.join("scout/identity.key"), b"\x00\x01secret").unwrap();

        let manifest = mur.path().join(".fleet_manifest.json");
        let changes = build_fleet_profile_changes(mur.path(), &manifest).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].action, "upsert");
        assert_eq!(changes[0].logical_id, "agent-scout");
        let payload = changes[0].payload.as_ref().unwrap();
        assert!(payload.contains("name: scout"));
        assert!(!payload.contains("secret")); // key file never included
    }
}
