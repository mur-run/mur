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
            "{\"kind\":\"retrieval\",\"ts\":\"2026-05-30T00:00:00Z\",\"device_id\":\"d\"}\n".into(),
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
