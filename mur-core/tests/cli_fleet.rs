//! Fleet sync Pro integration tests. Wiremock-backed HTTP tests plus
//! tempdir-based manifest/profile/push-pull round-trips.

#[tokio::test]
async fn fetch_effective_plan_reads_me() {
    let server = wiremock::MockServer::start().await;
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/core/auth/me"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(serde_json::json!({
            "effective_plan": "pro",
            "trial_active": false
        })))
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
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"ok": false, "conflict": true}),
        ))
        .up_to_n_times(1)
        .mount(&server)
        .await;

    // Pull → one entity at version 4
    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path("/api/v1/core/fleet/agent_profile"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"entities": [], "version": 4}),
        ))
        .mount(&server)
        .await;

    // Retry push → success
    wiremock::Mock::given(wiremock::matchers::method("POST"))
        .and(wiremock::matchers::path("/api/v1/core/fleet/agent_profile"))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
            serde_json::json!({"ok": true, "version": 5}),
        ))
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
    use mur_common::sync_types::{FleetEntity, FleetEntityType};
    let server = wiremock::MockServer::start().await;
    let tmp = tempfile::tempdir().unwrap();
    let mur = tmp.path();

    wiremock::Mock::given(wiremock::matchers::method("GET"))
        .and(wiremock::matchers::path(
            "/api/v1/core/fleet/model_binding",
        ))
        .respond_with(wiremock::ResponseTemplate::new(200).set_body_json(
            serde_json::json!({
                "entities": [{
                    "logical_id": "gpt5",
                    "content_hash": "h",
                    "version": 1,
                    "deleted": false,
                    "payload": "provider: openai\nmodel: gpt-5\nsecret: env:MUR_NEVER_SET_XYZ\n"
                }],
                "version": 1
            }),
        ))
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
