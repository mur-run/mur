//! `MurCard` — the minimal `.murcard.yaml` schema.
//!
//! The card is CCv3-compatible: any unknown top-level key is captured by
//! the `ccv3_passthrough` map and re-emitted verbatim on serialize. mur's
//! own additions live under the `extensions.mur` namespace so consumers
//! that only understand stock CCv3 can ignore them safely.
//!
//! Phase M2.7 lands the schema + a single helper. The full
//! `mur agent export --format card` pipeline is a D4 milestone.

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MurCard {
    /// `"murcard_v1"`.
    pub spec: String,
    /// `"1.0"`.
    pub spec_version: String,
    pub data: CardData,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<Extensions>,
    /// CCv3 passthrough: any unknown top-level key gets preserved verbatim.
    #[serde(flatten)]
    pub ccv3_passthrough: std::collections::BTreeMap<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardData {
    pub name: String,
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extensions {
    #[serde(default, rename = "mur", skip_serializing_if = "Option::is_none")]
    pub mur: Option<MurExt>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MurExt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_memory: Option<super::first_memory::FirstMemoryExt>,
}
