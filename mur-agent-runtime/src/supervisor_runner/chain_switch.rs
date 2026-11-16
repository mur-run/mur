//! The hot-switch seam for chain/routing agents, moved out of `provider.rs`
//! unchanged (size rule). See `chain_switch_handle`.

use std::sync::Arc;

use crate::llm::LlmClient;

/// Turns a registry ref into a concrete client; shared by the chain's own
/// fallback and by `/model`, so both resolve a ref the same way.
pub(super) type RefClientBuilder =
    Arc<dyn Fn(&str) -> anyhow::Result<Arc<dyn LlmClient>> + Send + Sync>;

/// The hot-switch seam for a chain/routing agent. `/model` on such an agent
/// swaps the PRIMARY and keeps the chain: the chain is the safety net, the
/// primary is the choice. Returns the live slot and a per-ref builder that
/// produces a whole routed client whose profile names `model_ref` as primary,
/// so `resolve_model_refs` keeps deriving `[primary, ...chain]` exactly as at
/// boot, telemetry included.
///
/// Before this, chain agents got no handle at all — and because the global
/// `models.fallback_chain` / `models.smart` settings make EVERY agent a chain
/// agent, `/model` stopped hot-switching anything the moment either was set:
/// each attempt reported `method not found: model/set`, wrote the profile and
/// asked for a restart.
pub(super) fn chain_switch_handle(
    profile: mur_common::agent::AgentProfile,
    cfg: mur_common::config::ModelSwitchConfig,
    telemetry: Option<tokio::sync::mpsc::Sender<crate::telemetry_writer::Event>>,
    build_one: RefClientBuilder,
) -> (
    Arc<crate::llm::switchable::SwitchableLlmClient>,
    crate::llm::fallback::ClientFactory,
) {
    let validate = build_one.clone();
    let agent = profile.name.clone();
    let make = Arc::new(move |primary: Option<&str>| -> Arc<dyn LlmClient> {
        let mut p = profile.clone();
        if let Some(r) = primary {
            p.model_ref = Some(r.to_string());
        }
        let build_one = build_one.clone();
        let factory: crate::llm::fallback::ClientFactory = Box::new(move |r: &str| build_one(r));
        let mut fb = crate::llm::fallback::FallbackLlmClient::new_routed(
            p,
            cfg.clone(),
            factory,
            cfg.retry.clone(),
        );
        if let Some(tx) = telemetry.clone() {
            fb = fb.with_telemetry(tx, agent.clone());
        }
        Arc::new(fb)
    });
    let switchable = crate::llm::switchable::SwitchableLlmClient::new(make(None));
    let build_client: crate::llm::fallback::ClientFactory = Box::new(move |model_ref: &str| {
        // A ref the registry does not know fails the switch here, where
        // `model/set` keeps the old client, instead of on the next turn, where
        // the chain would quietly answer for a primary that never existed.
        validate(model_ref)?;
        Ok(make(Some(model_ref)))
    });
    (switchable, build_client)
}
