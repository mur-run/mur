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

use std::sync::Arc;

use rand::rngs::StdRng;
use tokio::sync::Mutex;

use crate::companion::{
    clock::Clock,
    notifier::StdoutNotifier,
    outbox::{Outbox, OutboxConfig},
    picker::{BanditState, Picker},
    voice::{VoiceInput, compose_with_overrides},
};
use crate::durable::ledger::Ledger;
use crate::llm::LlmClient;

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
        let voice_md = compose_with_overrides(
            Some(agent_home),
            None,
            VoiceInput {
                relationship: profile.companion.relationship.clone(),
                locale: &profile.companion.locale,
                name_for_user,
                first_memory: None,
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
        let config = OutboxConfig {
            llm,
            notifier,
            voice_md,
            locale: profile.companion.locale.clone(),
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
