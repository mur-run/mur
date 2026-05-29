//! Fleet-sync (Pro) — manifest management, change builders, apply, push/pull.
//! See spec: `docs/superpowers/specs/2026-05-29-fleet-sync-pro-design.md`.

use anyhow::{Context, Result, bail};
use mur_common::identity::AgentIdentity;
use mur_common::model::ModelRegistry;
use mur_common::sync_types::{FleetChange, FleetEntity, FleetEntityType};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `{ logical_id: { content_hash, version } }` keyed per entity type.
pub(crate) type FleetManifest = BTreeMap<String, FleetManifestEntry>;

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub(crate) struct FleetManifestEntry {
    pub(crate) content_hash: String,
    #[serde(default)]
    pub(crate) version: i64,
}

pub(crate) fn load_manifest(path: &Path) -> FleetManifest {
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
pub fn build_fleet_profile_changes(
    mur_dir: &Path,
    manifest: &FleetManifest,
) -> Result<Vec<FleetChange>> {
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

/// Build fleet changes for model bindings by diffing models.yaml entries
/// against the manifest. Secrets are `SecretRef`s (env/keychain/file/cmd) —
/// serialized as references, so no plaintext leaves the device.
pub fn build_fleet_model_changes(
    reg: &ModelRegistry,
    manifest: &FleetManifest,
) -> Result<Vec<FleetChange>> {
    let mut changes = Vec::new();
    for (key, entry) in &reg.models {
        let body = serde_yaml_ng::to_string(entry)?;
        let ch = hash(&body);
        if manifest.get(key).map(|m| m.content_hash.as_str()) != Some(ch.as_str()) {
            changes.push(FleetChange {
                action: "upsert".into(),
                logical_id: key.clone(),
                content_hash: ch,
                payload: Some(body),
            });
        }
    }
    Ok(changes)
}

// ── Apply pull ──────────────────────────────────────────────────────

#[derive(Default, Debug)]
pub struct ApplyReport {
    pub written: usize,
    pub keys_generated: usize,
    /// logical_ids whose model-binding secret-ref does not resolve locally.
    pub unresolved_secrets: Vec<String>,
}

/// Write pulled entities to disk. For agent profiles, ensure a per-device
/// identity key exists (generate one if absent).
pub fn apply_fleet_pull(
    mur_dir: &Path,
    etype: FleetEntityType,
    ents: &[FleetEntity],
) -> Result<ApplyReport> {
    let mut report = ApplyReport::default();
    match etype {
        FleetEntityType::AgentProfile => {
            for e in ents {
                if e.deleted {
                    continue;
                }
                let Some(body) = &e.payload else {
                    continue;
                };
                let slug = profile_slug(body);
                let dir = mur_dir.join("agents").join(&slug);
                std::fs::create_dir_all(&dir)?;
                write_atomic(&dir.join("profile.yaml"), body.as_bytes())?;
                report.written += 1;
                if !dir.join("identity.key").exists() {
                    generate_device_identity_key(&dir)?;
                    report.keys_generated += 1;
                }
            }
        }
        FleetEntityType::ModelBinding => {
            apply_model_bindings(mur_dir, ents, &mut report)?;
        }
    }
    Ok(report)
}

fn profile_slug(body: &str) -> String {
    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("name:") {
            return rest.trim().to_string();
        }
    }
    "unknown".into()
}

fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}

fn generate_device_identity_key(dir: &Path) -> Result<()> {
    let identity = AgentIdentity::generate();
    identity
        .save(dir)
        .with_context(|| format!("save identity.key to {}", dir.display()))?;
    Ok(())
}

fn apply_model_bindings(
    mur_dir: &Path,
    ents: &[FleetEntity],
    report: &mut ApplyReport,
) -> Result<()> {
    let reg_path = mur_dir.join("models.yaml");
    let mut reg = ModelRegistry::load_from(&reg_path).unwrap_or_default();
    for e in ents {
        if e.deleted {
            reg.models.remove(&e.logical_id);
            report.written += 1;
            continue;
        }
        let Some(body) = &e.payload else {
            continue;
        };
        let entry: mur_common::model::ModelEntry = serde_yaml_ng::from_str(body)?;
        // Best-effort check: flag env-var refs that don't resolve locally.
        if let Some(mur_common::secret::SecretRef::Env(var)) = &entry.secret {
            if std::env::var(var).is_err() {
                report.unresolved_secrets.push(e.logical_id.clone());
            }
        }
        reg.models.insert(e.logical_id.clone(), entry);
        report.written += 1;
    }
    reg.save_to(&reg_path)?;
    Ok(())
}

// ── Push / Pull flow ────────────────────────────────────────────────

fn version_path(mur_dir: &Path, etype: FleetEntityType) -> PathBuf {
    mur_dir.join(format!(".fleet_version_{}", etype.path_segment()))
}

fn read_version(mur_dir: &Path, etype: FleetEntityType) -> i64 {
    std::fs::read_to_string(version_path(mur_dir, etype))
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0)
}

fn manifest_path(mur_dir: &Path, etype: FleetEntityType) -> PathBuf {
    mur_dir.join(format!(".fleet_manifest_{}.json", etype.path_segment()))
}

fn update_fleet_manifest(
    path: &Path,
    changes: &[FleetChange],
    version: i64,
) -> Result<()> {
    let mut manifest = load_manifest(path);
    for c in changes {
        manifest.insert(
            c.logical_id.clone(),
            FleetManifestEntry {
                content_hash: c.content_hash.clone(),
                version,
            },
        );
    }
    let json = serde_json::to_string_pretty(&manifest)?;
    write_atomic(path, json.as_bytes())?;
    Ok(())
}

pub async fn fleet_pull(
    base: &str,
    token: &str,
    mur_dir: &Path,
    etype: FleetEntityType,
) -> Result<ApplyReport> {
    let since = read_version(mur_dir, etype);
    let url = format!(
        "{}/api/v1/core/fleet/{}?since={}",
        base,
        etype.path_segment(),
        since
    );
    let resp: mur_common::sync_types::FleetPullResponse = reqwest::Client::new()
        .get(url)
        .bearer_auth(token)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;
    let report = apply_fleet_pull(mur_dir, etype, &resp.entities)?;
    std::fs::write(version_path(mur_dir, etype), resp.version.to_string())?;
    Ok(report)
}

pub async fn fleet_push(
    base: &str,
    token: &str,
    mur_dir: &Path,
    etype: FleetEntityType,
    force_local: bool,
) -> Result<i64> {
    use mur_common::sync_types::{FleetPushRequest, FleetPushResponse};

    let mp = manifest_path(mur_dir, etype);
    let manifest = load_manifest(&mp);
    let changes = match etype {
        FleetEntityType::AgentProfile => build_fleet_profile_changes(mur_dir, &manifest)?,
        FleetEntityType::ModelBinding => {
            let reg_path = mur_dir.join("models.yaml");
            let reg = ModelRegistry::load_from(&reg_path).unwrap_or_default();
            build_fleet_model_changes(&reg, &manifest)?
        }
    };
    let client = reqwest::Client::new();
    let url = format!(
        "{}/api/v1/core/fleet/{}",
        base,
        etype.path_segment()
    );
    let mut base_version = read_version(mur_dir, etype);

    for attempt in 0..2 {
        let req = FleetPushRequest {
            base_version,
            entity_type: etype,
            changes: changes.clone(),
            force_local,
        };
        let resp: FleetPushResponse = client
            .post(&url)
            .bearer_auth(token)
            .json(&req)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if let Some(v) = resp.version {
            std::fs::write(version_path(mur_dir, etype), v.to_string())?;
            update_fleet_manifest(&mp, &changes, v)?;
            return Ok(v);
        }
        if resp.conflict.unwrap_or(false) && attempt == 0 && !force_local {
            fleet_pull(base, token, mur_dir, etype).await?;
            base_version = read_version(mur_dir, etype);
            continue;
        }
        anyhow::bail!("fleet push failed (conflict unresolved)");
    }
    anyhow::bail!("fleet push exhausted retries")
}

/// CLI entry point for `mur sync fleet`. Checks Pro entitlement, then
/// pushes and/or pulls each fleet entity type.
pub async fn fleet_sync_cmd(direction: DeviceSyncDirection, force_local: bool) -> Result<()> {
    let server_url = crate::auth::server_url();
    let tokens = crate::auth::load_tokens().context("not signed in — run `mur login` first")?;
    let plan = crate::auth::fetch_effective_plan(&server_url, &tokens.access_token).await?;
    if !crate::auth::plan_allows_fleet(&plan) {
        bail!("fleet sync requires a Pro plan (current: {plan}). Upgrade at https://app.mur.run");
    }
    let mur_dir = mur_common::trust::mur_home();
    use mur_common::sync_types::FleetEntityType::*;
    for etype in [AgentProfile, ModelBinding] {
        if matches!(
            direction,
            DeviceSyncDirection::Pull | DeviceSyncDirection::Both
        ) {
            let r = fleet_pull(&server_url, &tokens.access_token, &mur_dir, etype).await?;
            for id in &r.unresolved_secrets {
                eprintln!("  ⚠ {id}: secret not resolvable on this device (agent will run unbound)");
            }
        }
        if matches!(
            direction,
            DeviceSyncDirection::Push | DeviceSyncDirection::Both
        ) {
            let v = fleet_push(&server_url, &tokens.access_token, &mur_dir, etype, force_local)
                .await?;
            eprintln!("  pushed {etype:?} (version {v})");
        }
    }
    Ok(())
}

/// Re-export for CLI dispatch.
pub use crate::cmd::sync_cmd::DeviceSyncDirection;

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::model::ModelEntry;
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
        fs::write(agents.join("scout/identity.key"), b"\x00\x01secret").unwrap();

        let manifest: FleetManifest = BTreeMap::new();
        let changes = build_fleet_profile_changes(mur.path(), &manifest).unwrap();

        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].action, "upsert");
        assert_eq!(changes[0].logical_id, "agent-scout");
        let payload = changes[0].payload.as_ref().unwrap();
        assert!(payload.contains("name: scout"));
        assert!(!payload.contains("secret"));
    }

    #[test]
    fn build_model_binding_changes_keeps_secret_as_ref() {
        let mut reg = ModelRegistry::default();
        reg.models.insert(
            "gpt5".into(),
            ModelEntry {
                provider: "openai".into(),
                model: "gpt-5".into(),
                base_url: None,
                secret: Some("keychain:mur/openai".parse().unwrap()),
                capabilities: vec![],
                params: serde_json::Value::Null,
            },
        );

        let changes = build_fleet_model_changes(&reg, &BTreeMap::new()).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].logical_id, "gpt5");
        let payload = changes[0].payload.as_ref().unwrap();
        assert!(payload.contains("keychain"));
        assert!(!payload.to_lowercase().contains("sk-"));
    }

    #[test]
    fn apply_pull_writes_profile_and_generates_missing_key() {
        let mur = tempdir().unwrap();
        let ent = FleetEntity {
            logical_id: "agent-scout".into(),
            content_hash: "h".into(),
            version: 1,
            deleted: false,
            payload: Some("id: agent-scout\nname: scout\n".into()),
        };
        let report =
            apply_fleet_pull(mur.path(), FleetEntityType::AgentProfile, &[ent]).unwrap();

        assert!(mur
            .path()
            .join("agents/scout/profile.yaml")
            .exists());
        assert!(mur.path().join("agents/scout/identity.key").exists());
        assert_eq!(report.written, 1);
    }
}
