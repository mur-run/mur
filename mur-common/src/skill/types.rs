//! Skill type enums.

use serde::{Deserialize, Serialize};

/// Which host(s) may load a skill. See spec §2.3.
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "kebab-case")]
pub enum HostId {
    MurAgent,
    MurCommander,
    /// Default when `hosts:` is omitted — backward compatible.
    #[default]
    All,
    #[serde(untagged)]
    Custom(String),
}

/// Three-tier skill trust model. Mirrors mur-commander `trust/level.rs`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, PartialOrd, Ord, Default,
)]
#[serde(rename_all = "kebab-case")]
pub enum TrustLevel {
    /// Peer transfer, agent-generated, untrusted registry.
    #[default]
    Sandboxed,
    /// Registry-verified checksum match, community-reviewed.
    Verified,
    /// Built-in, user-promoted, or trusted-publisher-signed.
    Trusted,
}

/// Top-level skill category.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Category {
    Context,
    Workflow,
    Command,
    Meta,
    Note,
    /// Media skills (video-analyze, scene-explain, vlc-control, watch-together):
    /// they drive the runtime's media tools rather than the four-stage pipeline.
    Media,
}

/// Publishers whose on-disk copies MUR owns and may replace: the shipped
/// builtins and the official catalog. Everything else — `human:local` from
/// `mur notes create`, `agent:<name>` from the `remember` tool, a `human:<you>`
/// skill you authored — is yours, and MUR cannot get it back if it removes it.
///
/// The list already existed in `sync_cmd` for "may I overwrite this?"; the
/// lifecycle sweep needs the same question for "may I delete this?", and two
/// copies of one list is how the answers start disagreeing.
pub const MUR_OWNED_PUBLISHERS: &[&str] = &["human:mur-official", "human:mur"];

/// Whether MUR published this and can therefore reinstall it.
pub fn is_mur_owned_publisher(publisher: &str) -> bool {
    MUR_OWNED_PUBLISHERS.contains(&publisher)
}

/// Where a skill came from. Drives the curation gate: `Llm`-authored skills
/// cannot auto-promote past `Emerging` until a human curates them
/// (amendment A1, `2026-05-28-mur-workflow-engine-design-v2.md`).
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Provenance {
    /// Hand-authored by a person. Default — no gate.
    #[default]
    Human,
    /// Produced by the LLM extraction judge. Gated until curated.
    Llm,
    /// LLM-extracted, then human-reviewed/edited. No gate.
    Hybrid,
}

/// Exactly one content mode is populated; see spec §3.2.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ContentMode {
    Context,
    Workflow,
    Command,
    Note,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, schemars::JsonSchema,
)]
#[serde(rename_all = "lowercase")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
    Critical,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Ord, PartialOrd, schemars::JsonSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum TriggerKind {
    Command,
    Keyword,
    SessionStart,
    Manual,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_id_serialises_kebab_case() {
        let yaml = serde_yaml_ng::to_string(&HostId::MurAgent).unwrap();
        assert_eq!(yaml.trim(), "mur-agent");
    }

    #[test]
    fn trust_level_ordering_matches_spec() {
        assert!(TrustLevel::Sandboxed < TrustLevel::Verified);
        assert!(TrustLevel::Verified < TrustLevel::Trusted);
    }

    #[test]
    fn host_id_default_is_all() {
        assert_eq!(HostId::default(), HostId::All);
    }

    #[test]
    fn note_category_serialises_lowercase_and_roundtrips() {
        let yaml = serde_yaml_ng::to_string(&Category::Note).unwrap();
        assert_eq!(yaml.trim(), "note");
        let parsed: Category = serde_yaml_ng::from_str("note").unwrap();
        assert_eq!(parsed, Category::Note);
    }

    #[test]
    fn media_category_serialises_lowercase_and_roundtrips() {
        let yaml = serde_yaml_ng::to_string(&Category::Media).unwrap();
        assert_eq!(yaml.trim(), "media");
        let parsed: Category = serde_yaml_ng::from_str("media").unwrap();
        assert_eq!(parsed, Category::Media);
    }

    #[test]
    fn note_content_mode_serialises_lowercase_and_roundtrips() {
        let yaml = serde_yaml_ng::to_string(&ContentMode::Note).unwrap();
        assert_eq!(yaml.trim(), "note");
        let parsed: ContentMode = serde_yaml_ng::from_str("note").unwrap();
        assert_eq!(parsed, ContentMode::Note);
    }

    #[test]
    fn provenance_defaults_to_human_and_roundtrips() {
        // Default is Human (a skill is human-authored unless stated otherwise).
        assert_eq!(Provenance::default(), Provenance::Human);
        // Serializes lowercase, like Category.
        let yaml = serde_yaml_ng::to_string(&Provenance::Llm).unwrap();
        assert_eq!(yaml.trim(), "llm");
        let parsed: Provenance = serde_yaml_ng::from_str("hybrid").unwrap();
        assert_eq!(parsed, Provenance::Hybrid);
    }
}
