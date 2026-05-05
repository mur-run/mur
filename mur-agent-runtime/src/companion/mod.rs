//! Companion subsystem (Phase 1.1) — see
//! `docs/superpowers/specs/2026-04-29-mur-companion-phase-1-1-design.md`.

pub mod clock;
pub mod earned_permission;
pub mod i18n;
pub mod inbox;
pub mod linter;
pub mod notifier;
pub mod outbox;
pub mod picker;
pub mod schedule;
pub mod situations;
pub mod telemetry;
pub mod voice;

// B0 rule 12 / M8.3 — compile-time audit that no companion file
// imports a direct network client. Test-only; no runtime cost.
#[cfg(test)]
mod network_audit;

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use rand::rngs::StdRng;
use serde::Deserialize;
use tokio::sync::Mutex;

use crate::companion::{
    clock::Clock,
    notifier::StdoutNotifier,
    outbox::{Outbox, OutboxConfig},
    picker::{BanditState, Picker, TemplateId},
    voice::{VoiceInput, compose_with_overrides},
};
use crate::durable::ledger::Ledger;
use crate::llm::LlmClient;
use mur_common::companion::Situation;
use mur_common::companion::content_seed::{SituationFile, all_seeds, parse};

// ──────────────────────────────────────────────────────────────────────────────
// relationship.json — runtime-side parser
// ──────────────────────────────────────────────────────────────────────────────
//
// `mur agent companion init` (mur-core) writes `<agent_dir>/companion/relationship.json`
// during onboarding. The full schema lives there as the user-facing source of
// truth; here we parse only the fields the runtime needs. All fields use
// `#[serde(default)]` so the runtime keeps reading older relationship files
// written before later fields landed.

/// Minimal projection of `relationship.json` that the runtime parses on
/// startup. Forward-compatible: unknown fields are ignored, and every
/// extension this struct cares about is `Option`.
#[derive(Debug, Deserialize, Default)]
struct RelationshipFile {
    /// `first_memory` is added by the M2.1 onboarding wizard once the user
    /// completes the "share one fact" step. Older files may not carry it.
    #[serde(default)]
    first_memory: Option<FirstMemoryRef>,
}

#[derive(Debug, Deserialize)]
struct FirstMemoryRef {
    text: String,
    /// Kept for round-trip / future schema use; the outbox does not consume it.
    #[allow(dead_code)]
    #[serde(default)]
    established_at: Option<chrono::DateTime<chrono::Utc>>,
}

// ──────────────────────────────────────────────────────────────────────────────
// Content-pool seed loader (M2.2.4)
// ──────────────────────────────────────────────────────────────────────────────
//
// Reads `<agent_dir>/companion/content/<situation>.<locale>.yaml` for every
// supported situation, falling back to the embedded seeds when a disk file is
// missing or unreadable. Returns a flat `template_id → prompt_seed` map that
// the outbox uses (via `OutboxConfig.prompt_seeds`) to look up the picked
// template's seed before LLM generation.
//
// Best-effort by design: a missing/malformed YAML is logged at warn level and
// the situation is skipped; the outbox then falls back to the legacy hardcoded
// "Compose one short message…" line for templates whose seed is unknown.

fn load_prompt_seeds(agent_home: &Path, locale: &str) -> BTreeMap<TemplateId, String> {
    let mut out: BTreeMap<TemplateId, String> = BTreeMap::new();
    let situations: [(Situation, &str); 4] = [
        (Situation::MorningGreeting, "morning_greeting"),
        (Situation::GentleCheckIn, "gentle_check_in"),
        (Situation::ShareQuote, "share_quote"),
        (Situation::ShareLink, "share_link"),
    ];

    for (situation, slug) in situations {
        // Try disk first.
        let disk_path = agent_home
            .join("companion/content")
            .join(format!("{slug}.{locale}.yaml"));
        let parsed: Option<SituationFile> = match std::fs::read_to_string(&disk_path) {
            Ok(body) => match serde_yaml_ng::from_str::<SituationFile>(&body) {
                Ok(p) => Some(p),
                Err(e) => {
                    tracing::warn!(
                        "companion: failed to parse content seed {}: {e} — falling back to embedded",
                        disk_path.display()
                    );
                    None
                }
            },
            Err(_) => None,
        };

        // Fall through to embedded if disk missing or malformed.
        let parsed = parsed.or_else(|| {
            for (sit, loc, yaml) in all_seeds() {
                if sit == situation
                    && loc == locale
                    && let Ok(p) = parse(yaml)
                {
                    return Some(p);
                }
            }
            None
        });

        if let Some(file) = parsed {
            for tpl in file.templates {
                out.insert(tpl.id, tpl.prompt_seed);
            }
        }
    }

    out
}

/// Best-effort load of `<agent_dir>/companion/relationship.json`.
///
/// Returns `None` when the file is missing, unreadable, or malformed — voice
/// composition then falls back to "no first memory", which collapses the
/// `{{FIRST_MEMORY}}` / `{{FIRST_MEMORY_PARAGRAPH}}` placeholders to empty
/// strings (see `voice::apply_placeholders`). This keeps Companion startup
/// resilient to a partially-written or absent relationship file.
fn load_relationship_file(agent_home: &Path) -> Option<RelationshipFile> {
    let p = agent_home.join("companion/relationship.json");
    let s = std::fs::read_to_string(&p).ok()?;
    match serde_json::from_str::<RelationshipFile>(&s) {
        Ok(rel) => Some(rel),
        Err(e) => {
            // A malformed relationship.json is real corruption (bad wizard
            // output or hand-edit), not just an absent file — surface as
            // error so the user notices the silent voice degradation.
            tracing::error!(
                "companion: failed to parse {}: {e} — falling back to no first_memory",
                p.display()
            );
            None
        }
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Companion — lightweight handle wired into the supervisor tick loop.
// ──────────────────────────────────────────────────────────────────────────────

/// Supervisor-level handle for the companion outbox.
///
/// Wraps an `Outbox` in an `Arc<Mutex<…>>` so it can be cheaply cloned into
/// a `tokio::spawn`-ed tick task.  When `profile.companion.enabled` is false,
/// `Companion::new` returns `Ok(None)` without allocating any of the heavy
/// subsystems (zero-cost path — Spec Q9).
pub struct Companion {
    inner: Arc<Mutex<Outbox<StdRng>>>,
    clock: Arc<dyn Clock>,
}

impl Clone for Companion {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            clock: self.clock.clone(),
        }
    }
}

impl Companion {
    /// Build a `Companion` from the agent profile.
    ///
    /// Returns `Ok(None)` immediately when `profile.companion.enabled` is false
    /// (zero-cost — no heap allocation beyond the early return).
    pub fn new(
        profile: &mur_common::agent::AgentProfile,
        agent_home: &std::path::Path,
        clock: Arc<dyn Clock>,
        llm: Arc<dyn LlmClient>,
    ) -> anyhow::Result<Option<Self>> {
        if !profile.companion.enabled {
            return Ok(None);
        }

        // 1. Compose voice markdown.
        let formality_str = profile
            .companion
            .voice_overrides
            .formality
            .as_ref()
            .map(|f| format!("{f:?}").to_lowercase())
            .unwrap_or_default();
        let name_for_user = profile
            .companion
            .voice_overrides
            .name_for_user
            .as_deref()
            .unwrap_or("");
        let extra_instructions = profile
            .companion
            .voice_overrides
            .extra_instructions
            .as_deref()
            .unwrap_or("");
        // Load relationship.json so we can thread first_memory.text into the
        // voice template. `rel` must outlive `compose_with_overrides` because
        // `VoiceInput.first_memory: Option<&str>` borrows from it.
        //
        // Lifecycle: voice_md is composed ONCE here at startup and cached on
        // the inner state. `mur agent companion init --re-init` therefore
        // requires a runtime restart for first_memory edits to take effect.
        let rel = load_relationship_file(agent_home);
        let first_memory: Option<&str> = rel
            .as_ref()
            .and_then(|r| r.first_memory.as_ref().map(|fm| fm.text.as_str()));
        let voice_md = compose_with_overrides(
            Some(agent_home),
            None,
            VoiceInput {
                relationship: profile.companion.relationship.clone(),
                locale: &profile.companion.locale,
                name_for_user,
                first_memory,
                formality: &formality_str,
                extra_instructions,
            },
        );

        // 2. Notifier — writes messages to companion/inbox/.
        let inbox_dir = agent_home.join("companion/inbox");
        std::fs::create_dir_all(&inbox_dir)?;
        let notifier: Arc<dyn crate::companion::notifier::Notifier> =
            Arc::new(StdoutNotifier::new(inbox_dir));

        // 3. Ledger.
        let ledger = Ledger::open(&agent_home.join("companion/outbox-ledger"))?;

        // 4. Picker — empty bandit-state for now.
        // TODO(M?.x): load bandit-state from disk (bandit-state.json).
        let picker = Picker::from_state(BanditState::default());

        // 5. OutboxConfig.
        // M2.2.4 — load the content-pool seeds so the outbox can substitute
        // `prompt_seed` placeholders into the LLM user prompt. The first_memory
        // / placeholder context here is OWNED (cloned) because Outbox outlives
        // the function-scoped `rel` we used for voice composition above.
        let prompt_seeds = load_prompt_seeds(agent_home, &profile.companion.locale);
        let first_memory_owned: Option<String> = first_memory.map(|s| s.to_string());
        let config = OutboxConfig {
            llm,
            notifier,
            voice_md,
            locale: profile.companion.locale.clone(),
            prompt_seeds,
            name_for_user: name_for_user.to_string(),
            first_memory: first_memory_owned,
            formality: formality_str,
            extra_instructions: extra_instructions.to_string(),
            relationship: profile.companion.relationship.clone(),
        };

        // 6. Outbox wrapped in Mutex for interior mutability across the tick task.
        let outbox = Outbox::new(
            clock.clone(),
            ledger,
            picker,
            profile.companion.proactive.clone(),
            config,
        );

        Ok(Some(Self {
            inner: Arc::new(Mutex::new(outbox)),
            clock,
        }))
    }

    /// Clone a cheap handle for use in the spawned tick task.
    pub fn clone_handle(&self) -> Self {
        self.clone()
    }

    /// Drive one outbox tick. Called every 60 s by the supervisor loop.
    pub async fn run_tick(&self) {
        let now_utc = self.clock.now_utc();
        let now_local = self.clock.now_local();
        let mut g = self.inner.lock().await;
        let _ = g.run_tick(now_utc, now_local).await;
    }

    /// Test-only: snapshot the composed `voice_md` (system prompt) so
    /// integration tests can assert that placeholders threaded from
    /// `relationship.json` (e.g. `{{FIRST_MEMORY}}`) actually expanded.
    /// Narrow surface — does not expose the inner `Outbox` itself.
    #[doc(hidden)]
    pub async fn voice_md_for_test(&self) -> String {
        self.inner.lock().await.voice_md.clone()
    }
}

// ──────────────────────────────────────────────────────────────────────────────
// Tests
// ──────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::companion::clock::SystemClock;

    #[tokio::test]
    async fn companion_new_returns_none_when_disabled() {
        // Build a minimal AgentProfile with companion.enabled = false (the default).
        let profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(
            r#"
schema: 1
id: 01JQX4TM8Y9K7VQH6B2N3R5DPF
name: test_agent
display_name: "Test"
version: "0.1.0"
persona:
  category: custom
  description: "Test agent"
  traits: { tone: neutral, risk: cautious, verbosity: low }
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
"#,
        )
        .expect("profile YAML must parse");
        // companion.enabled is false by default — no explicit companion block needed.
        assert!(
            !profile.companion.enabled,
            "companion must be disabled by default"
        );

        let tmp = tempfile::TempDir::new().unwrap();
        let clock = Arc::new(SystemClock) as Arc<dyn Clock>;
        let stub_llm: Arc<dyn LlmClient> =
            Arc::new(crate::llm::stub::StubLlm::with_default_scenarios());
        let result = Companion::new(&profile, tmp.path(), clock, stub_llm).unwrap();
        assert!(
            result.is_none(),
            "Companion::new must return None when companion.enabled is false"
        );
    }
}
