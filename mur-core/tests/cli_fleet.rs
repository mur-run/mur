//! Fleet sync Pro integration tests. Wiremock-backed HTTP tests plus
//! tempdir-based manifest/profile/push-pull round-trips.

#[tokio::test]
async fn fetch_effective_plan_reads_me() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/core/auth/me"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "effective_plan": "pro",
                "trial_active": false
            })),
        )
        .mount(&server)
        .await;

    let plan = mur_core::auth::fetch_effective_plan(&server.uri(), "tok")
        .await
        .unwrap();
    assert_eq!(plan, "pro");
}

#[test]
fn plan_allows_fleet_gates_correctly() {
    assert!(mur_core::auth::plan_allows_fleet("pro"));
    assert!(mur_core::auth::plan_allows_fleet("team"));
    assert!(mur_core::auth::plan_allows_fleet("enterprise"));
    assert!(!mur_core::auth::plan_allows_fleet("free"));
    assert!(!mur_core::auth::plan_allows_fleet("trial"));
}

#[tokio::test]
async fn fleet_push_resolves_conflict_then_succeeds() {
    use mur_common::sync_types::FleetEntityType;
    let server = wiremock::MockServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();

    // Seed a local profile so there is a change to push.
    let agents = mur.join("agents").join("scout");
    std::fs::create_dir_all(&agents).unwrap();
    std::fs::write(
        agents.join("profile.yaml"),
        "id: agent-scout\nname: scout\n",
    )
    .unwrap();

    // First push → conflict
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v1/core/fleet/agent_profile"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": false, "conflict": true})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Pull → one entity at version 4
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/core/fleet/agent_profile"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"entities": [], "version": 4})),
        )
        .mount(&server)
        .await;

    // Retry push → success
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v1/core/fleet/agent_profile"))
        .respond_with(
            wiremock::ResponseTemplate::new(200)
                .set_body_json(serde_json::json!({"ok": true, "version": 5})),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;

    let v = mur_core::cmd::fleet_sync::fleet_push(
        &server.uri(),
        "tok",
        mur,
        FleetEntityType::AgentProfile,
        false,
    )
    .await
    .unwrap();
    assert_eq!(v, 5);
}

#[tokio::test]
async fn missing_secret_ref_is_degraded_not_fatal() {
    use mur_common::sync_types::FleetEntityType;
    let server = wiremock::MockServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/core/fleet/model_binding"))
        .respond_with(
            wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
                "entities": [{
                    "logical_id": "gpt5",
                    "content_hash": "h",
                    "version": 1,
                    "deleted": false,
                    "payload": "provider: openai\nmodel: gpt-5\nsecret: env:MUR_NEVER_SET_XYZ\n"
                }],
                "version": 1
            })),
        )
        .mount(&server)
        .await;

    let report = mur_core::cmd::fleet_sync::fleet_pull(
        &server.uri(),
        "tok",
        mur,
        FleetEntityType::ModelBinding,
    )
    .await
    .unwrap();
    assert!(
        report.unresolved_secrets.contains(&"gpt5".to_string()),
        "expected gpt5 in unresolved, got {:?}",
        report.unresolved_secrets
    );
    assert!(mur.join("models.yaml").exists());
}

// ── Fleet command round-trip (job intake + roster management) ───────────

#[test]
fn fleet_job_and_roster_round_trip() {
    use mur_common::fleet::JobStatus;
    use mur_core::cmd::fleet::{create, delete, jobs, roster, store};

    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();

    // create (members need not exist on disk; names canonicalize themselves)
    create::cmd_fleet_create(
        home,
        "dev",
        vec!["pm".into()],
        None,
        Some("standing".into()),
        None,
    )
    .unwrap();

    // add member → fleet manifest stays in sync (the member must exist on
    // disk: `fleet add` validates agent existence, unlike `create`).
    let qa_dir = home.join("agents").join("qa");
    std::fs::create_dir_all(&qa_dir).unwrap();
    std::fs::write(qa_dir.join("profile.yaml"), "name: qa\n").unwrap();
    roster::cmd_fleet_add(home, "dev", vec!["qa".into()]).unwrap();
    assert!(
        store::load_fleet(home, "dev")
            .unwrap()
            .members
            .contains(&"qa".to_string())
    );

    // send job → lands queued (no execution)
    jobs::cmd_fleet_send(home, "dev", "first job").unwrap();
    let q = jobs::list_jobs(home, "dev").unwrap();
    assert_eq!(q.len(), 1);
    assert_eq!(q[0].status, JobStatus::Queued);
    assert_eq!(q[0].text, "first job");

    // remove member → manifest updated, others preserved
    roster::cmd_fleet_remove(home, "dev", vec!["qa".into()]).unwrap();
    let members = store::load_fleet(home, "dev").unwrap().members;
    assert!(!members.contains(&"qa".to_string()), "qa must be gone");
    assert!(members.contains(&"pm".to_string()), "pm must stay");

    // delete fleet → dir + channel gone; jobs removed as part of the dir
    let fleet = store::load_fleet(home, "dev").unwrap();
    let channel_id = fleet.channel_id.clone();
    delete::cmd_fleet_delete(home, "dev", true).unwrap();
    assert!(
        !store::fleet_dir(home, "dev").exists(),
        "fleet dir must be gone"
    );
    let svc = mur_channel::ChannelService::open(home).unwrap();
    assert!(
        svc.store().load_manifest(&channel_id).is_err(),
        "channel must be gone"
    );
}

// ── Skill fleet tests ────────────────────────────────────────────────

#[cfg(test)]
mod skill_fleet_tests {
    use mur_common::skill::event_log::{SkillEvent, append_event, read_events};
    use mur_common::sync_types::FleetEntity;
    use mur_core::cmd::fleet_sync::{FleetManifest, apply_fleet_pull, build_fleet_skill_changes};
    use tempfile::tempdir;

    fn write_skill(dir: &std::path::Path, name: &str, yaml: &str) {
        let d = dir.join("skills").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(d.join("skill.yaml"), yaml).unwrap();
    }

    fn write_event(dir: &std::path::Path, name: &str, ev: &SkillEvent) {
        let path = dir.join("skills").join(name).join("events.jsonl");
        append_event(&path, ev).unwrap();
    }

    fn make_retrieval(device: &str) -> SkillEvent {
        SkillEvent::Retrieval {
            ts: chrono::DateTime::from_timestamp(1_748_100_000, 0).unwrap(),
            device_id: device.into(),
        }
    }

    #[test]
    fn two_device_usage_histories_converge() {
        // Device A: has skill-alpha with 1 retrieval from device-a
        let device_a = tempdir().unwrap();
        write_skill(
            device_a.path(),
            "skill-alpha",
            "name: skill-alpha\nversion: 1.0.0\n",
        );
        write_event(device_a.path(), "skill-alpha", &make_retrieval("device-a"));

        // Device B: has skill-alpha with 1 retrieval from device-b (different device)
        let device_b = tempdir().unwrap();
        write_skill(
            device_b.path(),
            "skill-alpha",
            "name: skill-alpha\nversion: 1.0.0\n",
        );
        let ts_b = chrono::DateTime::from_timestamp(1_748_200_000, 0).unwrap();
        write_event(
            device_b.path(),
            "skill-alpha",
            &SkillEvent::Retrieval {
                ts: ts_b,
                device_id: "device-b".into(),
            },
        );

        // Simulate: Device B pushes its state as a fleet entity, Device A pulls it.
        let changes_b =
            build_fleet_skill_changes(device_b.path(), &FleetManifest::default()).unwrap();
        assert_eq!(changes_b.len(), 1);

        let ent = FleetEntity {
            logical_id: "skill-alpha".into(),
            content_hash: changes_b[0].content_hash.clone(),
            version: 1,
            deleted: false,
            payload: changes_b[0].payload.clone(),
        };

        // Apply device B's entity to device A.
        apply_fleet_pull(
            device_a.path(),
            mur_common::sync_types::FleetEntityType::Skill,
            &[ent],
        )
        .unwrap();

        // Device A's events.jsonl should now have BOTH retrievals.
        let events = read_events(&device_a.path().join("skills/skill-alpha/events.jsonl")).unwrap();
        assert_eq!(
            events.len(),
            2,
            "expected 2 events after merge, got {}",
            events.len()
        );

        // stats.json should reflect combined usage.
        let stats = mur_common::skill::stats::SkillStats::load(
            &mur_common::skill::stats::SkillStats::path(device_a.path(), "skill-alpha"),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            stats.usage_count, 1,
            "only net-new event should increment (1 from B, A's own counted separately)"
        );
    }

    #[test]
    fn event_union_is_idempotent_on_repull() {
        let dev = tempdir().unwrap();
        write_skill(
            dev.path(),
            "idem-skill",
            "name: idem-skill\nversion: 1.0.0\n",
        );
        write_event(dev.path(), "idem-skill", &make_retrieval("device-a"));

        let changes = build_fleet_skill_changes(dev.path(), &FleetManifest::default()).unwrap();
        let ent = FleetEntity {
            logical_id: "idem-skill".into(),
            content_hash: changes[0].content_hash.clone(),
            version: 1,
            deleted: false,
            payload: changes[0].payload.clone(),
        };

        // Pull twice — should not duplicate events.
        apply_fleet_pull(
            dev.path(),
            mur_common::sync_types::FleetEntityType::Skill,
            std::slice::from_ref(&ent),
        )
        .unwrap();
        apply_fleet_pull(
            dev.path(),
            mur_common::sync_types::FleetEntityType::Skill,
            &[ent],
        )
        .unwrap();

        let events = read_events(&dev.path().join("skills/idem-skill/events.jsonl")).unwrap();
        assert_eq!(events.len(), 1);
    }
}
