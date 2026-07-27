//! Model-slot get/set for the Hub Settings one-pager and the wizard.
//!
//! A "slot" is one of the eight model-consuming stages. `smart` (chat/answer
//! model, `llm.*`) and `search` (embeddings) are the two primary slots users
//! pick directly; `ask`/`compact`/`rollup` conversation stages default to
//! following `smart` and can be pinned independently via a per-stage backend
//! override; `summarize`/`reflector`/`curator` are secondary slots.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::store::config::{load_config, save_config};
use mur_common::config::{BackendConfig, Config};
use mur_common::model::{ModelRegistry, RoleEntry};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SlotId {
    Smart,
    Search,
    Ask,
    Compact,
    Rollup,
    Summarize,
    Reflector,
    Curator,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SlotSelection {
    /// Pick a registry model by ref name — secret ref comes from the entry.
    Registry { ref_name: String },
    /// Pick a detected local model.
    Local {
        provider: String,
        model: String,
        base_url: String,
        dims: Option<usize>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlotView {
    pub provider: String,
    pub model: String,
    pub api_key_ref: Option<String>,
    /// "ready" | "key_missing" | "unset"
    pub health: String,
    /// True when this sub-slot mirrors the smart slot (value equality).
    pub follows_smart: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelSlotsView {
    pub smart: SlotView,
    pub search: SlotView,
    pub ask: SlotView,
    pub compact: SlotView,
    pub rollup: SlotView,
    pub summarize: Option<String>,
    pub reflector: Option<String>,
    pub curator: Option<String>,
}

fn ask_pair(cfg: &Config) -> (String, String) {
    match &cfg.conversations.ask.backend {
        Some(b) => (b.provider.clone(), b.model.clone()),
        None => ("ollama".into(), cfg.conversations.ask.model.clone()),
    }
}

fn compact_pair(cfg: &Config) -> (String, String) {
    match &cfg.conversations.compact.extractive_backend {
        Some(b) => (b.provider.clone(), b.model.clone()),
        None => (
            "ollama".into(),
            cfg.conversations.compact.extractive_model.clone(),
        ),
    }
}

fn rollup_pair(cfg: &Config) -> (String, String) {
    (
        "ollama".into(),
        cfg.conversations.rollup.extractive_model.clone(),
    )
}

fn health_for(provider: &str, api_key_ref: Option<&str>) -> String {
    if provider == "ollama" {
        return "ready".into();
    }
    // ponytail: this is the only place the lib's test binary can reach the
    // real login keychain. `MUR_HOME` is mutated by ~45 unsynchronized
    // set/remove pairs across this crate's tests behind 5 different mutexes;
    // losing that race makes `get_slots()` read the developer's actual
    // ~/.mur/config.yaml and pops a macOS password prompt per keychain: ref.
    // Compiles to `false` in release. Upgrade path: one crate-wide env lock.
    if cfg!(test) {
        return "unset".into();
    }
    match api_key_ref {
        None => "unset".into(),
        Some(r) => match r.parse::<mur_common::secret::SecretRef>() {
            Ok(s) => match s.resolve_blocking() {
                Ok(_) => "ready".into(),
                Err(_) => "key_missing".into(),
            },
            Err(_) => "key_missing".into(),
        },
    }
}

fn slot_view(
    provider: String,
    model: String,
    api_key_ref: Option<String>,
    smart: &(String, String),
) -> SlotView {
    let health = health_for(&provider, api_key_ref.as_deref());
    let follows_smart = (&provider, &model) == (&smart.0, &smart.1);
    SlotView {
        provider,
        model,
        api_key_ref,
        health,
        follows_smart,
    }
}

pub fn get_slots() -> Result<ModelSlotsView> {
    let cfg = load_config()?;
    let smart_pair = (cfg.llm.provider.clone(), cfg.llm.model.clone());

    let smart = slot_view(
        cfg.llm.provider.clone(),
        cfg.llm.model.clone(),
        cfg.llm.api_key_ref.clone(),
        &smart_pair,
    );
    let search = slot_view(
        cfg.embedding.provider.clone(),
        cfg.embedding.model.clone(),
        cfg.embedding.api_key_ref.clone(),
        &smart_pair,
    );
    let (ap, am) = ask_pair(&cfg);
    let ask = slot_view(
        ap,
        am,
        cfg.conversations
            .ask
            .backend
            .as_ref()
            .and_then(|b| b.api_key_ref.clone()),
        &smart_pair,
    );
    let (cp, cm) = compact_pair(&cfg);
    let compact = slot_view(
        cp,
        cm,
        cfg.conversations
            .compact
            .extractive_backend
            .as_ref()
            .and_then(|b| b.api_key_ref.clone()),
        &smart_pair,
    );
    let (rp, rm) = rollup_pair(&cfg);
    let rollup = slot_view(rp, rm, None, &smart_pair);

    let reg = ModelRegistry::load_from(&ModelRegistry::default_path()?).unwrap_or_default();

    Ok(ModelSlotsView {
        smart,
        search,
        ask,
        compact,
        rollup,
        summarize: cfg.conversations.ask.summarize_model.clone(),
        reflector: reg.roles.get("reflector").map(|r| r.primary.clone()),
        curator: reg.roles.get("curator").map(|r| r.primary.clone()),
    })
}

/// A selection resolved down to a concrete (provider, model, endpoint, api_key_ref).
struct Resolved {
    provider: String,
    model: String,
    endpoint: Option<String>,
    api_key_ref: Option<String>,
}

fn resolve_selection(sel: &SlotSelection, reg: &ModelRegistry) -> Result<Resolved> {
    match sel {
        SlotSelection::Registry { ref_name } => {
            let entry = reg
                .models
                .get(ref_name)
                .ok_or_else(|| anyhow::anyhow!("no such model in registry: {ref_name}"))?;
            Ok(Resolved {
                provider: entry.provider.clone(),
                model: entry.model.clone(),
                endpoint: entry.base_url.clone(),
                api_key_ref: entry.secret.as_ref().map(|s| s.to_string()),
            })
        }
        SlotSelection::Local {
            provider,
            model,
            base_url,
            ..
        } => Ok(Resolved {
            provider: provider.clone(),
            model: model.clone(),
            endpoint: Some(base_url.clone()),
            api_key_ref: None,
        }),
    }
}

/// Writes a resolved selection into an Ask/Compact per-stage backend override
/// (Local clears the override back to the legacy string field; Registry sets
/// an explicit override), or into Rollup's legacy field directly (Local only).
fn write_conversation_stage(
    cfg: &mut Config,
    id: SlotId,
    sel: &SlotSelection,
    r: &Resolved,
) -> Result<()> {
    match id {
        SlotId::Ask => match sel {
            SlotSelection::Local { .. } => {
                cfg.conversations.ask.backend = None;
                cfg.conversations.ask.model = r.model.clone();
            }
            SlotSelection::Registry { .. } => {
                cfg.conversations.ask.backend = Some(BackendConfig {
                    provider: r.provider.clone(),
                    model: r.model.clone(),
                    endpoint: r.endpoint.clone(),
                    api_key_env: None,
                    api_key_ref: r.api_key_ref.clone(),
                    timeout_secs: None,
                });
            }
        },
        SlotId::Compact => match sel {
            SlotSelection::Local { .. } => {
                cfg.conversations.compact.extractive_backend = None;
                cfg.conversations.compact.extractive_model = r.model.clone();
            }
            SlotSelection::Registry { .. } => {
                cfg.conversations.compact.extractive_backend = Some(BackendConfig {
                    provider: r.provider.clone(),
                    model: r.model.clone(),
                    endpoint: r.endpoint.clone(),
                    api_key_env: None,
                    api_key_ref: r.api_key_ref.clone(),
                    timeout_secs: None,
                });
            }
        },
        SlotId::Rollup => match sel {
            SlotSelection::Local { .. } => {
                cfg.conversations.rollup.extractive_model = r.model.clone()
            }
            SlotSelection::Registry { .. } => bail!("this stage runs locally; pick a local model"),
        },
        _ => unreachable!("write_conversation_stage only called for Ask/Compact/Rollup"),
    }
    Ok(())
}

fn write_role(role: &str, sel: &SlotSelection) -> Result<()> {
    let SlotSelection::Registry { ref_name } = sel else {
        bail!("this stage picks a registry model; pick a registry entry")
    };
    let path = ModelRegistry::default_path()?;
    let mut reg = ModelRegistry::load_from(&path)?;
    reg.roles.insert(
        role.to_string(),
        RoleEntry {
            primary: ref_name.clone(),
            fallback: None,
            cost_budget_per_day_usd: None,
            privacy_local_only: false,
            route_policy: None,
        },
    );
    reg.save_to(&path)
}

pub fn set_slot(slot: SlotId, sel: &SlotSelection) -> Result<ModelSlotsView> {
    match slot {
        SlotId::Reflector => {
            write_role("reflector", sel)?;
            return get_slots();
        }
        SlotId::Curator => {
            write_role("curator", sel)?;
            return get_slots();
        }
        _ => {}
    }

    let path = ModelRegistry::default_path()?;
    let reg = ModelRegistry::load_from(&path)?;
    let mut cfg = load_config()?;

    match slot {
        SlotId::Smart => {
            let old_pair = (cfg.llm.provider.clone(), cfg.llm.model.clone());
            let ask_follows = ask_pair(&cfg) == old_pair;
            let compact_follows = compact_pair(&cfg) == old_pair;
            let rollup_follows = rollup_pair(&cfg) == old_pair;

            let r = resolve_selection(sel, &reg)?;
            cfg.llm.provider = r.provider.clone();
            cfg.llm.model = r.model.clone();
            cfg.llm.openai_url = r.endpoint.clone();
            cfg.llm.api_key_ref = r.api_key_ref.clone();

            if ask_follows {
                write_conversation_stage(&mut cfg, SlotId::Ask, sel, &r)?;
            }
            if compact_follows {
                write_conversation_stage(&mut cfg, SlotId::Compact, sel, &r)?;
            }
            if rollup_follows && let SlotSelection::Local { .. } = sel {
                write_conversation_stage(&mut cfg, SlotId::Rollup, sel, &r)?;
            }
        }
        SlotId::Search => {
            let r = resolve_selection(sel, &reg)?;
            let dims = match sel {
                SlotSelection::Local { dims: Some(d), .. } => *d,
                _ => super::fallback_dims_for(&r.model).unwrap_or(cfg.embedding.dimensions),
            };
            cfg.embedding.provider = r.provider;
            cfg.embedding.model = r.model;
            cfg.embedding.dimensions = dims;
            cfg.embedding.openai_url = r.endpoint;
            cfg.embedding.api_key_ref = r.api_key_ref;
        }
        SlotId::Ask | SlotId::Compact => {
            let r = resolve_selection(sel, &reg)?;
            write_conversation_stage(&mut cfg, slot, sel, &r)?;
        }
        SlotId::Rollup => {
            let r = resolve_selection(sel, &reg)?;
            write_conversation_stage(&mut cfg, slot, sel, &r)?;
        }
        SlotId::Summarize => match sel {
            SlotSelection::Local { model, .. } => {
                cfg.conversations.ask.summarize_model = Some(model.clone())
            }
            SlotSelection::Registry { .. } => bail!("this stage runs locally; pick a local model"),
        },
        SlotId::Reflector | SlotId::Curator => unreachable!("handled above"),
    }

    save_config(&cfg)?;
    get_slots()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Env vars are process-global — serialize the two tests below.
    static ENV_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn smart_set_mirrors_following_stages_only() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("MUR_HOME", tmp.path()) };

        let v = get_slots().unwrap();
        assert!(!v.ask.follows_smart);

        let sel = SlotSelection::Local {
            provider: "ollama".into(),
            model: mur_common::config::DEFAULT_LOCAL_LLM_MODEL.into(),
            base_url: "http://localhost:11434".into(),
            dims: None,
        };
        let v = set_slot(SlotId::Smart, &sel).unwrap();
        assert!(v.ask.follows_smart && v.compact.follows_smart && v.rollup.follows_smart);

        let sel2 = SlotSelection::Local {
            provider: "ollama".into(),
            model: "llama3:8b".into(),
            base_url: "http://localhost:11434".into(),
            dims: None,
        };
        let v = set_slot(SlotId::Smart, &sel2).unwrap();
        assert_eq!(v.ask.model, "llama3:8b");
        assert_eq!(v.rollup.model, "llama3:8b");

        unsafe { std::env::remove_var("MUR_HOME") };
    }

    #[test]
    fn rollup_rejects_registry_selection() {
        let _g = ENV_TEST_LOCK.lock().unwrap();
        let tmp = tempfile::TempDir::new().unwrap();
        unsafe { std::env::set_var("MUR_HOME", tmp.path()) };

        let sel = SlotSelection::Registry {
            ref_name: "whatever".into(),
        };
        assert!(set_slot(SlotId::Rollup, &sel).is_err());

        unsafe { std::env::remove_var("MUR_HOME") };
    }
}
