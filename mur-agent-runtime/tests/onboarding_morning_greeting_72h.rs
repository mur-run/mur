//! M2.2.3 — Outbox loads `first_memory` from `relationship.json` on startup
//! and threads it into the composed `voice_md` (system prompt).
//!
//! ## Why this test exists
//!
//! M2.1.x adds `first_memory: { text, established_at }` to the on-disk
//! `<agent_dir>/companion/relationship.json` written by the onboarding wizard.
//! The runtime needs to (a) parse that field and (b) feed it into
//! `VoiceInput.first_memory` so the `{{FIRST_MEMORY}}` /
//! `{{FIRST_MEMORY_PARAGRAPH}}` placeholders in any disk-override voice
//! template expand to the user's actual first-memory phrase.
//!
//! ## Test scope (focused integration)
//!
//! Driving `Companion::new` end-to-end exercises the exact code path that
//! ships in production:
//!   1. Read `relationship.json` from the agent dir.
//!   2. Pull `first_memory.text`.
//!   3. Pass it into `VoiceInput`.
//!   4. Run it through `compose_with_overrides`, which walks the agent →
//!      user → embedded template chain and substitutes placeholders.
//!
//! We use a per-agent disk override that contains `{{FIRST_MEMORY}}` because
//! the embedded voice templates intentionally don't reference the placeholder
//! today (M2.2.2's day-3+ first-memory templates live in the *content pool*
//! YAMLs that drive the proactive picker — outbox doesn't load those into the
//! user prompt yet, so the system-prompt path is the cleanest assertion
//! surface for M2.2.3's wiring fix).
//!
//! Driving the full outbox tick to assert on the rendered *message body*
//! would also require wiring template `prompt_seed` substitution into the
//! generation step — that's out of scope for M2.2.3 and tracked separately.
//! The simplification here keeps the test honest about what M2.2.3 changes:
//! "Companion startup loads first_memory and threads it into voice
//! composition."

use std::sync::Arc;

use chrono::{Duration, Utc};
use mur_agent_runtime::companion::{
    Companion,
    clock::{Clock, MockClock},
};
use mur_agent_runtime::llm::LlmClient;
use mur_agent_runtime::llm::stub::StubLlm;
use serde_json::json;
use tempfile::TempDir;

const PROFILE_YAML: &str = r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: darwin
display_name: "Darwin"
version: "0.1.0"
persona:
  category: custom
  description: "Test agent"
  traits: { tone: warm, risk: cautious, verbosity: low }
sys_prompt_file: "sys_prompt.md"
model: { provider: ollama, name: "llama3.2:3b", params: { temperature: 0.2, max_tokens: 4096 } }
mcp_servers: []
skills: []
transport:
  stdio: true
  socket: { enabled: false, bind: "" }
communication: { accepts_from: ["*"], sends_to: [] }
capabilities: []
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: ["tcp"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [rate_limit, timeout, connection_error] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: false }
created_at: "2026-04-29T10:00:00+00:00"
updated_at: "2026-04-29T10:00:00+00:00"
companion:
  enabled: true
  locale: "en-US"
  relationship: friend
  voice_overrides:
    name_for_user: "David"
    formality: casual
"#;

#[tokio::test]
async fn companion_threads_first_memory_from_relationship_json_into_voice_md() {
    // ── Arrange ──────────────────────────────────────────────────────────
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path();

    // 1. Write relationship.json with first_memory and onboarded_at = now - 72h.
    //    The 72h offset matches M2.2's "day-3+" gating intent for the picker
    //    side; the voice composition path itself is time-agnostic, but we
    //    keep the realistic timestamp so the fixture stays meaningful for
    //    follow-up picker-day-gating work.
    let companion_dir = agent_home.join("companion");
    std::fs::create_dir_all(&companion_dir).unwrap();
    let onboarded_at = Utc::now() - Duration::hours(72);
    let rel_json = json!({
        "version": 1,
        "name_for_user": "David",
        "relationship": "friend",
        "locale": "en-US",
        "formality": "casual",
        "extra_instructions": "",
        "onboarded_at": onboarded_at.to_rfc3339(),
        "first_memory": {
            "text": "Sunday in Taipei",
            "established_at": onboarded_at.to_rfc3339(),
        },
    });
    std::fs::write(
        companion_dir.join("relationship.json"),
        serde_json::to_vec_pretty(&rel_json).unwrap(),
    )
    .unwrap();

    // 2. Per-agent disk-override voice template that exercises both
    //    placeholder forms. compose_with_overrides reads this from
    //    `<agent_dir>/companion/templates/<relationship>.<locale>.md`.
    let templates_dir = companion_dir.join("templates");
    std::fs::create_dir_all(&templates_dir).unwrap();
    std::fs::write(
        templates_dir.join("friend.en-US.md"),
        "Hi {{NAME_FOR_USER}}.{{FIRST_MEMORY_PARAGRAPH}}\nMemory: {{FIRST_MEMORY}}\n",
    )
    .unwrap();

    // 3. Profile with companion.enabled = true.
    let profile: mur_common::agent::AgentProfile =
        serde_yaml_ng::from_str(PROFILE_YAML).expect("profile YAML must parse");
    assert!(
        profile.companion.enabled,
        "fixture must enable companion mode"
    );

    // 4. Stub LLM + mock clock — we never tick, so neither runs, but
    //    Companion::new requires both.
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(Utc::now()));
    let llm: Arc<dyn LlmClient> = Arc::new(StubLlm::with_default_scenarios());

    // ── Act ──────────────────────────────────────────────────────────────
    let companion = Companion::new(&profile, agent_home, clock, llm)
        .expect("Companion::new must succeed")
        .expect("Companion::new must return Some when enabled");

    // ── Assert ───────────────────────────────────────────────────────────
    let voice_md = companion.voice_md_for_test().await;

    // Both placeholder forms must have been substituted with the
    // first_memory.text loaded from relationship.json.
    assert!(
        voice_md.contains("Sunday in Taipei"),
        "voice_md must contain first_memory text; got: {voice_md}"
    );
    assert!(
        !voice_md.contains("{{FIRST_MEMORY}}"),
        "verbatim placeholder must not leak into voice_md; got: {voice_md}"
    );
    assert!(
        !voice_md.contains("{{FIRST_MEMORY_PARAGRAPH}}"),
        "paragraph placeholder must not leak into voice_md; got: {voice_md}"
    );
    // Sanity: the unrelated NAME_FOR_USER substitution still works.
    assert!(
        voice_md.contains("David"),
        "voice_md must contain name_for_user; got: {voice_md}"
    );
}

/// Regression guard: if `relationship.json` is missing entirely (the legacy
/// pre-M2.1 case), Companion::new must still succeed — voice composition
/// silently collapses the placeholders to empty strings.
#[tokio::test]
async fn companion_handles_missing_relationship_json() {
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path();

    // Per-agent override with the placeholder, but no relationship.json.
    let templates_dir = agent_home.join("companion/templates");
    std::fs::create_dir_all(&templates_dir).unwrap();
    std::fs::write(
        templates_dir.join("friend.en-US.md"),
        "Hi {{NAME_FOR_USER}}.{{FIRST_MEMORY_PARAGRAPH}}\nMemory: {{FIRST_MEMORY}}\n",
    )
    .unwrap();

    let profile: mur_common::agent::AgentProfile =
        serde_yaml_ng::from_str(PROFILE_YAML).expect("profile YAML must parse");
    let clock: Arc<dyn Clock> = Arc::new(MockClock::at(Utc::now()));
    let llm: Arc<dyn LlmClient> = Arc::new(StubLlm::with_default_scenarios());

    let companion = Companion::new(&profile, agent_home, clock, llm)
        .expect("Companion::new must succeed even without relationship.json")
        .expect("Companion::new must return Some when enabled");

    let voice_md = companion.voice_md_for_test().await;
    // Both placeholders collapse to empty when first_memory is unset.
    assert!(!voice_md.contains("{{FIRST_MEMORY}}"));
    assert!(!voice_md.contains("{{FIRST_MEMORY_PARAGRAPH}}"));
    // The literal "Memory: " line is preserved (the placeholder following it
    // collapsed to "" — the "Memory: " prose stays).
    assert!(voice_md.contains("Memory: "));
    // Hi David. — paragraph form collapsed to nothing → no leading space leak.
    assert!(voice_md.contains("Hi David."));
}
