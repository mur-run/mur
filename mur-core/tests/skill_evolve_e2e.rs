//! E2E: drives evolve_skill with a ScriptedChatBackend over synthetic telemetry.

use async_trait::async_trait;
use mur_common::skill::{global_skill_dir, read_from_dir, write_to_dir};
use mur_core::conversations::backend::{ChatBackend, ChatRequest, ChatResponse, ChatStream, Usage};
use mur_core::evolve::skill_evolve::evolve_skill;
use std::sync::Mutex;

struct ScriptedChatBackend {
    responses: Mutex<Vec<String>>,
}

impl ScriptedChatBackend {
    fn new(responses: Vec<String>) -> Self {
        Self {
            responses: Mutex::new(responses),
        }
    }
}

#[async_trait]
impl ChatBackend for ScriptedChatBackend {
    async fn generate(&self, _req: ChatRequest<'_>) -> anyhow::Result<ChatResponse> {
        let text = self.responses.lock().unwrap().remove(0);
        Ok(ChatResponse {
            text,
            usage: Usage {
                input_tokens: 100,
                output_tokens: 200,
                cache_creation_input_tokens: 0,
                cache_read_input_tokens: 0,
                provider: "test",
                model: "test-model".into(),
            },
        })
    }

    async fn generate_stream(&self, _req: ChatRequest<'_>) -> anyhow::Result<ChatStream> {
        unimplemented!("streaming not used in tests")
    }

    fn provider_name(&self) -> &'static str {
        "test"
    }
}

/// Build a valid minimal skill that round-trips through parse_canonical.
fn minimal_skill_yaml(name: &str, version: &str, extra_tool: &str) -> String {
    format!(
        r#"
name: {name}
version: {version}
publisher: human:test
description: test skill
category: context
hosts: [mur-agent]
content:
  abstract: test
  context: "Use {extra_tool}."
"#
    )
}

fn write_telemetry_fixture(telemetry_dir: &std::path::Path, skill_name: &str) {
    std::fs::create_dir_all(telemetry_dir).unwrap();
    let lines = [
        format!(
            r#"{{"mur.event.type":"telemetry/llm_call","mur.task.id":"t1","mur.fired_skills":["{skill_name}"],"gen_ai.usage.input_tokens":100,"gen_ai.request.model":"claude","latency_ms":1200}}"#
        ),
        r#"{"mur.event.type":"telemetry/tool_call","mur.task.id":"t1","tool":"wrong.tool","ok":false,"duration_ms":500}"#
            .to_string(),
        r#"{"mur.event.type":"telemetry/error","mur.task.id":"t1","kind":"ToolError","message":"tool 'wrong.tool' not found"}"#
            .to_string(),
    ];
    std::fs::write(telemetry_dir.join("2026-05-25.jsonl"), lines.join("\n")).unwrap();
}

#[tokio::test]
async fn evolve_writes_new_version_and_log_entry() {
    let home = tempfile::tempdir().unwrap();

    // 1. Write a skill with a known tool flaw.
    let skill_yaml = minimal_skill_yaml("broken-skill", "0.1.0", "wrong.tool");
    let manifest = mur_common::skill::parse_canonical(&skill_yaml).unwrap();
    let skill_dir = global_skill_dir(home.path(), "broken-skill");
    write_to_dir(&skill_dir, &manifest).unwrap();

    // 2. Write telemetry with 1 failure.
    let telemetry_dir = home.path().join("agents").join("test").join("telemetry");
    write_telemetry_fixture(&telemetry_dir, "broken-skill");

    // 3. Scripted LLM: diagnosis then optimized YAML.
    let responses = vec![
        r#"[{"dimension":"Tool","severity":0.9,"finding":"wrong.tool not found","suggested_fix":"replace wrong.tool with correct.tool","evidence":["t1"]}]"#.to_string(),
        format!(
            "name: broken-skill\nversion: 0.1.1\npublisher: human:test\ndescription: test skill\ncategory: context\nhosts:\n  - mur-agent\ncontent:\n  abstract: test\n  context: \"Use correct.tool.\"\nevolution_log:\n  - version: 0.1.1\n    generation: 1\n    source: agent:evolver\n    changes: replace wrong.tool with correct.tool\n    quality_score: 0.0\n    timestamp: 2026-05-25T00:00:00Z\n"
        ),
    ];
    let llm = ScriptedChatBackend::new(responses);

    // 4. Run evolve.
    let result = evolve_skill(home.path(), "test", "broken-skill", &llm, 1, false)
        .await
        .unwrap();
    assert_eq!(result.new_version, "0.1.1");

    // 5. Verify skill on disk has evolution_log entry.
    let evolved = read_from_dir(&skill_dir).unwrap();
    assert_eq!(evolved.evolution_log.len(), 1);
    assert_eq!(evolved.evolution_log[0].source, "agent:evolver");
    assert_eq!(evolved.evolution_log[0].generation, 1);
}

#[tokio::test]
async fn no_telemetry_returns_clean() {
    let home = tempfile::tempdir().unwrap();

    let skill_yaml = minimal_skill_yaml("no-data", "0.1.0", "some.tool");
    let manifest = mur_common::skill::parse_canonical(&skill_yaml).unwrap();
    let skill_dir = global_skill_dir(home.path(), "no-data");
    write_to_dir(&skill_dir, &manifest).unwrap();

    // No telemetry directory at all.
    let llm = ScriptedChatBackend::new(vec![]);

    let result = evolve_skill(home.path(), "test", "no-data", &llm, 3, false)
        .await
        .unwrap();
    assert_eq!(result.new_version, result.original_version);
    assert!(result.diagnoses.is_empty());
}

#[tokio::test]
async fn dry_run_does_not_write() {
    let home = tempfile::tempdir().unwrap();

    let skill_yaml = minimal_skill_yaml("dry-run-skill", "0.1.0", "bad.tool");
    let manifest = mur_common::skill::parse_canonical(&skill_yaml).unwrap();
    let skill_dir = global_skill_dir(home.path(), "dry-run-skill");
    write_to_dir(&skill_dir, &manifest).unwrap();

    let telemetry_dir = home.path().join("agents").join("test").join("telemetry");
    write_telemetry_fixture(&telemetry_dir, "dry-run-skill");

    let responses = vec![
        r#"[{"dimension":"Tool","severity":0.9,"finding":"bad.tool not found","suggested_fix":"replace bad.tool with good.tool","evidence":["t1"]}]"#.to_string(),
    ];
    let llm = ScriptedChatBackend::new(responses);

    let result = evolve_skill(home.path(), "test", "dry-run-skill", &llm, 3, true)
        .await
        .unwrap();

    // Dry run should not change version.
    assert_eq!(result.new_version, result.original_version);

    // Skill on disk should be unchanged.
    let disk = read_from_dir(&skill_dir).unwrap();
    assert_eq!(disk.version, "0.1.0");
    assert!(disk.evolution_log.is_empty());
}
