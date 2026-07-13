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
    LlmClient, LlmError, LlmRequest, LlmResponse, RequestIntent, Retryability, RichMessage,
    classify,
};

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
        }
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

#[async_trait]
impl LlmClient for FallbackLlmClient {
    async fn generate(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let now = Instant::now();
        let mut last: Option<LlmError> = None;
        let candidates = self.candidates_for(&req);

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

        'candidates: for model_ref in &candidates {
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
                match client.generate(req.clone()).await {
                    Ok(resp) => return Ok(resp),
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
                            return Err(e);
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
        Err(last.unwrap_or_else(|| LlmError::InvalidResponse("no model candidates".into())))
    }

    fn model_name(&self) -> &str {
        &self.primary_name
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
mod tests {
    use super::super::BackgroundKind;
    use super::*;

    #[test]
    fn cooldown_marks_and_expires() {
        let cm = CooldownMap::new();
        let now = Instant::now();
        assert!(!cm.is_cooling("m", now));
        cm.mark("m", now + Duration::from_secs(60));
        assert!(cm.is_cooling("m", now)); // within window
        assert!(!cm.is_cooling("m", now + Duration::from_secs(61))); // after window
        assert!(!cm.is_cooling("other", now));
    }

    #[test]
    fn backoff_grows_and_stays_in_bounds() {
        let base = 500u64;
        for attempt in 0..4u32 {
            let d = backoff_delay(attempt, base).as_millis() as u64;
            let floor = base * 2u64.pow(attempt);
            assert!(
                d >= floor && d < floor + base,
                "attempt {attempt}: {d} not in [{floor}, {})",
                floor + base
            );
        }
    }

    #[test]
    fn estimate_tokens_sums_text_over_rich_messages() {
        let req = LlmRequest {
            messages: vec![
                RichMessage::Text {
                    role: "user".into(),
                    content: "a".repeat(40),
                },
                RichMessage::ImageText {
                    role: "user".into(),
                    media_type: "image/png".into(),
                    data: String::new(),
                    text: "b".repeat(40),
                },
            ],
            temperature: None,
            max_tokens: None,
            tools: vec![],
            ..Default::default()
        };
        assert_eq!(estimate_input_tokens(&req), 20); // 80 chars / 4
    }

    use super::super::StopReason;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // LlmResponse has no Default (StopReason has no default variant), so build
    // one explicitly.
    fn mk_resp(text: &str) -> LlmResponse {
        LlmResponse {
            text: text.to_string(),
            input_tokens: 0,
            output_tokens: 0,
            model: text.to_string(),
            tool_calls: vec![],
            stop_reason: StopReason::EndTurn,
        }
    }

    // Mock client whose Nth generate() outcome is scripted.
    struct ScriptClient {
        name: String,
        outcomes: Vec<Result<(), LlmError>>,
        idx: AtomicUsize,
    }
    #[async_trait]
    impl LlmClient for ScriptClient {
        async fn generate(&self, _req: LlmRequest) -> Result<LlmResponse, LlmError> {
            let i = self
                .idx
                .fetch_add(1, Ordering::SeqCst)
                .min(self.outcomes.len() - 1);
            match &self.outcomes[i] {
                Ok(()) => Ok(mk_resp(&self.name)),
                Err(e) => Err(e.clone()),
            }
        }
        fn model_name(&self) -> &str {
            &self.name
        }
    }

    fn factory_for(scripts: HashMap<String, Vec<Result<(), LlmError>>>) -> ClientFactory {
        Box::new(move |r: &str| {
            let o = scripts.get(r).cloned().unwrap_or_else(|| vec![Ok(())]);
            Ok(Arc::new(ScriptClient {
                name: r.to_string(),
                outcomes: o,
                idx: AtomicUsize::new(0),
            }) as Arc<dyn LlmClient>)
        })
    }

    fn retry0() -> RetryConfig {
        RetryConfig {
            max_retries: 0,
            backoff_base_ms: 1,
            cooldown_secs: 60,
        }
    }

    #[tokio::test]
    async fn advances_chain_on_retryable_then_succeeds() {
        let mut s = HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::ServerError(500))]); // a fails (retryable)
        s.insert("b".into(), vec![Ok(())]); // b succeeds
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let resp = fb.generate(LlmRequest::default()).await.unwrap();
        assert_eq!(resp.text, "b"); // fell through to b
    }

    #[tokio::test]
    async fn fatal_error_does_not_advance() {
        let mut s = HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::Http("401".into()))]); // fatal
        s.insert("b".into(), vec![Ok(())]);
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let err = fb.generate(LlmRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::Http(_))); // returned a's fatal error, never tried b
    }

    #[tokio::test]
    async fn exhaustion_returns_last_error() {
        let mut s = HashMap::new();
        s.insert("a".into(), vec![Err(LlmError::RateLimit)]);
        s.insert("b".into(), vec![Err(LlmError::ServerError(503))]);
        let fb = FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(s), retry0());
        let err = fb.generate(LlmRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::ServerError(503))); // last candidate's error
    }

    #[test]
    fn candidates_pin_overrides_everything() {
        // A pinned ref returns exactly [pinned] regardless of source.
        let fb = FallbackLlmClient::new(
            vec!["a".into(), "b".into()],
            factory_for(Default::default()),
            retry0(),
        );
        let mut req = LlmRequest::default();
        req.pin_model_ref = Some("frontier".into());
        assert_eq!(fb.candidates_for(&req), vec!["frontier".to_string()]);
    }

    #[test]
    fn candidates_smart_background_prepends_cheap() {
        use mur_common::agent::AgentProfile;
        use mur_common::config::{ModelSwitchConfig, SmartConfig};
        let mut cfg = ModelSwitchConfig::default();
        cfg.default = Some("primary".into());
        cfg.fallback_chain = vec!["primary".into(), "mid".into()];
        cfg.smart = SmartConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            max_escalations: 1,
        };
        let fb = FallbackLlmClient::new_routed(
            AgentProfile::default_for_tests(),
            cfg,
            factory_for(Default::default()),
            retry0(),
        );
        // Background + smart → cheap first, then phase-1 candidates, deduped.
        let mut bg = LlmRequest::default();
        bg.intent = RequestIntent::Background(BackgroundKind::Scheduled);
        assert_eq!(
            fb.candidates_for(&bg),
            vec!["cheap".to_string(), "primary".into(), "mid".into()]
        );
        // Interactive → unchanged (no cheap prepend).
        let inter = LlmRequest::default();
        assert_eq!(
            fb.candidates_for(&inter),
            vec!["primary".to_string(), "mid".into()]
        );
    }

    #[tokio::test]
    async fn routed_generate_picks_frontier_for_large_request() {
        use mur_common::agent::AgentProfile;
        use mur_common::config::{ModelSwitchConfig, RoutingConfig};
        let mut cfg = ModelSwitchConfig::default();
        cfg.routing = RoutingConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            frontier: Some("frontier".into()),
            threshold_input_tokens: Some(5),
            smart: None,
        };
        let mut scripts = std::collections::HashMap::new();
        scripts.insert("frontier".to_string(), vec![Ok(())]);
        scripts.insert("cheap".to_string(), vec![Ok(())]);
        let fb = FallbackLlmClient::new_routed(
            AgentProfile::default_for_tests(),
            cfg,
            factory_for(scripts),
            retry0(),
        );
        // A big request (> threshold=5 tokens) routes to frontier.
        let big = LlmRequest {
            messages: vec![RichMessage::Text {
                role: "user".into(),
                content: "x".repeat(400),
            }],
            temperature: None,
            max_tokens: None,
            tools: vec![],
            ..Default::default()
        };
        assert_eq!(fb.generate(big).await.unwrap().text, "frontier");
        // A tiny request routes to cheap.
        let small = LlmRequest {
            messages: vec![RichMessage::Text {
                role: "user".into(),
                content: "x".into(),
            }],
            temperature: None,
            max_tokens: None,
            tools: vec![],
            ..Default::default()
        };
        assert_eq!(fb.generate(small).await.unwrap().text, "cheap");
    }

    #[tokio::test]
    async fn cascade_escalates_structural_fail_under_background_smart() {
        use mur_common::agent::AgentProfile;
        use mur_common::config::{ModelSwitchConfig, SmartConfig};
        // cheap returns InvalidResponse (structural), then primary succeeds.
        let mut scripts = std::collections::HashMap::new();
        scripts.insert(
            "cheap".to_string(),
            vec![Err(LlmError::InvalidResponse("empty".into()))],
        );
        scripts.insert("primary".to_string(), vec![Ok(())]);

        let mut cfg = ModelSwitchConfig::default();
        cfg.default = Some("primary".into());
        cfg.smart = SmartConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            max_escalations: 1,
        };
        let fb = FallbackLlmClient::new_routed(
            AgentProfile::default_for_tests(),
            cfg,
            factory_for(scripts),
            retry0(),
        );

        let mut bg = LlmRequest::default();
        bg.intent = RequestIntent::Background(BackgroundKind::Scheduled);
        assert_eq!(fb.generate(bg).await.unwrap().text, "primary");
    }

    #[tokio::test]
    async fn interactive_invalid_response_stays_fatal() {
        let mut scripts = std::collections::HashMap::new();
        scripts.insert(
            "a".to_string(),
            vec![Err(LlmError::InvalidResponse("x".into()))],
        );
        scripts.insert("b".to_string(), vec![Ok(())]);
        let fb =
            FallbackLlmClient::new(vec!["a".into(), "b".into()], factory_for(scripts), retry0());
        // Interactive: InvalidResponse is Fatal → returns the error, never tries b.
        let err = fb.generate(LlmRequest::default()).await.unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)));
    }

    #[tokio::test]
    async fn cascade_respects_max_escalations() {
        use mur_common::agent::AgentProfile;
        use mur_common::config::{ModelSwitchConfig, SmartConfig};
        // Both cheap AND primary return InvalidResponse; max_escalations=1
        // means only one escalation is allowed, so the second structural
        // failure (on primary) must surface instead of looping past the cap.
        let mut scripts = std::collections::HashMap::new();
        scripts.insert(
            "cheap".to_string(),
            vec![Err(LlmError::InvalidResponse("empty".into()))],
        );
        scripts.insert(
            "primary".to_string(),
            vec![Err(LlmError::InvalidResponse("still empty".into()))],
        );

        let mut cfg = ModelSwitchConfig::default();
        cfg.default = Some("primary".into());
        cfg.smart = SmartConfig {
            enabled: true,
            cheap: Some("cheap".into()),
            max_escalations: 1,
        };
        let fb = FallbackLlmClient::new_routed(
            AgentProfile::default_for_tests(),
            cfg,
            factory_for(scripts),
            retry0(),
        );

        let mut bg = LlmRequest::default();
        bg.intent = RequestIntent::Background(BackgroundKind::Scheduled);
        let err = fb.generate(bg).await.unwrap_err();
        assert!(matches!(err, LlmError::InvalidResponse(_)));
    }
}
