use mur_agent_runtime::protocol::a2a_server::MethodHandler;
use mur_agent_runtime::protocol::methods::card::CardHandler;
use std::sync::Arc;

#[tokio::test]
async fn card_returns_agent_identity_and_card_fields() {
    let profile = load_test_profile();
    let handler = CardHandler::new(Arc::new(profile));
    let result = handler.handle(None).await.unwrap();
    assert_eq!(result["name"], "agent_a");
    assert_eq!(result["protocolVersion"], "a2a/0.3");
    assert!(
        result["capabilities"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c == "a2a.message.send")
    );
    let transports = result["transports"].as_array().unwrap();
    assert!(
        transports.iter().any(|t| t == "stdio"),
        "stdio must be present: {:?}",
        transports
    );
    assert!(
        result["entitlements"].is_object(),
        "entitlements must be exposed on card"
    );
}

fn load_test_profile() -> mur_agent_runtime::profile::Profile {
    use tempfile::TempDir;
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("agent_a");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("profile.yaml"),
        include_str!("fixtures/profile_minimal.yaml"),
    )
    .unwrap();
    let p = mur_agent_runtime::profile::Profile::load(&dir).unwrap();
    std::mem::forget(tmp);
    p
}

#[tokio::test]
async fn card_emits_installed_skills_block() {
    use tempfile::TempDir;

    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("agent_b");
    std::fs::create_dir_all(&dir).unwrap();

    let base = include_str!("fixtures/profile_minimal.yaml");
    let yaml = format!(
        "{base}installed_skills:\n  - name: find-prices\n    version: 1.0.0\n    publisher: human:alice\n    description: find prices\n    category: workflow\n    abstract: looks up prices\n    transfer_chain:\n      - agent://alice\n"
    );
    std::fs::write(dir.join("profile.yaml"), yaml).unwrap();
    let profile = mur_agent_runtime::profile::Profile::load(&dir).unwrap();
    std::mem::forget(tmp);

    let handler = CardHandler::new(Arc::new(profile));
    let card = handler.handle(None).await.unwrap();
    let entries = card["installed_skills"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["name"], "find-prices");
    assert_eq!(entries[0]["transfer_chain"][0], "agent://alice");
}
