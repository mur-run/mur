use super::*;
use mur_common::model::{ModelEntry, ModelRegistry};

fn seed(home: &std::path::Path, keys: &[&str]) {
    let mut reg = ModelRegistry::default();
    for k in keys {
        reg.models.insert((*k).into(), ModelEntry::default());
    }
    reg.save_to(&home.join("models.yaml")).unwrap();
}

/// A one-character miss on a registry key used to produce an ollama agent
/// bound to a model ollama does not have, reported as a success.
#[test]
fn names_the_key_only_when_it_is_unambiguous() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    seed(home, &["anthropic_claude_opus_5", "omlx"]);

    // Missing underscore, wrong case, no separators — all the same key.
    for typed in [
        "anthropic_claude_opus5",
        "Anthropic-Claude-Opus-5",
        "anthropicclaudeopus5",
    ] {
        assert_eq!(
            near_registry_key(home, typed).unwrap().as_deref(),
            Some("anthropic_claude_opus_5"),
            "typed {typed}"
        );
    }
    // A genuine ollama model id must not be dressed up as a typo.
    assert_eq!(near_registry_key(home, "llama3.2:3b").unwrap(), None);
    assert_eq!(near_registry_key(home, "").unwrap(), None);
}

#[test]
fn stays_silent_when_two_keys_collide() {
    let tmp = tempfile::TempDir::new().unwrap();
    let home = tmp.path();
    // Both reduce to "gpt5".
    seed(home, &["gpt_5", "gpt-5"]);
    assert_eq!(
        near_registry_key(home, "gpt5").unwrap(),
        None,
        "a hint that has to pick between two keys is a guess"
    );
}

#[test]
fn missing_registry_is_not_an_error() {
    let tmp = tempfile::TempDir::new().unwrap();
    assert_eq!(near_registry_key(tmp.path(), "anything").unwrap(), None);
}
