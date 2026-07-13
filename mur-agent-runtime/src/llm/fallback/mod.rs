//! Failure-fallback for LLM calls: cooldown circuit-breaker, backoff, and token
//! estimate. See the model-switch spec.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use mur_common::agent::AgentProfile;
use mur_common::config::{ModelSwitchConfig, RetryConfig};
use mur_common::model::{choose_by_difficulty, resolve_model_refs};

use super::{
    BackgroundKind, LlmClient, LlmError, LlmRequest, LlmResponse, RequestIntent, Retryability,
    RichMessage, classify,
};
use crate::telemetry_writer::Event;

/// Max chars kept from the first user message for `Event::Routing::task_summary`
/// — enough to identify the turn in telemetry review without leaking a whole
/// prompt into the local JSONL log.
const ROUTING_SUMMARY_MAX: usize = 200;

/// Factory that builds a concrete `LlmClient` for a given model_ref. Boxed so
/// `FallbackLlmClient` doesn't need a generic parameter per candidate type.
pub type ClientFactory = Box<dyn Fn(&str) -> anyhow::Result<Arc<dyn LlmClient>> + Send + Sync>;

/// How the adapter decides the ordered candidate list for a request.
enum CandidateSource {
    /// Fixed list (unit tests / simple wiring).
    Static(Vec<String>),
    /// Recomputed per request: applies opt-in difficulty routing (which needs
    /// the request's token estimate) then `resolve_model_refs` (priority
    /// per-agent → global). Reuses the pure, tested mur-common fns.
    PerRequest {
        profile: Box<AgentProfile>,
        cfg: Box<ModelSwitchConfig>,
    },
}

/// An `LlmClient` that tries an ordered list of model_refs, advancing on
/// retryable failures (per-candidate backoff retries first), skipping models in
/// cooldown, and returning a fatal error immediately. Drops into the existing
/// `Arc<dyn LlmClient>` slot with no agent-loop change.
pub struct FallbackLlmClient {
    source: CandidateSource,
    factory: ClientFactory,
    retry: RetryConfig,
    cooldown: CooldownMap,
    primary_name: String,
    /// Best-effort routing telemetry sink (Phase B). `None` in every existing
    /// unit test and any client built without `with_telemetry` — recording is
    /// additive, never load-bearing for the generate loop itself.
    telemetry: Option<tokio::sync::mpsc::Sender<Event>>,
    /// Owning agent's name, stamped onto each `Event::Routing`. Empty when
    /// `telemetry` is `None`.
    agent: String,
}

impl FallbackLlmClient {
    pub fn new(candidates: Vec<String>, factory: ClientFactory, retry: RetryConfig) -> Self {
        let primary_name = candidates.first().cloned().unwrap_or_default();
        Self {
            source: CandidateSource::Static(candidates),
            factory,
            retry,
            cooldown: CooldownMap::new(),
            primary_name,
            telemetry: None,
            agent: String::new(),
        }
    }

    /// Routing-aware constructor: candidates are computed per request so the
    /// difficulty heuristic can see the request size.
    pub fn new_routed(
        profile: AgentProfile,
        cfg: ModelSwitchConfig,
        factory: ClientFactory,
        retry: RetryConfig,
    ) -> Self {
        // primary_name for model_name(): the non-routed primary (model_ref/default).
        let primary_name = resolve_model_refs(&profile, &cfg, None)
            .first()
            .cloned()
            .unwrap_or_default();
        Self {
            source: CandidateSource::PerRequest {
                profile: Box::new(profile),
                cfg: Box::new(cfg),
            },
            factory,
            retry,
            cooldown: CooldownMap::new(),
            primary_name,
            telemetry: None,
            agent: String::new(),
        }
    }

    /// Attach the routing-telemetry sink + owning agent name. Optional — a
    /// client built without this call simply never emits `Event::Routing`
    /// (used by every existing fallback/cascade unit test). Wired in
    /// production by `build_provider_runner` (see `supervisor_runner.rs`).
    pub fn with_telemetry(
        mut self,
        tx: tokio::sync::mpsc::Sender<Event>,
        agent: impl Into<String>,
    ) -> Self {
        self.telemetry = Some(tx);
        self.agent = agent.into();
        self
    }

    /// Which mechanism chose the candidate list for this request — recorded
    /// verbatim in `Event::Routing::reason`.
    fn selection_reason(&self, req: &LlmRequest) -> String {
        if req.pin_model_ref.is_some() {
            return "explicit".to_string();
        }
        if let CandidateSource::PerRequest { profile, cfg } = &self.source {
            let smart = profile
                .routing
                .as_ref()
                .and_then(|r| r.smart.clone())
                .unwrap_or_else(|| cfg.smart.clone());
            if matches!(req.intent, RequestIntent::Background(_)) && smart.enabled {
                return "smart-background".to_string();
            }
        }
        "fallback-advance".to_string()
    }

    /// The ordered candidate refs for this request.
    fn candidates_for(&self, req: &LlmRequest) -> Vec<String> {
        // Explicit pin (user re-run) wins over everything: bypasses routing,
        // Smart, and fallback candidate assembly entirely.
        if let Some(p) = &req.pin_model_ref {
            return vec![p.clone()];
        }
        match &self.source {
            CandidateSource::Static(v) => v.clone(),
            CandidateSource::PerRequest { profile, cfg } => {
                // Per-agent routing overrides global; disabled → None → normal
                // model_ref/default primary.
                let routing = profile
                    .routing
                    .clone()
                    .unwrap_or_else(|| cfg.routing.clone());
                let routed = if routing.enabled {
                    choose_by_difficulty(estimate_input_tokens(req), &routing)
                } else {
                    None
                };
                let base = resolve_model_refs(profile, cfg, routed);

                // Smart background: cheap model first, base (primary + chain)
                // behind it (cascade). Per-agent Smart config overrides global.
                let smart = profile
                    .routing
                    .as_ref()
                    .and_then(|r| r.smart.clone())
                    .unwrap_or_else(|| cfg.smart.clone());
                if matches!(req.intent, RequestIntent::Background(_)) && smart.enabled {
                    let primary = base.first().cloned();
                    let cheap = smart
                        .cheap
                        .clone()
                        .or_else(|| autopick_cheap(primary.as_deref()));
                    if let Some(c) = cheap {
                        let mut out = vec![c];
                        for r in base {
                            if !out.contains(&r) {
                                out.push(r);
                            }
                        }
                        return out;
                    }
                }
                base
            }
        }
    }
}

/// Per-call bookkeeping threaded alongside the `Result` so the routing
/// telemetry emitted by `generate()` can see attempts/escalations/the
/// last-tried model_ref even on the failure path (where `LlmResponse` isn't
/// available to read `model` off of).
struct RoutingMeta {
    attempts: u32,
    escalations: u32,
    last_model_ref: String,
    /// True when the final (non-escalated-further) failure was a structural
    /// one (`LlmError::InvalidResponse`) — distinguishes `structural_fail`
    /// from a plain `error` outcome in `Event::Routing`.
    structural_final: bool,
}

impl FallbackLlmClient {
    /// The exact pre-Task-5 fallback/cascade loop, unchanged in behavior —
    /// only the return type grew a `RoutingMeta` sidecar so `generate()`
    /// (below) can emit `Event::Routing` without touching this control flow.
    async fn generate_with_meta(
        &self,
        req: &LlmRequest,
    ) -> (Result<LlmResponse, LlmError>, RoutingMeta) {
        let now = Instant::now();
        let mut last: Option<LlmError> = None;
        let candidates = self.candidates_for(req);

        // Structural-failure escalation (cascade) is allowed ONLY for
        // Background+Smart turns — Interactive InvalidResponse must stay
        // Fatal (Phase-1 security boundary: a malformed response on the
        // user's chosen model must surface, never silently switch models).
        let max_esc: u32 = match &self.source {
            CandidateSource::PerRequest { profile, cfg }
                if matches!(req.intent, RequestIntent::Background(_)) =>
            {
                let smart = profile
                    .routing
                    .as_ref()
                    .and_then(|r| r.smart.clone())
                    .unwrap_or_else(|| cfg.smart.clone());
                if smart.enabled {
                    smart.max_escalations
                } else {
                    0
                }
            }
            _ => 0,
        };
        let mut escalations = 0u32;
        let mut attempts = 0u32;
        let mut last_model_ref = String::new();

        'candidates: for model_ref in &candidates {
            last_model_ref = model_ref.clone();
            if self.cooldown.is_cooling(model_ref, now) {
                continue;
            }
            let client = match (self.factory)(model_ref) {
                Ok(c) => c,
                Err(e) => {
                    last = Some(LlmError::Http(format!("build {model_ref}: {e}")));
                    continue;
                }
            };
            for attempt in 0..=self.retry.max_retries {
                attempts += 1;
                match client.generate(req.clone()).await {
                    Ok(resp) => {
                        let meta = RoutingMeta {
                            attempts,
                            escalations,
                            last_model_ref: model_ref.clone(),
                            structural_final: false,
                        };
                        return (Ok(resp), meta);
                    }
                    Err(e) => match classify(&e) {
                        Retryability::Fatal => {
                            // Structural failure is escalatable ONLY under
                            // Background+Smart, within the per-call cap.
                            let structural = matches!(e, LlmError::InvalidResponse(_));
                            if structural && escalations < max_esc {
                                escalations += 1;
                                tracing::info!(
                                    model_ref,
                                    escalations,
                                    "smart cascade: structural fail, escalating"
                                );
                                last = Some(e);
                                continue 'candidates; // advance to next candidate (the better model)
                            }
                            // Interactive / over-cap / non-structural Fatal → surface (Phase-1 boundary).
                            let meta = RoutingMeta {
                                attempts,
                                escalations,
                                last_model_ref: last_model_ref.clone(),
                                structural_final: structural,
                            };
                            return (Err(e), meta);
                        }
                        Retryability::Retryable => {
                            tracing::info!(model_ref, attempt, error = %e, "llm fallback: retryable failure");
                            if attempt < self.retry.max_retries {
                                tokio::time::sleep(backoff_delay(
                                    attempt,
                                    self.retry.backoff_base_ms,
                                ))
                                .await;
                            } else {
                                let until =
                                    Instant::now() + Duration::from_secs(self.retry.cooldown_secs);
                                self.cooldown.mark(model_ref, until);
                                tracing::info!(
                                    model_ref,
                                    "llm fallback: cooling down, advancing chain"
                                );
                                last = Some(e);
                            }
                        }
                    },
                }
            }
        }
        let err = last.unwrap_or_else(|| LlmError::InvalidResponse("no model candidates".into()));
        let meta = RoutingMeta {
            attempts,
            escalations,
            last_model_ref,
            structural_final: false,
        };
        (Err(err), meta)
    }
}

#[async_trait]
impl LlmClient for FallbackLlmClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let (result, meta) = self.generate_with_meta(&req).await;

        // Best-effort routing telemetry (Phase B training-data feed) — never
        // await, never let a full mpsc channel or missing sink affect the
        // turn. `try_send` + ignored result is the entire contract.
        if let Some(tx) = &self.telemetry {
            let outcome = match (&result, meta.structural_final) {
                (Ok(_), _) if meta.escalations > 0 => "escalated",
                (Ok(_), _) => "ok",
                (Err(_), true) => "structural_fail",
                (Err(_), false) => "error",
            };
            let (model_ref, input_tokens, output_tokens) = match &result {
                Ok(resp) => (resp.model.clone(), resp.input_tokens, resp.output_tokens),
                Err(_) => (meta.last_model_ref.clone(), 0, 0),
            };
            let ev = Event::Routing {
                agent: self.agent.clone(),
                task_id: req.task_id.clone(),
                intent: intent_label(&req.intent),
                model_ref,
                reason: self.selection_reason(&req),
                outcome: outcome.to_string(),
                attempts: meta.attempts,
                escalations: meta.escalations,
                input_tokens,
                output_tokens,
                task_summary: task_summary(&req),
            };
            let _ = tx.try_send(ev);
        }

        result
    }

    fn model_name(&self) -> &str {
        &self.primary_name
    }
}

/// Label for `Event::Routing::intent` — `"interactive"` or
/// `"background/<kind>"`.
fn intent_label(intent: &RequestIntent) -> String {
    match intent {
        RequestIntent::Interactive => "interactive".to_string(),
        RequestIntent::Background(BackgroundKind::Scheduled) => "background/scheduled".to_string(),
        RequestIntent::Background(BackgroundKind::Companion) => "background/companion".to_string(),
        RequestIntent::Background(BackgroundKind::Maintenance) => {
            "background/maintenance".to_string()
        }
    }
}

/// First user-authored text in the request, truncated to
/// `ROUTING_SUMMARY_MAX` chars — enough to identify the turn in the local
/// telemetry log without persisting a whole prompt.
fn task_summary(req: &LlmRequest) -> String {
    let text = req
        .messages
        .iter()
        .find_map(|m| match m {
            RichMessage::Text { role, content } if role == "user" => Some(content.clone()),
            RichMessage::ImageText { role, text, .. } if role == "user" => Some(text.clone()),
            _ => None,
        })
        .unwrap_or_default();
    truncate_chars(&text, ROUTING_SUMMARY_MAX)
}

fn truncate_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        s.chars().take(max).collect()
    }
}

/// Auto-pick a cheap model from the on-disk registry, excluding `primary`.
/// Any failure (no registry path, load error, empty registry) yields `None`
/// so the caller falls through to Phase-1 `base` candidates (fail-expensive,
/// never fail-hard).
fn autopick_cheap(primary: Option<&str>) -> Option<String> {
    let path = mur_common::model::ModelRegistry::default_path().ok()?;
    let reg = mur_common::model::ModelRegistry::load_from(&path).ok()?;
    mur_common::model::pick_cheap_model(&reg, primary)
}

/// In-memory per-model cooldown (circuit-breaker). Process-local; a restart
/// clears it (cooldowns are seconds-scale, so persistence is unnecessary).
#[derive(Default)]
pub struct CooldownMap {
    inner: Mutex<HashMap<String, Instant>>,
}

impl CooldownMap {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(HashMap::new()),
        }
    }

    pub fn mark(&self, model_ref: &str, until: Instant) {
        self.inner
            .lock()
            .unwrap()
            .insert(model_ref.to_string(), until);
    }

    pub fn is_cooling(&self, model_ref: &str, now: Instant) -> bool {
        match self.inner.lock().unwrap().get(model_ref) {
            Some(until) => now < *until,
            None => false,
        }
    }
}

/// Exponential backoff with jitter: `base * 2^attempt + rand[0, base)`.
pub fn backoff_delay(attempt: u32, base_ms: u64) -> Duration {
    let floor = base_ms.saturating_mul(2u64.saturating_pow(attempt));
    let jitter = if base_ms > 0 {
        rand::random::<u64>() % base_ms
    } else {
        0
    };
    Duration::from_millis(floor.saturating_add(jitter))
}

/// Coarse input-token estimate (chars/4) over the request's message texts —
/// enough for a routing heuristic, not billing. `LlmRequest.messages` is a
/// `Vec<RichMessage>` (an enum), so match the text-bearing variants.
pub fn estimate_input_tokens(req: &LlmRequest) -> u32 {
    let chars: usize = req
        .messages
        .iter()
        .map(|m| match m {
            RichMessage::Text { content, .. } => content.len(),
            RichMessage::ImageText { text, .. } => text.len(),
            RichMessage::ToolUse { text, .. } => text.as_deref().map_or(0, str::len),
            RichMessage::ToolResults { .. } => 0,
        })
        .sum();
    (chars / 4).min(u32::MAX as usize) as u32
}

#[cfg(test)]
mod tests;
