use mur_agent_runtime::profile::Profile;
use mur_agent_runtime::protocol::a2a_server::MethodHandler;
use mur_agent_runtime::protocol::methods::card::CardHandler;
use std::sync::Arc;

fn test_profile() -> Profile {
    use tempfile::TempDir;
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("agent_test");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        include_str!("fixtures/card_full_profile.yaml"),
    )
    .unwrap();
    let p = Profile::load(&dir).unwrap();
    std::mem::forget(tmp);
    p
}

#[tokio::test]
async fn card_includes_pubkey_endpoints_deployment() {
    let p = Arc::new(test_profile());
    let handler = CardHandler::new(p);
    let json = handler.handle(None).await.unwrap();

    assert_eq!(json["pubkey"], "zTESTPUB");
    let eps = json["endpoints"].as_array().unwrap();
    // order: tcp first (most reachable), then unix-socket, then stdio
    assert_eq!(eps[0]["transport"], "tcp+noise");
    assert_eq!(eps[0]["reachability"], "lan");
    assert_eq!(json["deployment"]["type"], "docker");
    assert_eq!(json["deployment"]["environment"], "prod");
}
