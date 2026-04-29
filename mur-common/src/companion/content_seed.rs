//! Embedded content pool seed. mur-core 'companion init' copies these to disk;
//! later phases let users edit on disk; mur-agent-runtime never reads embedded
//! seed at runtime (it reads the disk copy).

use crate::companion::Situation;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SituationFile {
    pub situation: Situation,
    pub locale: String,
    pub templates: Vec<TemplateSeed>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TemplateSeed {
    pub id: String,
    #[serde(default = "default_weight")]
    pub weight: f32,
    pub cooldown_days: u32,
    #[serde(default)]
    pub tags: Vec<String>,
    pub source: String,
    pub reviewed_by: String,
    pub prompt_seed: String,
}

fn default_weight() -> f32 {
    1.0
}

pub const MORNING_GREETING_ZH_TW: &str = include_str!("content/morning_greeting.zh-TW.yaml");
pub const MORNING_GREETING_EN_US: &str = include_str!("content/morning_greeting.en-US.yaml");
pub const GENTLE_CHECK_IN_ZH_TW: &str = include_str!("content/gentle_check_in.zh-TW.yaml");
pub const GENTLE_CHECK_IN_EN_US: &str = include_str!("content/gentle_check_in.en-US.yaml");
pub const SHARE_QUOTE_ZH_TW: &str = include_str!("content/share_quote.zh-TW.yaml");
pub const SHARE_QUOTE_EN_US: &str = include_str!("content/share_quote.en-US.yaml");
pub const SHARE_LINK_ZH_TW: &str = include_str!("content/share_link.zh-TW.yaml");
pub const SHARE_LINK_EN_US: &str = include_str!("content/share_link.en-US.yaml");

/// Returns `(situation, locale, raw_yaml)` for every embedded seed file.
/// Used by `mur agent companion init` (M1.6+) to seed the user's disk copy.
pub fn all_seeds() -> Vec<(Situation, &'static str, &'static str)> {
    use Situation::*;
    vec![
        (MorningGreeting, "zh-TW", MORNING_GREETING_ZH_TW),
        (MorningGreeting, "en-US", MORNING_GREETING_EN_US),
        (GentleCheckIn, "zh-TW", GENTLE_CHECK_IN_ZH_TW),
        (GentleCheckIn, "en-US", GENTLE_CHECK_IN_EN_US),
        (ShareQuote, "zh-TW", SHARE_QUOTE_ZH_TW),
        (ShareQuote, "en-US", SHARE_QUOTE_EN_US),
        (ShareLink, "zh-TW", SHARE_LINK_ZH_TW),
        (ShareLink, "en-US", SHARE_LINK_EN_US),
    ]
}

/// Parse the embedded YAML into a typed `SituationFile`.
pub fn parse(yaml: &str) -> Result<SituationFile, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str(yaml)
}
