//! Shared model-setup recommendation engine.
//!
//! One deterministic policy used by BOTH `mur init` (one-question Step G)
//! and the Hub first-run wizard: cloud smart model when a key is available,
//! local embedding + conversations when a local runtime exists. Pure
//! functions — probing (env/keychain/discovery) happens in the callers'
//! gather helpers so `recommend` is unit-testable.

pub mod slots;

use serde::{Deserialize, Serialize};

use crate::discovery::aggregate::{MenuRowKind, build_embedding_menu, build_llm_menu};
use crate::discovery::{Backend, DiscoveredModel};
use mur_common::config::{BackendConfig, Config};
use mur_common::model::ModelRegistry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeySource {
    pub provider: String,
    pub api_key_ref: String,
    pub base_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotChoice {
    pub provider: String,
    pub model: String,
    pub openai_url: Option<String>,
    pub api_key_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchChoice {
    pub provider: String,
    pub model: String,
    pub dimensions: usize,
    pub openai_url: Option<String>,
    pub api_key_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSetupPlan {
    pub smart: Option<SlotChoice>,
    pub search: Option<SearchChoice>,
    /// Backend for the three conversation stages (ask/compact/rollup).
    /// Deliberately derived from the *local* model rather than `smart`:
    /// conversation stages stay on-device even when a cloud key is present
    /// (see `recommend()`). Carries the real `provider`/`openai_url`/
    /// `api_key_ref` for whichever backend the local model was discovered
    /// on (Ollama vs oMLX) — never just a bare model-name string.
    pub conversations: Option<SlotChoice>,
    pub summary: String,
}

struct CloudDefault {
    key_provider: &'static str,
    cfg_provider: &'static str,
    model: &'static str,
    openai_url: Option<&'static str>,
}
const CLOUD_LLM_DEFAULTS: &[CloudDefault] = &[
    CloudDefault {
        key_provider: "anthropic",
        cfg_provider: "anthropic",
        model: "claude-opus-5",
        openai_url: None,
    },
    CloudDefault {
        key_provider: "openai",
        cfg_provider: "openai",
        model: "gpt-5.4-mini",
        openai_url: None,
    },
    CloudDefault {
        key_provider: "gemini",
        cfg_provider: "gemini",
        model: "gemini-3.5-flash-lite",
        openai_url: None,
    },
    CloudDefault {
        key_provider: "openrouter",
        cfg_provider: "openai",
        model: "google/gemini-3.5-flash-lite",
        openai_url: Some("https://openrouter.ai/api/v1"),
    },
];

const CLOUD_EMBEDDING_DEFAULTS: &[(&str, &str, &str, usize)] = &[
    ("openai", "openai", "text-embedding-3-small", 1536),
    ("gemini", "gemini", "text-embedding-004", 768),
    ("anthropic", "anthropic", "voyage-3-lite", 1024),
];

const ENV_KEY_TABLE: &[(&str, &str)] = &[
    ("anthropic", "ANTHROPIC_API_KEY"),
    ("openai", "OPENAI_API_KEY"),
    ("gemini", "GEMINI_API_KEY"),
    ("openrouter", "OPENROUTER_API_KEY"),
];

pub fn probe_env_keys() -> Vec<KeySource> {
    ENV_KEY_TABLE
        .iter()
        .filter(|(_, var)| std::env::var(var).is_ok_and(|v| !v.is_empty()))
        .map(|(p, var)| KeySource {
            provider: (*p).to_string(),
            api_key_ref: format!("env:{var}"),
            base_url: None,
        })
        .collect()
}

pub fn keychain_key_sources(reg: &ModelRegistry) -> Vec<KeySource> {
    let mut seen = std::collections::BTreeSet::new();
    reg.models
        .values()
        .filter_map(|e| {
            let s = e.secret.as_ref()?;
            if !seen.insert(e.provider.clone()) {
                return None;
            }
            s.resolve_blocking().ok()?;
            Some(KeySource {
                provider: e.provider.clone(),
                api_key_ref: s.to_string(),
                base_url: e.base_url.clone(),
            })
        })
        .collect()
}

fn best_local_llm(discovered: &[DiscoveredModel]) -> Option<DiscoveredModel> {
    build_llm_menu(discovered)
        .into_iter()
        .find(|r| r.kind == MenuRowKind::Auto)
        .and_then(|r| r.model)
}

fn best_local_embedding(discovered: &[DiscoveredModel]) -> Option<DiscoveredModel> {
    build_embedding_menu(discovered)
        .into_iter()
        .find(|r| r.kind == MenuRowKind::Auto)
        .and_then(|r| r.model)
}

fn local_slot_choice(m: &DiscoveredModel) -> SlotChoice {
    match m.backend {
        Backend::Ollama => SlotChoice {
            provider: "ollama".into(),
            model: m.id.clone(),
            openai_url: None,
            api_key_ref: None,
        },
        Backend::OMlx => SlotChoice {
            provider: "openai".into(),
            model: m.id.clone(),
            openai_url: Some("http://localhost:8000/v1".into()),
            api_key_ref: Some("env:OMLX_API_KEY".into()),
        },
    }
}

pub fn recommend(discovered: &[DiscoveredModel], keys: &[KeySource]) -> ModelSetupPlan {
    let cloud = CLOUD_LLM_DEFAULTS.iter().find_map(|d| {
        keys.iter()
            .find(|k| k.provider == d.key_provider)
            .map(|k| (d, k))
    });
    let local_llm = best_local_llm(discovered);
    let local_emb = best_local_embedding(discovered);

    let smart = match (&cloud, &local_llm) {
        (Some((d, k)), _) => Some(SlotChoice {
            provider: d.cfg_provider.into(),
            model: d.model.into(),
            openai_url: d.openai_url.map(String::from),
            api_key_ref: Some(k.api_key_ref.clone()),
        }),
        (None, Some(m)) => Some(local_slot_choice(m)),
        (None, None) => None,
    };

    let search = match &local_emb {
        Some(m) => Some(SearchChoice {
            provider: match m.backend {
                Backend::Ollama => "ollama".into(),
                Backend::OMlx => "omlx".into(),
            },
            model: m.id.clone(),
            dimensions: m.dims.or_else(|| fallback_dims_for(&m.id)).unwrap_or(1024),
            openai_url: match m.backend {
                Backend::Ollama => None,
                Backend::OMlx => Some("http://localhost:8000/v1".into()),
            },
            api_key_ref: None,
        }),
        None => cloud.as_ref().and_then(|(d, k)| {
            CLOUD_EMBEDDING_DEFAULTS
                .iter()
                .find(|(kp, ..)| *kp == d.key_provider)
                .map(|(_, provider, model, dims)| SearchChoice {
                    provider: (*provider).into(),
                    model: (*model).into(),
                    dimensions: *dims,
                    openai_url: None,
                    api_key_ref: Some(k.api_key_ref.clone()),
                })
        }),
    };

    let conversations = local_llm.as_ref().map(local_slot_choice);

    let summary = match (&smart, &search) {
        (None, _) => {
            "no models detected — connect a provider in MUR Hub → Settings → Models".into()
        }
        (Some(s), Some(e)) => format!(
            "{}/{} (smart) + {}/{} (search)",
            s.provider, s.model, e.provider, e.model
        ),
        (Some(s), None) => format!(
            "{}/{} (smart); no embedding runtime found — search stays unconfigured",
            s.provider, s.model
        ),
    };

    ModelSetupPlan {
        smart,
        search,
        conversations,
        summary,
    }
}

pub fn apply(plan: &ModelSetupPlan, config: &mut Config) {
    if let Some(s) = &plan.smart {
        config.llm.provider = s.provider.clone();
        config.llm.model = s.model.clone();
        config.llm.openai_url = s.openai_url.clone();
        config.llm.api_key_ref = s.api_key_ref.clone();
    }
    if let Some(e) = &plan.search {
        config.embedding.provider = e.provider.clone();
        config.embedding.model = e.model.clone();
        config.embedding.dimensions = e.dimensions;
        config.embedding.openai_url = e.openai_url.clone();
        config.embedding.api_key_ref = e.api_key_ref.clone();
    }
    if let Some(c) = &plan.conversations {
        // `conversations` is deliberately independent of `plan.smart` (see
        // `recommend()`: keep the high-volume, latency-sensitive
        // conversations pipeline on the local model even when a cloud key
        // is available for smart) — so this must be an explicit per-stage
        // pin, not a `None`/inherit-smart clear.
        //
        // `c` already carries whichever backend the local model was
        // discovered on (`local_slot_choice()` maps `Backend::Ollama` to
        // "ollama" and `Backend::OMlx` to "openai" + its own endpoint/key
        // ref) — no hardcoded provider here.
        let backend = BackendConfig {
            provider: c.provider.clone(),
            model: c.model.clone(),
            endpoint: c.openai_url.clone(),
            api_key_env: None,
            api_key_ref: c.api_key_ref.clone(),
            timeout_secs: None,
        };
        config.conversations.ask.backend = Some(backend.clone());
        config.conversations.compact.extractive_backend = Some(backend.clone());
        config.conversations.rollup.extractive_backend = Some(backend);
    }
}

pub fn is_factory_default_models(config: &Config) -> bool {
    let d = Config::default();
    // provider/model alone can coincidentally match the shipped defaults
    // (e.g. recommend() picks anthropic/claude-opus-5 for the smart slot,
    // same as Config::default()) — api_key_ref is what actually flips once a
    // key is wired up, so it must gate the "still untouched" check too.
    config.llm.provider == d.llm.provider
        && config.llm.model == d.llm.model
        && config.llm.api_key_ref == d.llm.api_key_ref
        && config.embedding.provider == d.embedding.provider
        && config.embedding.model == d.embedding.model
        && config.embedding.api_key_ref == d.embedding.api_key_ref
}

pub fn fallback_dims_for(id: &str) -> Option<usize> {
    if id.contains("Qwen3-Embedding-0.6B") || id.contains("qwen3-embedding:0.6b") {
        Some(1024)
    } else if id.contains("Qwen3-Embedding-4B") || id.contains("qwen3-embedding:4b") {
        Some(2560)
    } else if id.contains("Qwen3-Embedding-8B") || id.contains("qwen3-embedding:8b") {
        Some(4096)
    } else if id.contains("bge-m3") {
        Some(1024)
    } else if id.contains("nomic-embed-text") || id.contains("embeddinggemma") {
        Some(768)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::{Backend, DiscoveredModel, ModelKind};

    fn local_llm(id: &str) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            backend: Backend::Ollama,
            kind: ModelKind::Llm,
            dims: None,
            family: None,
            size_bytes: None,
            probed_at: None,
        }
    }
    fn local_emb(id: &str, dims: usize) -> DiscoveredModel {
        DiscoveredModel {
            id: id.into(),
            backend: Backend::Ollama,
            kind: ModelKind::Embedding,
            dims: Some(dims),
            family: None,
            size_bytes: None,
            probed_at: None,
        }
    }
    fn anthropic_key() -> KeySource {
        KeySource {
            provider: "anthropic".into(),
            api_key_ref: "keychain:mur/anthropic".into(),
            base_url: None,
        }
    }

    #[test]
    fn cloud_key_plus_local_runtime_is_hybrid() {
        let d = vec![
            local_llm("qwen3.5:4b"),
            local_emb("qwen3-embedding:0.6b", 1024),
        ];
        let plan = recommend(&d, &[anthropic_key()]);
        let smart = plan.smart.unwrap();
        assert_eq!(smart.provider, "anthropic");
        assert_eq!(smart.model, "claude-opus-5");
        assert_eq!(smart.api_key_ref.as_deref(), Some("keychain:mur/anthropic"));
        let search = plan.search.unwrap();
        assert_eq!(search.provider, "ollama");
        assert_eq!(search.dimensions, 1024);
        // Conversations deliberately keep the local runtime even though
        // `smart` went cloud above — and carry a real backend, not a bare
        // model-name string.
        let conversations = plan.conversations.unwrap();
        assert_eq!(conversations.provider, "ollama");
        assert_eq!(conversations.model, "qwen3.5:4b");
    }

    #[test]
    fn no_key_falls_back_to_local_llm() {
        let d = vec![local_llm("qwen3.5:4b")];
        let plan = recommend(&d, &[]);
        let smart = plan.smart.unwrap();
        assert_eq!(smart.provider, "ollama");
        assert_eq!(smart.model, "qwen3.5:4b");
        assert_eq!(smart.api_key_ref, None);
    }

    #[test]
    fn nothing_detected_yields_empty_plan_with_honest_summary() {
        let plan = recommend(&[], &[]);
        assert!(plan.smart.is_none());
        assert!(plan.search.is_none());
        assert!(plan.summary.contains("MUR Hub"));
    }

    #[test]
    fn openrouter_key_maps_to_openai_compat() {
        let plan = recommend(
            &[],
            &[KeySource {
                provider: "openrouter".into(),
                api_key_ref: "env:OPENROUTER_API_KEY".into(),
                base_url: None,
            }],
        );
        let smart = plan.smart.unwrap();
        assert_eq!(smart.provider, "openai");
        assert_eq!(smart.model, "google/gemini-3.5-flash-lite");
        assert_eq!(
            smart.openai_url.as_deref(),
            Some("https://openrouter.ai/api/v1")
        );
    }

    #[test]
    fn apply_writes_all_slots() {
        let d = vec![
            local_llm("qwen3.5:4b"),
            local_emb("qwen3-embedding:0.6b", 1024),
        ];
        let plan = recommend(&d, &[anthropic_key()]);
        let mut cfg = mur_common::config::Config::default();
        apply(&plan, &mut cfg);
        assert_eq!(cfg.llm.provider, "anthropic");
        assert_eq!(
            cfg.llm.api_key_ref.as_deref(),
            Some("keychain:mur/anthropic")
        );
        assert_eq!(cfg.embedding.model, "qwen3-embedding:0.6b");
        assert_eq!(cfg.embedding.dimensions, 1024);
        // `apply()` must write an explicit per-stage `BackendConfig`
        // override — not leave the field `None` to fall through
        // `effective_*` to `smart` (which is "anthropic" in this test) and
        // not a bare model-name string either.
        let b = cfg
            .conversations
            .ask
            .backend
            .as_ref()
            .expect("setup writes an explicit backend, not a bare model name");
        assert_eq!(b.provider, "ollama");
        assert_eq!(b.model, "qwen3.5:4b");
        let compact_b = cfg
            .conversations
            .compact
            .extractive_backend
            .as_ref()
            .expect("compact stage gets an explicit backend too");
        assert_eq!(compact_b.provider, "ollama");
        assert_eq!(compact_b.model, "qwen3.5:4b");
        let rollup_b = cfg
            .conversations
            .rollup
            .extractive_backend
            .as_ref()
            .expect("rollup stage gets an explicit backend too");
        assert_eq!(rollup_b.provider, "ollama");
        assert_eq!(rollup_b.model, "qwen3.5:4b");
        assert!(!is_factory_default_models(&cfg));
    }

    #[test]
    fn an_omlx_local_model_reaches_the_conversation_stages_as_openai() {
        // The exact bug this whole change exists to fix: a local model
        // discovered on the oMLX backend must not surface in the
        // conversation stages paired with the Ollama runtime it never ran
        // on (that pairing would silently send every request to
        // `localhost:11434` for a model that endpoint doesn't have).
        let discovered = vec![DiscoveredModel {
            id: "Qwen3.5-4B-MLX-4bit".into(),
            backend: Backend::OMlx,
            kind: ModelKind::Llm,
            dims: None,
            family: None,
            size_bytes: None,
            probed_at: None,
        }];
        let plan = recommend(&discovered, &[]);
        let mut cfg = mur_common::config::Config::default();
        apply(&plan, &mut cfg);
        let b = cfg
            .conversations
            .ask
            .backend
            .expect("apply() writes an explicit backend for the omlx model");
        assert_eq!(
            b.provider, "openai",
            "omlx must not be left as an Ollama name"
        );
        assert_eq!(b.model, "Qwen3.5-4B-MLX-4bit");
        assert_eq!(b.endpoint.as_deref(), Some("http://localhost:8000/v1"));
        assert_eq!(b.api_key_ref.as_deref(), Some("env:OMLX_API_KEY"));
    }

    #[test]
    fn factory_default_predicate() {
        assert!(is_factory_default_models(
            &mur_common::config::Config::default()
        ));
    }
}
