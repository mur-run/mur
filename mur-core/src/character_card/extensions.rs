//! `extensions.mur` namespace — full v1 schema.
//!
//! Roadmap §4.4. The signature payload is part of the schema here, but
//! the signing/verification helpers live in `signing.rs` (M4.2). v1
//! locks `schema_version = 1`; future fields go behind their own
//! optional sub-blocks so legacy cards keep deserializing.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::first_memory::FirstMemoryExt;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MurExt {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<VoiceExt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub avatar: Option<AvatarExt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<RelationshipExt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_memory: Option<FirstMemoryExt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub companion: Option<CompanionExt>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<ProvenanceExt>,
}

fn default_schema_version() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceExt {
    pub provider: String, // "kokoro" | "local-piper" | "system" | "character-ai" | "none"
    pub voice_id: String,
    #[serde(default = "default_speed")]
    pub speed: f32,
}

fn default_speed() -> f32 {
    1.0
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AvatarExt {
    pub primary_asset: String,
    #[serde(default)]
    pub emotion_map: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RelationshipExt {
    pub kind: String,
    #[serde(default)]
    pub addressing: String,
    #[serde(default)]
    pub formality: String,
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary_language: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompanionExt {
    #[serde(default)]
    pub proactive_enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_window: Option<String>,
    #[serde(default)]
    pub situations: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceExt {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<CardSignature>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_rating: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_trust: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardSignature {
    pub algorithm: String,
    pub public_key: String,
    pub value: String,
    pub signed_at: chrono::DateTime<chrono::Utc>,
}
