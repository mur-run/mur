//! Boot-time construction of the layer-3 intent checker
//! (`profile.yaml hitl.intent_check`).

use std::path::Path;
use std::sync::Arc;

use crate::hitl::intent::IntentChecker;
use crate::profile::Profile;

/// The checker this profile asks for, or `None` when it asks for none or the
/// named model cannot be built. A build failure is a warning, not a boot
/// failure: without the checker the gate decides on the static tier, which
/// is exactly what it did before layer 3 existed — never less strict.
pub(crate) fn intent_checker(
    profile: &Profile,
    mur_home: &Path,
    telemetry: Option<tokio::sync::mpsc::Sender<crate::telemetry_writer::Event>>,
) -> Option<Arc<IntentChecker>> {
    let cfg = profile.inner.hitl.intent_check.as_ref()?;
    let built = mur_common::model::ModelRegistry::default_path()
        .and_then(|p| mur_common::model::ModelRegistry::load_from(&p))
        .and_then(|reg| {
            reg.models
                .get(&cfg.model_ref)
                .cloned()
                .ok_or_else(|| anyhow::anyhow!("model_ref {:?} not in registry", cfg.model_ref))
        })
        .and_then(|entry| {
            // Keyed so its usage is priced under the checker's own
            // `models.yaml` entry, never the main model's (#2083).
            crate::llm::client_builder::build_client_from_entry(&entry, profile, mur_home).map(
                |c| {
                    (
                        crate::llm::keyed::KeyedLlmClient::wrap(c, &cfg.model_ref),
                        entry.provider.clone(),
                    )
                },
            )
        });
    match built {
        Ok((client, provider)) => {
            tracing::info!(model_ref = %cfg.model_ref, "hitl intent check on (raise-only)");
            Some(Arc::new(
                IntentChecker::new(client, cfg.timeout()).with_telemetry(telemetry, provider),
            ))
        }
        Err(e) => {
            tracing::warn!(
                model_ref = %cfg.model_ref,
                error = %format!("{e:#}"),
                "hitl intent check NOT active: model did not build; static tiers only"
            );
            None
        }
    }
}
