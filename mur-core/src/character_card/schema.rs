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

use super::extensions::MurExt;

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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CardData {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nickname: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub personality: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub scenario: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub first_mes: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mes_example: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_greetings: Vec<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub system_prompt: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub post_history_instructions: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creator: Option<String>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub creator_notes_multilingual: std::collections::BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub creation_date: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modification_date: Option<i64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assets: Vec<Asset>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character_book: Option<CharacterBook>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    #[serde(rename = "type")]
    pub kind: String,
    pub uri: String,
    pub name: String,
    pub ext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterBook {
    pub name: String,
    #[serde(default = "default_scan_depth")]
    pub scan_depth: u32,
    #[serde(default = "default_token_budget")]
    pub token_budget: u32,
    #[serde(default)]
    pub recursive_scanning: bool,
    pub entries: Vec<CharacterBookEntry>,
}

fn default_scan_depth() -> u32 {
    4
}

fn default_token_budget() -> u32 {
    512
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CharacterBookEntry {
    pub keys: Vec<String>,
    pub content: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub insertion_order: i32,
    #[serde(default = "default_position")]
    pub position: String,
    #[serde(default)]
    pub constant: bool,
}

fn default_true() -> bool {
    true
}

fn default_position() -> String {
    "before_char".into()
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Extensions {
    #[serde(default, rename = "mur", skip_serializing_if = "Option::is_none")]
    pub mur: Option<MurExt>,
}
