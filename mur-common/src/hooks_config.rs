use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HooksConfig {
    /// Whether the outbox `LedgerHook` runs. Default: true.
    #[serde(default = "default_true")]
    pub ledger: bool,

    /// Whether `CompanionVoiceHook` runs.
    /// `None` = auto-wire from `profile.companion.enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion_voice: Option<bool>,

    /// Whether `VoiceInputHook` runs.
    /// `None` = auto-wire from `profile.voice.enabled`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice_input: Option<bool>,
}

impl Default for HooksConfig {
    fn default() -> Self {
        Self {
            ledger: true,
            companion_voice: None,
            voice_input: None,
        }
    }
}
