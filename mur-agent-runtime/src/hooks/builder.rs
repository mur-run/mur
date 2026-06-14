use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use mur_common::agent::AgentProfile;
use mur_common::companion::Formality;

use super::{
    Hook, HookChain, b0::B0SafetyHook, companion_voice::CompanionVoiceHook, ledger::LedgerHook,
    telemetry::TelemetryHook, voice_input::VoiceInputHook,
};
use crate::companion::voice::{VoiceInput, compose_with_overrides};
use crate::voice::stt::{VadGate, WhisperStt};

/// Build the `HookChain` for `profile`.
///
/// Mandatory tier (always present, fixed order):
///   TelemetryHook (pos 0) → B0SafetyHook (pos 1)
///
/// Optional tier (auto-wire from feature flags, overridable via `profile.hooks`):
///   LedgerHook → CompanionVoiceHook → VoiceInputHook
///
/// * `agent_home` — `~/.mur/agents/<name>/`
/// * `mur_home`   — `~/.mur/`
pub fn build_chain(profile: &AgentProfile, agent_home: &Path, mur_home: &Path) -> HookChain {
    let cfg = &profile.hooks;

    let mut chain: Vec<Arc<dyn Hook>> = vec![
        Arc::new(TelemetryHook::new()) as Arc<dyn Hook>,
        Arc::new(B0SafetyHook::new()),
    ];

    // Auto-compression of oversized tool outputs (Surface 2). Gated by
    // compress.yaml `auto.enabled` + `auto.agent_runtime`.
    let ccfg = mur_compress::CompressConfig::load(mur_home);
    if ccfg.auto.enabled && ccfg.auto.agent_runtime {
        chain.push(Arc::new(super::compress::CompressHook::new(
            mur_home.join("compress"),
            ccfg,
        )));
    }

    if cfg.ledger {
        chain.push(Arc::new(LedgerHook::new()));
    }

    let want_companion_voice = cfg.companion_voice.unwrap_or(profile.companion.enabled);
    if want_companion_voice {
        let formality_str = match profile.companion.voice_overrides.formality {
            Some(Formality::Formal) => "formal",
            _ => "casual",
        };
        let extra = profile
            .companion
            .voice_overrides
            .extra_instructions
            .as_deref()
            .unwrap_or("");
        let first_memory = profile
            .companion
            .onboarding
            .first_memory
            .as_ref()
            .map(|m| m.text.as_str());
        let rendered = compose_with_overrides(
            Some(agent_home),
            Some(mur_home),
            VoiceInput {
                relationship: profile.companion.relationship.clone(),
                locale: &profile.companion.locale,
                name_for_user: &profile.display_name,
                first_memory,
                formality: formality_str,
                extra_instructions: extra,
            },
        );
        chain.push(Arc::new(CompanionVoiceHook::new(Arc::new(rendered))));
    }

    let want_voice_input = cfg.voice_input.unwrap_or(profile.voice.enabled);
    if want_voice_input {
        let model_path = mur_home.join("voices/whisper-large-v3-turbo-q5_1.bin");
        match WhisperStt::new(&model_path) {
            Ok(stt) => chain.push(Arc::new(VoiceInputHook::new(
                stt,
                VadGate::default(),
                profile.voice.input_device.clone(),
                Duration::from_secs(30),
            ))),
            Err(e) => tracing::warn!(
                model = %model_path.display(),
                error = %e,
                "VoiceInputHook skipped: whisper model not found; \
                 run `mur voice install` to download"
            ),
        }
    }

    HookChain::new(chain)
}
