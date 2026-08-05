//! E2E: drives cmd_generate with a ScriptedLlm for a fixed JSONL fixture.

use mur_common::error::LlmError;
use mur_common::llm::LlmClient;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

struct ScriptedLlm {
    script: Mutex<VecDeque<String>>,
}

impl LlmClient for ScriptedLlm {
    fn complete(
        &self,
        _prompt: &str,
        _system: Option<&str>,
    ) -> impl std::future::Future<Output = Result<String, LlmError>> + Send {
        let resp = self.script.lock().unwrap().pop_front().unwrap_or_default();
        async move { Ok(resp) }
    }

    async fn embed(&self, _text: &str) -> Result<Vec<f32>, LlmError> {
        Ok(vec![])
    }
}

fn make_script() -> Vec<String> {
    let success_patch = r#"{"abstract_hint":"find product prices","procedure_steps":[{"description":"open product page","tool":"browser.navigate"},{"description":"extract price","tool":"browser.extract"}],"triggers":[{"kind":"command","pattern":"/find-price"}],"variables":[{"name":"product","type":"string","required":true}],"notes":[]}"#;
    let error_patch = "THOUGHT: missing selectors. ACTION: done. PATCH: {\"abstract_hint\":null,\"procedure_steps\":[],\"triggers\":[],\"variables\":[{\"name\":\"region\",\"type\":\"string\",\"required\":false}],\"notes\":[\"add region-detect step\"]}";
    let consolidator_yaml = r#"
name: find-price
version: 0.1.0
publisher: agent:generator
description: find product prices
category: workflow
content:
  abstract: Searches product prices online.
  procedure:
    variables:
      - name: product
        type: string
        required: true
      - name: region
        type: string
        required: false
    steps:
      - description: open product page
        tool: browser.navigate
      - description: extract price text
        tool: browser.extract
triggers:
  - type: command
    pattern: /find-price
"#;
    vec![
        success_patch.into(),
        success_patch.into(),
        error_patch.into(),
        consolidator_yaml.into(),
    ]
}

#[tokio::test]
async fn generates_skill_from_fixture() {
    let home = tempfile::tempdir().unwrap();

    let rec_dir = home.path().join("session").join("recordings");
    std::fs::create_dir_all(&rec_dir).unwrap();
    let fixture = include_str!("fixtures/skill_gen/sample_session.jsonl");
    std::fs::write(rec_dir.join("test-sess.jsonl"), fixture).unwrap();

    let llm = Arc::new(ScriptedLlm {
        script: Mutex::new(make_script().into()),
    });

    let manifest = mur_core::cmd::skill_generate::cmd_generate(
        home.path(),
        llm,
        mur_core::cmd::skill_generate::GenerateOptions {
            session_id: "test-sess".into(),
            name: None,
            model_override: None,
            dry_run: false,
            max_parallel: 2,
        },
    )
    .await
    .unwrap();

    assert_eq!(manifest.name, "find-price");
    assert_eq!(manifest.version, "0.1.0");
    assert!(
        home.path()
            .join("skills")
            .join("find-price")
            .join("skill.yaml")
            .exists()
    );

    let trust = mur_common::trust::skills::SkillTrustStore::load(home.path()).unwrap();
    let entry = trust
        .entries
        .values()
        .find(|e| e.name == "find-price")
        .expect("trust entry");
    assert!(matches!(
        entry.level,
        mur_common::skill::TrustLevel::Sandboxed
    ));
}

#[tokio::test]
async fn missing_session_returns_error() {
    let home = tempfile::tempdir().unwrap();
    let llm: Arc<ScriptedLlm> = Arc::new(ScriptedLlm {
        script: Mutex::new(Default::default()),
    });
    let err = mur_core::cmd::skill_generate::cmd_generate(
        home.path(),
        llm,
        mur_core::cmd::skill_generate::GenerateOptions {
            session_id: "nonexistent".into(),
            name: None,
            model_override: None,
            dry_run: true,
            max_parallel: 2,
        },
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("no recording"));
}

#[tokio::test]
async fn dry_run_does_not_write_to_disk() {
    let home = tempfile::tempdir().unwrap();

    let rec_dir = home.path().join("session").join("recordings");
    std::fs::create_dir_all(&rec_dir).unwrap();
    let fixture = include_str!("fixtures/skill_gen/sample_session.jsonl");
    std::fs::write(rec_dir.join("dry-sess.jsonl"), fixture).unwrap();

    let llm = Arc::new(ScriptedLlm {
        script: Mutex::new(make_script().into()),
    });

    let manifest = mur_core::cmd::skill_generate::cmd_generate(
        home.path(),
        llm,
        mur_core::cmd::skill_generate::GenerateOptions {
            session_id: "dry-sess".into(),
            name: None,
            model_override: None,
            dry_run: true,
            max_parallel: 2,
        },
    )
    .await
    .unwrap();

    assert_eq!(manifest.name, "find-price");
    // Dry run must NOT write to disk.
    assert!(
        !home
            .path()
            .join("skills")
            .join("find-price")
            .join("skill.yaml")
            .exists()
    );
    // No trust entry either.
    let trust = mur_common::trust::skills::SkillTrustStore::load(home.path()).unwrap();
    assert!(trust.entries.is_empty());
}
