use std::path::{Path, PathBuf};
use tempfile::TempDir;

#[tokio::test]
async fn import_png_lands_in_inbox_with_unsigned_trust() {
    let tmp = TempDir::new().unwrap();
    let _guard = mur_common::test_env::EnvGuard::set([("MUR_HOME", tmp.path())]);
    let agent_dir = tmp.path().join("agents/import-test");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let fixture = std::fs::read_to_string("../mur-common/tests/fixtures/profile_p0a_minimal.yaml")
        .or_else(|_| std::fs::read_to_string("mur-common/tests/fixtures/profile_p0a_minimal.yaml"))
        .unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        fixture.replace("name: agent_test", "name: import-test"),
    )
    .unwrap();

    let png_path = tmp.path().join("input.png");
    std::fs::write(
        &png_path,
        std::fs::read("mur-core/tests/fixtures/cards/silly-v3.png")
            .or_else(|_| std::fs::read("../mur-core/tests/fixtures/cards/silly-v3.png"))
            .unwrap(),
    )
    .unwrap();

    let result =
        mur_core::cmd::agent_companion::card::import::import_card("import-test", &png_path)
            .await
            .unwrap();

    // Card lands in inbox.
    let inbox = agent_dir.join("inbox/cards");
    let entries: Vec<_> = std::fs::read_dir(&inbox).unwrap().collect();
    assert_eq!(entries.len(), 2, "yaml + meta sidecar");

    // Meta sidecar marks unsigned.
    let meta_path: PathBuf = entries
        .iter()
        .find_map(|e| {
            let p = e.as_ref().unwrap().path();
            (p.extension().and_then(|s| s.to_str()) == Some("json")).then_some(p)
        })
        .unwrap();
    let meta: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&meta_path).unwrap()).unwrap();
    assert_eq!(meta["import_trust"], "unsigned");
    assert_eq!(meta["id"], result.id.as_str());

    // Card YAML present + parseable.
    let yaml_path = inbox.join(format!("{}.murcard.yaml", result.id));
    let body = std::fs::read_to_string(&yaml_path).unwrap();
    let card: mur_core::character_card::schema::MurCard = serde_yaml_ng::from_str(&body).unwrap();
    assert_eq!(card.data.name, "TestV3");
    assert_eq!(result.trust, "unsigned");
}

#[tokio::test]
async fn import_yaml_card_works() {
    let tmp = TempDir::new().unwrap();
    let _guard = mur_common::test_env::EnvGuard::set([("MUR_HOME", tmp.path())]);
    let agent_dir = tmp.path().join("agents/yaml-import");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let fixture = std::fs::read_to_string("../mur-common/tests/fixtures/profile_p0a_minimal.yaml")
        .or_else(|_| std::fs::read_to_string("mur-common/tests/fixtures/profile_p0a_minimal.yaml"))
        .unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        fixture.replace("name: agent_test", "name: yaml-import"),
    )
    .unwrap();

    let yaml = "spec: murcard_v1\nspec_version: \"1.0\"\ndata:\n  name: FromYaml\n";
    let yaml_path = tmp.path().join("hand.murcard.yaml");
    std::fs::write(&yaml_path, yaml).unwrap();

    let r = mur_core::cmd::agent_companion::card::import::import_card("yaml-import", &yaml_path)
        .await
        .unwrap();
    assert_eq!(r.trust, "unsigned");
    let entries: Vec<_> = std::fs::read_dir(agent_dir.join("inbox/cards"))
        .unwrap()
        .collect();
    assert_eq!(entries.len(), 2);
}

#[tokio::test]
async fn list_returns_imported_cards() {
    let tmp = TempDir::new().unwrap();
    let _guard = mur_common::test_env::EnvGuard::set([("MUR_HOME", tmp.path())]);
    let agent_dir = tmp.path().join("agents/list-test");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let fixture = std::fs::read_to_string("../mur-common/tests/fixtures/profile_p0a_minimal.yaml")
        .or_else(|_| std::fs::read_to_string("mur-common/tests/fixtures/profile_p0a_minimal.yaml"))
        .unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        fixture.replace("name: agent_test", "name: list-test"),
    )
    .unwrap();

    let png_path = tmp.path().join("input.png");
    std::fs::write(
        &png_path,
        std::fs::read("mur-core/tests/fixtures/cards/silly-v3.png")
            .or_else(|_| std::fs::read("../mur-core/tests/fixtures/cards/silly-v3.png"))
            .unwrap(),
    )
    .unwrap();
    let r = mur_core::cmd::agent_companion::card::import::import_card("list-test", &png_path)
        .await
        .unwrap();

    let entries = mur_core::cmd::agent_companion::card::list::list_inbox("list-test").unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, r.id);
    assert_eq!(entries[0].trust, "unsigned");
    assert_eq!(entries[0].name, "TestV3");
}

#[tokio::test]
async fn list_empty_inbox_returns_empty_vec() {
    let tmp = TempDir::new().unwrap();
    let _guard = mur_common::test_env::EnvGuard::set([("MUR_HOME", tmp.path())]);
    let agent_dir = tmp.path().join("agents/empty");
    std::fs::create_dir_all(&agent_dir).unwrap();
    let fixture = std::fs::read_to_string("../mur-common/tests/fixtures/profile_p0a_minimal.yaml")
        .or_else(|_| std::fs::read_to_string("mur-common/tests/fixtures/profile_p0a_minimal.yaml"))
        .unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        fixture.replace("name: agent_test", "name: empty"),
    )
    .unwrap();
    let entries = mur_core::cmd::agent_companion::card::list::list_inbox("empty").unwrap();
    assert!(entries.is_empty());
}
