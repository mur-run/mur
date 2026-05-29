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
