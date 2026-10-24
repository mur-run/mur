//! `mur agent perm intent-check` — layer 3 of the tier gate.
//!
//! The model named here may RAISE a `bash` call's tier (make it ask), never
//! lower one; see `mur_common::hitl::intent`. Turning it on therefore can
//! only add prompts, which is why it is an ordinary perm edit and not a
//! reseal-gated entitlement.

use anyhow::{Result, bail};
use mur_common::hitl::intent::IntentCheckConfig;
use mur_common::model::ModelRegistry;

use super::super::{load_profile_for_edit, save_profile};
use super::warn_if_running;

/// Turn the check on with `model_ref`, or off with `None`.
pub fn cmd_perm_set_intent_check(
    name: &str,
    model_ref: Option<&str>,
    timeout_ms: Option<u64>,
) -> Result<()> {
    let cfg = match model_ref.map(str::trim).filter(|r| !r.is_empty()) {
        None => None,
        Some(r) => {
            let reg = ModelRegistry::load_from(&ModelRegistry::default_path()?)?;
            let Some(entry) = reg.models.get(r) else {
                bail!("model_ref '{r}' is not in ~/.mur/models.yaml (see `mur model list`)");
            };
            // A spawned CLI owns its own loop; there is no client to send a
            // one-word side question to, so the runtime could never build it.
            if mur_common::cli_backend::from_provider(&entry.provider).is_some() {
                bail!(
                    "model_ref '{r}' runs through an external CLI ({}); pick an API or local model",
                    entry.provider
                );
            }
            Some((
                IntentCheckConfig {
                    model_ref: r.to_string(),
                    timeout_ms,
                },
                egress_note(&entry.provider),
            ))
        }
    };
    let (path, mut profile) = load_profile_for_edit(name)?;
    let (cfg, egress) = match cfg {
        Some((c, egress)) => (Some(c), egress),
        None => (None, None),
    };
    let on = cfg.as_ref().map(|c| c.model_ref.clone());
    profile.hitl.intent_check = cfg;
    save_profile(&path, &mut profile)?;
    match on {
        Some(r) => println!(
            "{name}: intent check on with `{r}` — it may make a bash call ask, never skip one"
        ),
        None => println!("{name}: intent check off — static tiers only"),
    }
    if let Some(note) = egress {
        println!("{note}");
    }
    warn_if_running(name);
    Ok(())
}

/// The disclosure printed when the intent-check model is not on this machine,
/// or `None` for a local provider. Locality is `provider_is_local`'s call.
fn egress_note(provider: &str) -> Option<String> {
    (!mur_common::model::provider_is_local(provider)).then(|| {
        format!(
            "note: `{provider}` is not a local provider — each checked bash command \
             is sent to it. Pick a local model to keep commands on this machine."
        )
    })
}

#[cfg(test)]
mod tests {
    use super::egress_note;

    #[test]
    fn cloud_provider_gets_a_disclosure_naming_it() {
        let note = egress_note("anthropic").expect("cloud provider must be disclosed");
        assert!(
            note.contains("`anthropic` is not a local provider"),
            "{note}"
        );
        assert!(note.contains("each checked bash command"), "{note}");
    }

    #[test]
    fn local_provider_gets_no_disclosure() {
        assert_eq!(egress_note("ollama"), None);
        // provider_is_local lowercases; the note must follow it, not re-decide.
        assert_eq!(egress_note("Ollama"), None);
    }
}
