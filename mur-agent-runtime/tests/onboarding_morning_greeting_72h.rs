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

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use mur_agent_runtime::companion::{
    Companion,
    clock::{Clock, MockClock},
    notifier::{CompanionMessage, Notifier, NotifyOutcome},
    outbox::{Outbox, OutboxConfig, TickOutcome},
    picker::{BanditState, Picker, TemplateState},
};
use mur_agent_runtime::durable::ledger::Ledger;
use mur_agent_runtime::llm::stub::StubLlm;
use mur_agent_runtime::llm::{LlmClient, LlmError, LlmRequest, LlmResponse, RichMessage, StopReason};
use mur_common::agent::ProactiveConfig;
use mur_common::companion::Situation;
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

// ──────────────────────────────────────────────────────────────────────────────
// M2.2.4 — outbox uses picked template's prompt_seed (with first_memory)
// ──────────────────────────────────────────────────────────────────────────────

/// Capturing LLM stub: records the user-prompt text it receives, returns a
/// fixed clean response. Exists only in this test module so the production
/// `StubLlm` doesn't grow a capture-API surface for one assertion.
struct CapturingLlm {
    captured: Mutex<Vec<String>>,
    response: String,
}

impl CapturingLlm {
    fn new(response: impl Into<String>) -> Self {
        Self {
            captured: Mutex::new(Vec::new()),
            response: response.into(),
        }
    }
    fn captured_user_prompts(&self) -> Vec<String> {
        self.captured.lock().unwrap().clone()
    }
}

#[async_trait]
impl LlmClient for CapturingLlm {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        // Capture the user message content (the prompt the outbox built from
        // prompt_seed). We deliberately DO NOT capture the system message
        // (voice_md) — the assertion is about user-prompt construction.
        if let Some(content) = req.messages.iter().find_map(|m| match m {
            RichMessage::Text { role, content } if role == "user" => Some(content.clone()),
            _ => None,
        }) {
            self.captured.lock().unwrap().push(content);
        }
        Ok(LlmResponse {
            text: self.response.clone(),
            input_tokens: 0,
            output_tokens: 0,
            model: "capturing-stub".to_string(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        })
    }
    fn model_name(&self) -> &str {
        "capturing-stub"
    }
}

/// Notifier that always reports `Delivered` without writing to disk. Avoids
/// having the inbox file path leak into the test fixture; the test only
/// cares about the LLM-side prompt construction.
struct NoopNotifier;

#[async_trait]
impl Notifier for NoopNotifier {
    fn name(&self) -> &'static str {
        "noop-test"
    }
    async fn send(&self, _msg: &CompanionMessage) -> anyhow::Result<NotifyOutcome> {
        Ok(NotifyOutcome::Delivered)
    }
}

/// Build a UTC `DateTime` that converts to the given local-time tuple.
fn local_as_utc(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> chrono::DateTime<Utc> {
    let naive = NaiveDate::from_ymd_opt(y, mo, d)
        .unwrap()
        .and_time(NaiveTime::from_hms_opt(h, mi, s).unwrap());
    chrono::Local
        .from_local_datetime(&naive)
        .single()
        .expect("ambiguous local time in test")
        .with_timezone(&Utc)
}

/// M2.2.4 — previously the outbox built a hardcoded user prompt
/// (`"Compose one short message for situation: …"`) and ignored the picked
/// template's `prompt_seed` entirely. This test pins that the picked
/// `morning_first_memory_*` template's `prompt_seed` is substituted with the
/// `first_memory` text and reaches the LLM.
///
/// The `CapturingLlm` records its inputs, so we can assert that
/// `{{FIRST_MEMORY}}` was expanded BEFORE the LLM call (i.e. it isn't a
/// model hallucination). The picker is seeded with ONLY the day-3+
/// first-memory templates so the picker's WeightedIndex is forced to one of
/// them, regardless of RNG seed.
#[tokio::test]
async fn morning_greeting_prompt_seed_substitutes_first_memory() {
    // ── Arrange ──────────────────────────────────────────────────────────
    let tmp = TempDir::new().unwrap();
    let agent_home = tmp.path();
    let companion_dir = agent_home.join("companion");
    std::fs::create_dir_all(&companion_dir).unwrap();

    // 1. Write relationship.json so the runtime's first_memory loader picks
    //    up "Sunday in Taipei". Companion::new is NOT used here, but we
    //    keep the on-disk shape consistent for fidelity.
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

    // 2. Build the prompt_seeds map directly from the bundled template
    //    text — this is what `Companion::new` would also produce after
    //    M2.2.4's `load_prompt_seeds` runs.
    let mut prompt_seeds: BTreeMap<String, String> = BTreeMap::new();
    prompt_seeds.insert(
        "morning_first_memory_warm_en_001".to_string(),
        // Verbatim from morning_greeting.en-US.yaml.
        "Good morning {{NAME_FOR_USER}} — still thinking about {{FIRST_MEMORY}}.\nHow's that shaping your day? One sentence, warm tone.\n".to_string(),
    );

    // 3. Bandit-state seeded with ONE morning_greeting template so the
    //    WeightedIndex collapses to that single entry. cooldown_days=0 so
    //    we don't fight day-3 hold-back logic in this test.
    let mut bandit = BanditState::default();
    bandit.templates.insert(
        "morning_first_memory_warm_en_001".to_string(),
        TemplateState {
            id: "morning_first_memory_warm_en_001".to_string(),
            situation: Situation::MorningGreeting,
            weight: 1.0,
            last_used_at: None,
            pos_count: 0,
            neg_count: 0,
            dismiss_count: 0,
            cooldown_days: 0,
        },
    );

    // 4. Construct the outbox directly. We pick 7:00 local on a weekday so
    //    `situations::pick_for_hour` weights MorningGreeting non-zero, and
    //    the proactive gate is wide open (no quiet hours, daily_cap=3).
    let base_utc = local_as_utc(2026, 4, 29, 7, 0, 0);
    let clock = Arc::new(MockClock::at(base_utc));
    let ledger = Ledger::open(&companion_dir.join("outbox-ledger")).unwrap();
    // Force MorningGreeting (idx 0). With seed=0 and a single eligible
    // morning template, the WeightedIndex always returns the only entry —
    // so the pick is deterministic regardless of the situation-RNG roll.
    let picker = Picker::with_seed(bandit, 0);
    let proactive = ProactiveConfig {
        enabled: true,
        daily_cap: 3,
        quiet_hours: None,
        ..ProactiveConfig::default()
    };

    let llm = Arc::new(CapturingLlm::new(
        "Good morning, David — what's the day looking like?",
    ));
    let notifier = Arc::new(NoopNotifier);

    let mut outbox = Outbox::with_picker(
        clock.clone() as Arc<dyn Clock>,
        ledger,
        picker,
        proactive,
        OutboxConfig {
            llm: llm.clone(),
            notifier,
            voice_md: "You are a warm companion.".to_string(),
            locale: "en-US".to_string(),
            prompt_seeds,
            name_for_user: "David".to_string(),
            first_memory: Some("Sunday in Taipei".to_string()),
            formality: "casual".to_string(),
            extra_instructions: String::new(),
            relationship: mur_common::companion::Relationship::Friend,
        },
    );

    // ── Act ──────────────────────────────────────────────────────────────
    // Drive ticks until the morning send fires (situations::pick_for_hour
    // also returns ShareQuote ~40% of the time at 06–09 — we may need a
    // second tick, but the seed=0 picker is deterministic so this is
    // bounded). We bail after 3 ticks to keep the test loud on regression.
    let mut sent = false;
    for _ in 0..3 {
        let now_utc = clock.now_utc();
        let now_local = clock.now_local();
        let outcome = outbox.run_tick(now_utc, now_local).await;
        if let TickOutcome::Sent { situation, .. } = outcome
            && situation == Situation::MorningGreeting
        {
            sent = true;
            break;
        }
        clock.advance(Duration::minutes(1));
    }
    assert!(
        sent,
        "expected at least one MorningGreeting Sent within 3 ticks"
    );

    // ── Assert ───────────────────────────────────────────────────────────
    let prompts = llm.captured_user_prompts();
    assert!(
        !prompts.is_empty(),
        "CapturingLlm must have received at least one user prompt"
    );

    // The substituted prompt_seed reached the LLM (NOT the legacy
    // hardcoded "Compose one short message…" line).
    let merged: String = prompts.join("\n---\n");
    assert!(
        merged.contains("Sunday in Taipei"),
        "user prompt sent to LLM must contain substituted {{{{FIRST_MEMORY}}}}: {merged}"
    );
    assert!(
        merged.contains("David"),
        "user prompt must contain substituted {{{{NAME_FOR_USER}}}}: {merged}"
    );
    assert!(
        !merged.contains("{{FIRST_MEMORY}}"),
        "verbatim placeholder must not leak through to the LLM: {merged}"
    );
    assert!(
        !merged.contains("Compose one short message for situation"),
        "legacy hardcoded prompt must not be used when prompt_seed is set: {merged}"
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
