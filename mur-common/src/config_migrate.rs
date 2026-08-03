//! One-shot migration of the legacy conversations model fields.
//!
//! `conversations.{ask,compact,rollup}` used to carry a bare model-name
//! string plus an `ollama_endpoint`, and resolution fabricated an Ollama
//! backend from them. This converts those into explicit `BackendConfig`
//! overrides so the fields can be deleted.
//!
//! Operates on raw YAML text, NOT on the typed `Config`: by the time this
//! runs the struct no longer has the legacy fields, so a typed load would
//! silently drop exactly the values we need to read.

use serde_yaml_ng::{Mapping, Value};

use crate::config::DEFAULT_LOCAL_LLM_MODEL;

const DEFAULT_OLLAMA_ENDPOINT: &str = "http://localhost:11434";

/// One legacy model field and the override field it becomes.
struct StageField {
    legacy_model: &'static str,
    backend_key: &'static str,
}

/// Every (stage, legacy model field → override field) pair. `compact` and
/// `rollup` each own two models sharing one `ollama_endpoint`.
const STAGES: &[(&str, &[StageField])] = &[
    (
        "ask",
        &[StageField {
            legacy_model: "model",
            backend_key: "backend",
        }],
    ),
    (
        "compact",
        &[
            StageField {
                legacy_model: "extractive_model",
                backend_key: "extractive_backend",
            },
            StageField {
                legacy_model: "abstractive_model",
                backend_key: "abstractive_backend",
            },
        ],
    ),
    (
        "rollup",
        &[
            StageField {
                legacy_model: "extractive_model",
                backend_key: "extractive_backend",
            },
            StageField {
                legacy_model: "abstractive_model",
                backend_key: "abstractive_backend",
            },
        ],
    ),
];

/// Convert legacy `conversations.*` model fields into explicit backend
/// overrides. Returns `None` when there was nothing to migrate — including
/// when the input does not parse, so a syntactically broken config is left
/// untouched rather than replaced.
///
/// A stage counts as *untouched* only when its model name AND its stage's
/// `ollama_endpoint` both still hold the shipped defaults. A default model
/// pointed at a custom endpoint (a remote Ollama box) is a deliberate choice
/// and gets pinned, not inherited away.
pub fn migrate_conversations_yaml(text: &str) -> Option<String> {
    let mut root: Value = serde_yaml_ng::from_str(text).ok()?;
    let conversations = root.get_mut("conversations")?.as_mapping_mut()?;

    let mut changed = false;
    for (stage_name, fields) in STAGES {
        let Some(stage) = conversations
            .get_mut(Value::from(*stage_name))
            .and_then(Value::as_mapping_mut)
        else {
            continue;
        };
        if migrate_stage(stage, fields) {
            changed = true;
        }
    }

    changed
        .then(|| serde_yaml_ng::to_string(&root).ok())
        .flatten()
}

/// Returns true when this stage's mapping was modified.
fn migrate_stage(stage: &mut Mapping, fields: &[StageField]) -> bool {
    let endpoint = stage
        .get(Value::from("ollama_endpoint"))
        .and_then(Value::as_str)
        .map(str::to_owned);

    // Nothing legacy present at all → nothing to do.
    let has_legacy = endpoint.is_some()
        || fields
            .iter()
            .any(|f| stage.contains_key(Value::from(f.legacy_model)));
    if !has_legacy {
        return false;
    }

    let endpoint_is_default =
        endpoint.as_deref().unwrap_or(DEFAULT_OLLAMA_ENDPOINT) == DEFAULT_OLLAMA_ENDPOINT;

    for f in fields {
        let model = stage
            .remove(Value::from(f.legacy_model))
            .as_ref()
            .and_then(Value::as_str)
            .map(str::to_owned);

        // An override the user already wrote always wins — never overwrite it.
        if stage.contains_key(Value::from(f.backend_key))
            && !stage[Value::from(f.backend_key)].is_null()
        {
            continue;
        }

        let Some(model) = model else { continue };
        if model == DEFAULT_LOCAL_LLM_MODEL && endpoint_is_default {
            // Untouched: leave the override absent so the stage inherits smart.
            continue;
        }

        let mut backend = Mapping::new();
        backend.insert(Value::from("provider"), Value::from("ollama"));
        backend.insert(Value::from("model"), Value::from(model));
        backend.insert(
            Value::from("endpoint"),
            Value::from(
                endpoint
                    .clone()
                    .unwrap_or_else(|| DEFAULT_OLLAMA_ENDPOINT.to_string()),
            ),
        );
        stage.insert(Value::from(f.backend_key), Value::Mapping(backend));
    }

    stage.remove(Value::from("ollama_endpoint"));
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn untouched_stage_migrates_to_inherit() {
        let yaml = "\
conversations:
  ask:
    model: qwen3.5:4b
    ollama_endpoint: http://localhost:11434
    timeout_secs: 120
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        assert!(
            !out.contains("ollama_endpoint"),
            "legacy endpoint removed: {out}"
        );
        assert!(
            !out.contains("model: qwen3.5:4b"),
            "legacy model removed: {out}"
        );
        assert!(!out.contains("backend:"), "no pin written: {out}");
        assert!(
            out.contains("timeout_secs: 120"),
            "unrelated keys kept: {out}"
        );
    }

    #[test]
    fn customized_model_migrates_to_a_pinned_ollama_backend() {
        let yaml = "\
conversations:
  ask:
    model: llama3:70b
    ollama_endpoint: http://localhost:11434
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let b = &v["conversations"]["ask"]["backend"];
        assert_eq!(b["provider"].as_str(), Some("ollama"));
        assert_eq!(b["model"].as_str(), Some("llama3:70b"));
        assert_eq!(b["endpoint"].as_str(), Some("http://localhost:11434"));
    }

    #[test]
    fn default_model_at_a_custom_endpoint_is_pinned_not_inherited() {
        let yaml = "\
conversations:
  ask:
    model: qwen3.5:4b
    ollama_endpoint: http://box.local:11434
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let b = &v["conversations"]["ask"]["backend"];
        assert_eq!(b["endpoint"].as_str(), Some("http://box.local:11434"));
        assert_eq!(b["model"].as_str(), Some("qwen3.5:4b"));
    }

    #[test]
    fn an_existing_override_is_left_alone_and_its_legacy_siblings_dropped() {
        let yaml = "\
conversations:
  ask:
    model: llama3:70b
    ollama_endpoint: http://localhost:11434
    backend:
      provider: openai
      model: Qwen3.5-4B-MLX-4bit
      endpoint: http://127.0.0.1:8000/v1
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(
            v["conversations"]["ask"]["backend"]["provider"].as_str(),
            Some("openai")
        );
        assert!(v["conversations"]["ask"]["model"].is_null());
        assert!(v["conversations"]["ask"]["ollama_endpoint"].is_null());
    }

    #[test]
    fn compacts_two_models_share_one_endpoint() {
        let yaml = "\
conversations:
  compact:
    extractive_model: llama3:70b
    abstractive_model: qwen3.5:4b
    ollama_endpoint: http://box.local:11434
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        let c = &v["conversations"]["compact"];
        assert_eq!(
            c["extractive_backend"]["model"].as_str(),
            Some("llama3:70b")
        );
        assert_eq!(
            c["extractive_backend"]["endpoint"].as_str(),
            Some("http://box.local:11434")
        );
        // default model name, but the endpoint was customized → still pinned
        assert_eq!(
            c["abstractive_backend"]["endpoint"].as_str(),
            Some("http://box.local:11434")
        );
    }

    #[test]
    fn rollup_migrates_too() {
        let yaml = "\
conversations:
  rollup:
    extractive_model: llama3:70b
    abstractive_model: llama3:70b
    ollama_endpoint: http://localhost:11434
";
        let out = migrate_conversations_yaml(yaml).expect("migrates");
        let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(
            v["conversations"]["rollup"]["extractive_backend"]["model"].as_str(),
            Some("llama3:70b")
        );
        assert_eq!(
            v["conversations"]["rollup"]["abstractive_backend"]["model"].as_str(),
            Some("llama3:70b")
        );
    }

    #[test]
    fn is_idempotent() {
        let yaml = "\
conversations:
  ask:
    model: llama3:70b
    ollama_endpoint: http://localhost:11434
";
        let once = migrate_conversations_yaml(yaml).expect("migrates");
        assert!(
            migrate_conversations_yaml(&once).is_none(),
            "second pass must be a no-op"
        );
    }

    #[test]
    fn a_config_without_legacy_keys_is_untouched() {
        assert!(migrate_conversations_yaml("skills:\n  max_skills_in_prompt: 5\n").is_none());
        assert!(migrate_conversations_yaml("").is_none());
    }

    #[test]
    fn unparseable_yaml_is_left_alone_rather_than_destroyed() {
        assert!(migrate_conversations_yaml("conversations: [unclosed\n").is_none());
    }
}
