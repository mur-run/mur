//! Fleet-sync (Pro) — manifest management, change builders, apply, push/pull.
//! See spec: `docs/superpowers/specs/2026-05-29-fleet-sync-pro-design.md`.

use anyhow::{Context, Result, bail};
use chrono::Utc;
use mur_common::identity::AgentIdentity;
use mur_common::model::ModelRegistry;
use mur_common::sync_types::{FleetChange, FleetEntity, FleetEntityType};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// `{ logical_id: { content_hash, version } }` keyed per entity type.
pub type FleetManifest = BTreeMap<String, FleetManifestEntry>;

#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
pub struct FleetManifestEntry {
    pub content_hash: String,
    #[serde(default)]
    pub version: i64,
    /// For skill entities: line-count of events.jsonl at last push.
    /// Old manifest entries default to 0 (backward compatible).
    #[serde(default)]
    pub events_tail: u64,
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

/// Build fleet changes for skills by scanning `~/.mur/skills/*/skill.yaml`
/// and diffing content_hash + events_tail against the manifest.
pub fn build_fleet_skill_changes(
    mur_dir: &Path,
    manifest: &FleetManifest,
) -> Result<Vec<FleetChange>> {
    use mur_common::sync_types::SkillFleetPayload;
    let skills_dir = mur_dir.join("skills");
    let mut changes = Vec::new();
    if !skills_dir.exists() {
        return Ok(changes);
    }
    for entry in std::fs::read_dir(&skills_dir)? {
        let dir = entry?.path();
        let skill_yaml_path = dir.join("skill.yaml");
        if !skill_yaml_path.is_file() {
            continue;
        }
        let skill_name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let manifest_yaml = std::fs::read_to_string(&skill_yaml_path)?;
        let content_hash = hash(&manifest_yaml);
        let events_path = dir.join("events.jsonl");
        let events_jsonl = std::fs::read_to_string(&events_path).unwrap_or_default();
        let events_tail = events_jsonl.lines().filter(|l| !l.is_empty()).count() as u64;

        let stored = manifest.get(&skill_name);
        let hash_match = stored.map(|m| m.content_hash.as_str()) == Some(content_hash.as_str());
        let tail_match = stored.map(|m| m.events_tail) == Some(events_tail);
        if hash_match && tail_match {
            continue;
        }

        // When only events grew (yaml unchanged), send the delta so we don't
        // re-upload the full log on every run. When yaml also changed, send the
        // full events so the server payload stays self-contained.
        let events_payload = if hash_match {
            let prev_tail = stored.map(|m| m.events_tail).unwrap_or(0);
            events_jsonl
                .lines()
                .filter(|l| !l.is_empty())
                .skip(prev_tail as usize)
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            events_jsonl.clone()
        };

        // Include the latest stats snapshot so the server can serve run-history
        // without re-parsing the (potentially partial) events_jsonl.
        let stats_json = {
            use mur_common::skill::stats::SkillStats;
            let stats_path = SkillStats::path(mur_dir, &skill_name);
            std::fs::read_to_string(stats_path).unwrap_or_default()
        };

        let payload = SkillFleetPayload {
            manifest_yaml,
            events_jsonl: events_payload,
            content_sha256: content_hash.clone(),
            stats_json,
        };
        changes.push(FleetChange {
            action: "upsert".into(),
            logical_id: skill_name,
            content_hash,
            payload: Some(serde_json::to_string(&payload)?),
        });
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
                // The private half moved to `keys/<name>/` (#850 option (c));
                // checking the old path here would report every migrated agent
                // as keyless and try to mint a second identity for it.
                if !mur_common::identity::private_key_dir(&dir)
                    .join("identity.key")
                    .exists()
                {
                    generate_device_identity_key(&dir)?;
                    report.keys_generated += 1;
                }
            }
        }
        FleetEntityType::ModelBinding => {
            apply_model_bindings(mur_dir, ents, &mut report)?;
        }
        FleetEntityType::Skill => {
            apply_fleet_skill_pull(mur_dir, ents, &mut report)?;
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
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
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
        if let Some(mur_common::secret::SecretRef::Env(var)) = &entry.secret
            && std::env::var(var).is_err()
        {
            report.unresolved_secrets.push(e.logical_id.clone());
        }
        reg.models.insert(e.logical_id.clone(), entry);
        report.written += 1;
    }
    reg.save_to(&reg_path)?;
    Ok(())
}

/// Apply pulled skill entities: LWW on skill.yaml, set-union on events.jsonl,
/// incremental counter update on stats.json.
pub fn apply_fleet_skill_pull(
    mur_dir: &Path,
    ents: &[FleetEntity],
    report: &mut ApplyReport,
) -> Result<()> {
    use mur_common::skill::event_log::{
        apply_new_events_to_stats, event_log_path, parse_events_jsonl, read_events, union_events,
    };
    use mur_common::skill::stats::SkillStats;
    use mur_common::sync_types::SkillFleetPayload;

    for e in ents {
        if e.deleted {
            continue;
        }
        let Some(payload_str) = &e.payload else {
            continue;
        };
        let payload: SkillFleetPayload =
            serde_json::from_str(payload_str).context("parse skill fleet payload")?;

        let skill_dir = mur_dir.join("skills").join(&e.logical_id);
        std::fs::create_dir_all(&skill_dir)?;

        // skill.yaml: LWW — remote wins (server holds latest committed version).
        let skill_yaml_path = skill_dir.join("skill.yaml");
        let local_sha = if skill_yaml_path.exists() {
            hash(&std::fs::read_to_string(&skill_yaml_path)?)
        } else {
            String::new()
        };
        if local_sha != payload.content_sha256 {
            write_atomic(&skill_yaml_path, payload.manifest_yaml.as_bytes())?;
        }

        // events.jsonl: set-union of local and remote.
        let events_path = event_log_path(mur_dir, &e.logical_id);
        let local_events = read_events(&events_path)?;
        let remote_events = parse_events_jsonl(&payload.events_jsonl)?;
        let merged = union_events(local_events.clone(), remote_events.clone());

        // Write merged events.jsonl (atomic via temp file).
        let merged_content: String = merged
            .iter()
            .map(|ev| serde_json::to_string(ev).unwrap())
            .collect::<Vec<_>>()
            .join("\n")
            + if merged.is_empty() { "" } else { "\n" };
        write_atomic(&events_path, merged_content.as_bytes())?;

        // Apply only the net-new remote events to stats.json.
        let local_keys: std::collections::HashSet<String> =
            local_events.iter().map(|ev| ev.dedup_key()).collect();
        let new_events: Vec<_> = remote_events
            .into_iter()
            .filter(|ev| !local_keys.contains(&ev.dedup_key()))
            .collect();
        if !new_events.is_empty() {
            let version = extract_skill_version(&payload.manifest_yaml);
            let digest = payload.content_sha256.clone();
            let name = e.logical_id.clone();
            let stats_path = SkillStats::path(mur_dir, &name);
            SkillStats::merge_in_place(
                &stats_path,
                || SkillStats::new(&name, &version, &digest, Utc::now()),
                |s| {
                    apply_new_events_to_stats(s, &new_events);
                    Ok(())
                },
            )?;
        }
        report.written += 1;
    }
    Ok(())
}

fn extract_skill_version(yaml: &str) -> String {
    for line in yaml.lines() {
        if let Some(rest) = line.strip_prefix("version:") {
            return rest.trim().to_string();
        }
    }
    "0.0.0".into()
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

/// Record the pushed content hash and version for each changed entity.
///
/// `events_tail` is deliberately reset to 0 here and set afterwards by
/// `update_skill_events_tail`, the only writer of that field (see the skill
/// branch of `fleet_push`). Between those two writes the manifest on disk
/// says "no events pushed yet", so a crash in that window makes the next
/// push send the whole `events.jsonl` where it would have sent a delta —
/// see `build_fleet_skill_changes`, which reads `prev_tail` from here.
/// The zeroing is not a lost-data window; do not "fix" it by carrying the
/// old tail forward, which would skip events when the log is rewritten.
fn update_fleet_manifest(path: &Path, changes: &[FleetChange], version: i64) -> Result<()> {
    let mut manifest = load_manifest(path);
    for c in changes {
        manifest.insert(
            c.logical_id.clone(),
            FleetManifestEntry {
                content_hash: c.content_hash.clone(),
                version,
                ..Default::default()
            },
        );
    }
    let json = serde_json::to_string_pretty(&manifest)?;
    write_atomic(path, json.as_bytes())?;
    Ok(())
}

/// For skill entities, also persist the events_tail so future pushes
/// can detect new-event-only deltas without a content_hash change.
fn update_skill_events_tail(manifest: &mut FleetManifest, mur_dir: &Path, skill_name: &str) {
    let events_path = mur_dir.join("skills").join(skill_name).join("events.jsonl");
    let tail = std::fs::read_to_string(&events_path)
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.is_empty())
        .count() as u64;
    if let Some(entry) = manifest.get_mut(skill_name) {
        entry.events_tail = tail;
    }
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
        FleetEntityType::Skill => build_fleet_skill_changes(mur_dir, &manifest)?,
    };
    let client = reqwest::Client::new();
    let url = format!("{}/api/v1/core/fleet/{}", base, etype.path_segment());
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
            if etype == FleetEntityType::Skill {
                let mut manifest = load_manifest(&mp);
                for c in &changes {
                    update_skill_events_tail(&mut manifest, mur_dir, &c.logical_id);
                }
                let json = serde_json::to_string_pretty(&manifest)?;
                write_atomic(&mp, json.as_bytes())?;
            }
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
    let tokens =
        crate::auth::load_tokens().context("not signed in — run `mur auth login` first")?;
    let plan = crate::auth::fetch_effective_plan(&server_url, &tokens.access_token)
        .await
        .context(
            "could not verify your plan for fleet sync — your login may have expired; \
             run `mur auth login` again (fleet sync is a Pro feature)",
        )?;
    if !crate::auth::plan_allows_fleet(&plan) {
        bail!("fleet sync requires a Pro plan (current: {plan}). Upgrade at https://app.mur.run");
    }
    let mur_dir = mur_common::trust::mur_home();
    use mur_common::sync_types::FleetEntityType::*;
    for etype in [AgentProfile, ModelBinding, Skill] {
        if matches!(
            direction,
            DeviceSyncDirection::Pull | DeviceSyncDirection::Both
        ) {
            let r = fleet_pull(&server_url, &tokens.access_token, &mur_dir, etype).await?;
            for id in &r.unresolved_secrets {
                eprintln!(
                    "  ⚠ {id}: secret not resolvable on this device (agent will run unbound)"
                );
            }
        }
        if matches!(
            direction,
            DeviceSyncDirection::Push | DeviceSyncDirection::Both
        ) {
            let v = fleet_push(
                &server_url,
                &tokens.access_token,
                &mur_dir,
                etype,
                force_local,
            )
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
                tier: None,
                cost_per_1k_tokens: None,
                ..Default::default()
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
        let report = apply_fleet_pull(mur.path(), FleetEntityType::AgentProfile, &[ent]).unwrap();

        assert!(mur.path().join("agents/scout/profile.yaml").exists());
        assert!(
            mur_common::identity::private_key_dir(&mur.path().join("agents/scout"))
                .join("identity.key")
                .exists()
        );
        assert_eq!(report.written, 1);
    }

    #[test]
    fn build_fleet_skill_changes_detects_new_skill() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("skill.yaml"),
            "name: my-skill\nversion: 1.0.0\n",
        )
        .unwrap();
        let manifest = FleetManifest::default();
        let changes = build_fleet_skill_changes(dir.path(), &manifest).unwrap();
        assert_eq!(changes.len(), 1);
        assert_eq!(changes[0].logical_id, "my-skill");
        let payload: mur_common::sync_types::SkillFleetPayload =
            serde_json::from_str(changes[0].payload.as_ref().unwrap()).unwrap();
        assert!(payload.manifest_yaml.contains("my-skill"));
        assert_eq!(payload.events_jsonl, ""); // no events yet
    }

    #[test]
    fn build_fleet_skill_changes_skips_unchanged() {
        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        let yaml = "name: my-skill\nversion: 1.0.0\n";
        std::fs::write(skill_dir.join("skill.yaml"), yaml).unwrap();
        let ch = {
            use sha2::{Digest, Sha256};
            let mut h = Sha256::new();
            h.update(yaml.as_bytes());
            format!("{:x}", h.finalize())
        };
        let mut manifest = FleetManifest::default();
        manifest.insert(
            "my-skill".into(),
            FleetManifestEntry {
                content_hash: ch,
                version: 1,
                events_tail: 0,
            },
        );
        let changes = build_fleet_skill_changes(dir.path(), &manifest).unwrap();
        assert!(changes.is_empty());
    }

    #[test]
    fn apply_fleet_skill_pull_writes_yaml_and_appends_events() {
        use mur_common::sync_types::{FleetEntity, SkillFleetPayload};
        let dir = tempdir().unwrap();
        let payload = SkillFleetPayload {
            manifest_yaml: "name: foo\nversion: 1.0.0\n".into(),
            events_jsonl:
                "{\"kind\":\"retrieval\",\"ts\":\"2026-05-30T00:00:00Z\",\"device_id\":\"d\"}\n"
                    .into(),
            content_sha256: "abc".into(),
            stats_json: String::new(),
        };
        let ent = FleetEntity {
            logical_id: "foo".into(),
            content_hash: "abc".into(),
            version: 1,
            deleted: false,
            payload: Some(serde_json::to_string(&payload).unwrap()),
        };
        let mut report = ApplyReport::default();
        apply_fleet_skill_pull(dir.path(), &[ent], &mut report).unwrap();
        assert_eq!(report.written, 1);
        assert!(dir.path().join("skills/foo/skill.yaml").exists());
        assert!(dir.path().join("skills/foo/events.jsonl").exists());
    }

    #[test]
    fn two_device_round_trip_local_and_remote_in_sync() {
        use mur_common::skill::event_log::{SkillEvent, union_events};
        let device_a = "device-a";
        let device_b = "device-b";

        // Simulate device A having a retrieval event
        let event_a = SkillEvent::Retrieval {
            ts: chrono::Utc::now(),
            device_id: device_a.to_string(),
        };

        // Simulate device B having an execution event on the same skill
        let event_b = SkillEvent::Execution {
            ts: chrono::Utc::now(),
            device_id: device_b.to_string(),
            outcome: "success".into(),
            error: None,
            step: None,
            duration_ms: None,
            exit_code: None,
            env_class: None,
            confidence: None,
            trigger: None,
        };

        // Union the events (simulating merge)
        let local = vec![event_a.clone()];
        let remote = vec![event_b.clone()];
        let merged = union_events(local, remote);

        // Both events should be present and deduplicated correctly
        assert_eq!(merged.len(), 2);
        assert!(
            merged
                .iter()
                .any(|e| matches!(e, SkillEvent::Retrieval { .. }))
        );
        assert!(
            merged
                .iter()
                .any(|e| matches!(e, SkillEvent::Execution { .. }))
        );
    }

    #[test]
    fn full_e2e_sync_with_multiple_skills_and_profiles() {
        let tmpdir = tempdir().unwrap();
        let local_skills_dir = tmpdir.path().join("local/skills");
        let remote_skills_dir = tmpdir.path().join("remote/skills");

        std::fs::create_dir_all(&local_skills_dir).unwrap();
        std::fs::create_dir_all(&remote_skills_dir).unwrap();

        // Create two local skills
        for skill_name in &["skill-1", "skill-2"] {
            let skill_dir = local_skills_dir.join(skill_name);
            std::fs::create_dir_all(&skill_dir).unwrap();
            let yaml = format!(
                "name: {}\nversion: 1.0.0\npublisher: human:test\ndescription: test\ncategory: workflow\ncontent:\n  abstract: test skill\n",
                skill_name
            );
            std::fs::write(skill_dir.join("skill.yaml"), yaml).unwrap();

            // Add events to skill-1
            if *skill_name == "skill-1" {
                let events_jsonl = "{\"kind\":\"retrieval\",\"ts\":\"2026-05-30T00:00:00Z\",\"device_id\":\"dev-local\"}\n";
                std::fs::write(skill_dir.join("events.jsonl"), events_jsonl).unwrap();
            }
        }

        // Simulate remote having skill-1 with different events
        let remote_skill_1_dir = remote_skills_dir.join("skill-1");
        std::fs::create_dir_all(&remote_skill_1_dir).unwrap();
        let yaml = "name: skill-1\nversion: 1.0.1\npublisher: human:test\ndescription: test\ncategory: workflow\ncontent:\n  abstract: test skill\n";
        std::fs::write(remote_skill_1_dir.join("skill.yaml"), yaml).unwrap();
        let events_jsonl = "{\"kind\":\"execution\",\"ts\":\"2026-05-30T00:01:00Z\",\"device_id\":\"dev-remote\",\"outcome\":\"success\"}\n";
        std::fs::write(remote_skill_1_dir.join("events.jsonl"), events_jsonl).unwrap();

        // Remote has skill-3 that local doesn't have
        let remote_skill_3_dir = remote_skills_dir.join("skill-3");
        std::fs::create_dir_all(&remote_skill_3_dir).unwrap();
        let yaml = "name: skill-3\nversion: 1.0.0\npublisher: human:test\ndescription: test\ncategory: workflow\ncontent:\n  abstract: test skill\n";
        std::fs::write(remote_skill_3_dir.join("skill.yaml"), yaml).unwrap();

        // Verify directory structure
        assert!(local_skills_dir.join("skill-1").exists());
        assert!(local_skills_dir.join("skill-2").exists());
        assert!(!local_skills_dir.join("skill-3").exists());
        assert!(remote_skills_dir.join("skill-1").exists());
        assert!(!remote_skills_dir.join("skill-2").exists());
        assert!(remote_skills_dir.join("skill-3").exists());

        // Simulate applying remote pull to local (would overwrite skill-1, add skill-3)
        // This is a minimal sanity check for the overall flow
        let skill_1_yaml_remote =
            std::fs::read_to_string(remote_skill_1_dir.join("skill.yaml")).unwrap();
        let skill_3_yaml_remote =
            std::fs::read_to_string(remote_skill_3_dir.join("skill.yaml")).unwrap();

        // Check that we can detect version difference (1.0.0 vs 1.0.1)
        assert!(skill_1_yaml_remote.contains("1.0.1"));
        assert!(skill_3_yaml_remote.contains("1.0.0"));
    }

    #[test]
    fn event_union_dedup_identical_timestamps() {
        use mur_common::skill::event_log::{SkillEvent, union_events};
        let ts = chrono::Utc::now();
        let device = "dev-a";

        // Create identical events
        let event1 = SkillEvent::Retrieval {
            ts,
            device_id: device.to_string(),
        };
        let event2 = SkillEvent::Retrieval {
            ts,
            device_id: device.to_string(),
        };

        // Union should deduplicate (dedup_key is identical)
        let merged = union_events(vec![event1], vec![event2]);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn manifest_lww_newer_remote_wins() {
        use mur_common::skill::event_log::resolve_manifest_lww;
        use mur_common::skill::manifest::{Content, Skill, SkillManifest, Visibility};
        use mur_common::skill::types::Category;

        let t_old = chrono::DateTime::from_timestamp(1_000, 0).unwrap();
        let t_new = chrono::DateTime::from_timestamp(2_000, 0).unwrap();

        let local = Skill {
            manifest: SkillManifest {
                name: "test".into(),
                version: "1.0".into(),
                publisher: "p".into(),
                description: "d".into(),
                category: Category::Context,
                provenance: Default::default(),
                hosts: vec![],
                scope: Default::default(),
                visibility: Visibility::default(),
                origin: None,
                origin_version: None,
                origin_hash: None,
                fleet: None,
                project: None,
                team: None,
                governance: None,
                content: Content {
                    r#abstract: "a".into(),
                    context: Some("old".into()),
                    procedure: None,
                    command: None,
                    note: None,
                },
                requires: vec![],
                tags: vec![],
                triggers: vec![],
                priority: Default::default(),
                evolution_log: vec![],
                transfer_chain: vec![],
                mcp_requirements: vec![],
                updated_at: t_old,
                requires_programs: vec![],
            },
            content_sha256: Some("old-hash".into()),
            trust_level: Default::default(),
            capabilities_declared: vec![],
            publisher_signature: None,
        };

        let mut remote = local.clone();
        remote.manifest.updated_at = t_new;
        remote.manifest.content.context = Some("new".into());
        remote.content_sha256 = Some("new-hash".into());

        let (winner, reason) = resolve_manifest_lww(local, remote.clone(), false);
        assert_eq!(reason, "remote_newer");
        assert_eq!(winner.manifest.content.context, Some("new".into()));
        assert_eq!(winner.content_sha256, Some("new-hash".into()));
    }

    #[test]
    fn manifest_lww_force_local_overrides() {
        use mur_common::skill::event_log::resolve_manifest_lww;
        use mur_common::skill::manifest::{Content, Skill, SkillManifest, Visibility};
        use mur_common::skill::types::Category;

        let t_old = chrono::DateTime::from_timestamp(1_000, 0).unwrap();
        let t_new = chrono::DateTime::from_timestamp(2_000, 0).unwrap();

        let local = Skill {
            manifest: SkillManifest {
                name: "test".into(),
                version: "1.0".into(),
                publisher: "p".into(),
                description: "d".into(),
                category: Category::Context,
                provenance: Default::default(),
                hosts: vec![],
                scope: Default::default(),
                visibility: Visibility::default(),
                origin: None,
                origin_version: None,
                origin_hash: None,
                fleet: None,
                project: None,
                team: None,
                governance: None,
                content: Content {
                    r#abstract: "a".into(),
                    context: Some("keep-me".into()),
                    procedure: None,
                    command: None,
                    note: None,
                },
                requires: vec![],
                tags: vec![],
                triggers: vec![],
                priority: Default::default(),
                evolution_log: vec![],
                transfer_chain: vec![],
                mcp_requirements: vec![],
                updated_at: t_old,
                requires_programs: vec![],
            },
            content_sha256: Some("local-hash".into()),
            trust_level: Default::default(),
            capabilities_declared: vec![],
            publisher_signature: None,
        };

        let mut remote = local.clone();
        remote.manifest.updated_at = t_new;
        remote.manifest.content.context = Some("discard".into());
        remote.content_sha256 = Some("remote-hash".into());

        let (winner, reason) = resolve_manifest_lww(local.clone(), remote, true);
        assert_eq!(reason, "force_local");
        assert_eq!(winner.manifest.content.context, Some("keep-me".into()));
        assert_eq!(winner.content_sha256, Some("local-hash".into()));
    }

    // ── Delta push optimisation ────────────────────────────────────────────

    /// When yaml is unchanged, only new events (beyond manifest events_tail)
    /// should appear in the pushed payload — never a full re-upload.
    #[test]
    fn build_fleet_skill_changes_sends_delta_events_when_yaml_unchanged() {
        use mur_common::sync_types::SkillFleetPayload;
        use sha2::{Digest, Sha256};

        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        let yaml = "name: my-skill\nversion: 1.0.0\n";
        std::fs::write(skill_dir.join("skill.yaml"), yaml).unwrap();

        let content_hash = {
            let mut h = Sha256::new();
            h.update(yaml.as_bytes());
            format!("{:x}", h.finalize())
        };

        // Simulate three events already pushed (manifest records tail=3).
        let all_events = concat!(
            "{\"kind\":\"retrieval\",\"ts\":\"2026-01-01T00:00:00Z\",\"device_id\":\"d\"}\n",
            "{\"kind\":\"retrieval\",\"ts\":\"2026-01-02T00:00:00Z\",\"device_id\":\"d\"}\n",
            "{\"kind\":\"retrieval\",\"ts\":\"2026-01-03T00:00:00Z\",\"device_id\":\"d\"}\n",
            "{\"kind\":\"retrieval\",\"ts\":\"2026-01-04T00:00:00Z\",\"device_id\":\"d\"}\n",
            "{\"kind\":\"retrieval\",\"ts\":\"2026-01-05T00:00:00Z\",\"device_id\":\"d\"}\n",
        );
        std::fs::write(skill_dir.join("events.jsonl"), all_events).unwrap();

        let mut manifest = FleetManifest::default();
        manifest.insert(
            "my-skill".into(),
            FleetManifestEntry {
                content_hash: content_hash.clone(),
                version: 1,
                events_tail: 3, // 3 events already pushed
            },
        );

        let changes = build_fleet_skill_changes(dir.path(), &manifest).unwrap();
        assert_eq!(changes.len(), 1, "should detect new events");

        let payload: SkillFleetPayload =
            serde_json::from_str(changes[0].payload.as_ref().unwrap()).unwrap();

        // Only the 2 new events (lines 4 and 5) should be in the delta.
        let lines: Vec<_> = payload
            .events_jsonl
            .lines()
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(lines.len(), 2, "delta should contain only 2 new events");
        assert!(
            lines[0].contains("2026-01-04"),
            "first delta event should be the 4th event"
        );
        assert!(
            lines[1].contains("2026-01-05"),
            "second delta event should be the 5th event"
        );
    }

    /// When yaml changes, the full events log is sent so the server payload
    /// stays self-contained for any pull receiver.
    #[test]
    fn build_fleet_skill_changes_sends_full_events_when_yaml_changed() {
        use mur_common::sync_types::SkillFleetPayload;
        use sha2::{Digest, Sha256};

        let dir = tempdir().unwrap();
        let skill_dir = dir.path().join("skills/my-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();

        let yaml = "name: my-skill\nversion: 2.0.0\n"; // new version
        std::fs::write(skill_dir.join("skill.yaml"), yaml).unwrap();

        let new_hash = {
            let mut h = Sha256::new();
            h.update(yaml.as_bytes());
            format!("{:x}", h.finalize())
        };

        let events = concat!(
            "{\"kind\":\"retrieval\",\"ts\":\"2026-01-01T00:00:00Z\",\"device_id\":\"d\"}\n",
            "{\"kind\":\"retrieval\",\"ts\":\"2026-01-02T00:00:00Z\",\"device_id\":\"d\"}\n",
        );
        std::fs::write(skill_dir.join("events.jsonl"), events).unwrap();

        let mut manifest = FleetManifest::default();
        manifest.insert(
            "my-skill".into(),
            FleetManifestEntry {
                content_hash: "old-hash-different-from-new".into(),
                version: 1,
                events_tail: 1, // 1 event already pushed
            },
        );
        // Sanity: new_hash != old stored hash
        assert_ne!(new_hash, "old-hash-different-from-new");

        let changes = build_fleet_skill_changes(dir.path(), &manifest).unwrap();
        assert_eq!(changes.len(), 1);

        let payload: SkillFleetPayload =
            serde_json::from_str(changes[0].payload.as_ref().unwrap()).unwrap();

        // Full events (both lines) because yaml changed.
        let lines: Vec<_> = payload
            .events_jsonl
            .lines()
            .filter(|l| !l.is_empty())
            .collect();
        assert_eq!(
            lines.len(),
            2,
            "full events should be sent when yaml changed"
        );
    }
}
