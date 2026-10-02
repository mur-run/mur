use super::*;
use tempfile::tempdir;

#[test]
fn load_returns_empty_when_file_missing() {
    let dir = tempdir().unwrap();
    let r = ModelRegistry::load_from(&dir.path().join("nope.yaml")).unwrap();
    assert_eq!(r.models.len(), 0);
    assert_eq!(r.schema_version, 1);
}

#[test]
fn save_then_load_round_trips() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("models.yaml");
    let mut r = ModelRegistry::default();
    r.models.insert(
        "x".into(),
        ModelEntry {
            provider: "ollama".into(),
            model: "llama3.2:3b".into(),
            base_url: None,
            secret: None,
            capabilities: vec![],
            params: serde_json::Value::Null,
            tier: None,
            cost_per_1k_tokens: None,
            input_cost_per_1k: None,
            output_cost_per_1k: None,
            context_window: None,
            priced_at: None,
            ..Default::default()
        },
    );
    r.save_to(&p).unwrap();
    let r2 = ModelRegistry::load_from(&p).unwrap();
    assert_eq!(r, r2);
}

#[test]
fn save_uses_atomic_rename() {
    let dir = tempdir().unwrap();
    let p = dir.path().join("models.yaml");
    ModelRegistry::default().save_to(&p).unwrap();
    let temp = dir.path().join("models.yaml.tmp");
    assert!(!temp.exists(), "atomic temp left behind");
}
