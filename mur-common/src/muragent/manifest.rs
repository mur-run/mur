//! `.muragent` v2 manifest schema types.
//!
//! Schema version: `mur-agent/2`. No backwards compat with `mur-agent-package/1`.

use serde::{Deserialize, Serialize};

/// Top-level manifest as written to `manifest.yaml` inside a `.muragent`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MuragentManifest {
    pub schema: String,
    pub exported_at: String,
    pub exporter: ExporterInfo,
    pub agent: AgentRef,
    pub required_surfaces: Vec<Surface>,
    #[serde(default)]
    pub optional_capabilities: Vec<String>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerRef>,
    pub icon: IconHashes,
    #[serde(default)]
    pub sanitized: SanitizedReport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hub: Option<HubBlock>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commander: Option<CommanderBlock>,
    /// Reserved for future specs; v1 must ignore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<serde_json::Value>,
    /// Reserved for future specs; v1 must ignore.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assignment: Option<serde_json::Value>,
    /// Model backend hint for the recipient's first-run resolution (§7.1).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_hint: Option<ModelHint>,
    /// `Some("official")` for agents published from the official catalog.
    /// Tamper-evident via `predicate.manifest_sha256` in the DSSE-signed
    /// statement (the canonical JSON derived from `manifest.yaml`), so
    /// stripping it invalidates the package signature.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExporterInfo {
    pub mur_version: String,
    pub tool: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_hub_version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min_commander_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentRef {
    pub slug: String,
    pub display_name: String,
    pub bundle_id: String,
    pub url_scheme: String,
    pub original_uuid: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelTier {
    Small,
    Mid,
    Frontier,
}

/// Declares what kind of model the agent was authored against, so the
/// recipient's first-run wizard can resolve a backend (no weights travel).
/// See spec §7.1.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ModelHint {
    pub provider: String,
    pub name: String,
    pub tier: ModelTier,
    #[serde(default)]
    pub min_ram_gb: u32,
    pub local_capable: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Surface {
    Hub,
    Commander,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerRef {
    pub name: String,
    pub command_basename: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IconHashes {
    #[serde(default)]
    pub formats: Vec<String>,
    #[serde(default)]
    pub hash: IconHashMap,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IconHashMap {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icns: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ico: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub png: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SanitizedReport {
    #[serde(default)]
    pub removed_fields: Vec<String>,
}

// ─── Hub-specific block ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubBlock {
    pub appearance: HubAppearance,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub voice: Option<HubVoice>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pet: Option<HubPet>,
    #[serde(default)]
    pub url_scheme_overrides: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubAppearance {
    pub style_preset: String,
    pub behavior_preset: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubVoice {
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HubPet {
    pub enabled: bool,
}

// ─── Commander-specific block ───

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderBlock {
    pub chat_platforms: Vec<String>,
    #[serde(default)]
    pub workflows: Vec<CommanderWorkflowRef>,
    #[serde(default)]
    pub programs: Vec<CommanderProgramRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub jira: Option<CommanderJira>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sub_agents: Option<CommanderSubAgents>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule_defaults: Option<CommanderScheduleDefaults>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderWorkflowRef {
    pub name: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub schedule: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderProgramRef {
    pub file: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderJira {
    pub base_url: String,
    pub secret: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderSubAgents {
    pub max_concurrent: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommanderScheduleDefaults {
    pub timezone: String,
}

// ─── Validation helpers ───

impl MuragentManifest {
    /// Schema version must be exactly `mur-agent/2`.
    pub fn is_v2(&self) -> bool {
        self.schema == "mur-agent/2"
    }

    /// Slug must match `run.mur.agent.<slug>` bundle ID pattern.
    pub fn validate_bundle_id(&self) -> Result<(), String> {
        let expected = format!("run.mur.agent.{}", self.agent.slug);
        if self.agent.bundle_id != expected {
            return Err(format!(
                "bundle_id '{}' does not match expected '{}'",
                self.agent.bundle_id, expected
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod model_hint_tests {
    use super::*;

    #[test]
    fn manifest_round_trips_without_model_hint() {
        let yaml = "\
schema: mur-agent/2
exported_at: '2026-05-29T00:00:00Z'
exporter: { mur_version: 1.0.0, tool: mur }
agent: { slug: coach, display_name: Coach, bundle_id: run.mur.agent.coach, url_scheme: muragent-coach, original_uuid: u1 }
required_surfaces: [hub]
icon: {}
";
        let m: MuragentManifest = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(m.model_hint.is_none());
    }

    #[test]
    fn model_hint_serializes_and_parses() {
        let hint = ModelHint {
            provider: "ollama".into(),
            name: "llama3.2:3b".into(),
            tier: ModelTier::Small,
            min_ram_gb: 8,
            local_capable: true,
        };
        let s = serde_yaml_ng::to_string(&hint).unwrap();
        let back: ModelHint = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(hint, back);
        assert!(s.contains("tier: small"));
    }
}

#[cfg(test)]
mod distribution_marker_tests {
    use super::*;

    #[test]
    fn distribution_marker_roundtrips_and_defaults_none() {
        let yaml = "\
schema: mur-agent/2
exported_at: '2026-05-29T00:00:00Z'
exporter: { mur_version: 1.0.0, tool: mur }
agent: { slug: coach, display_name: Coach, bundle_id: run.mur.agent.coach, url_scheme: muragent-coach, original_uuid: u1 }
required_surfaces: [hub]
icon: {}
";
        let m: MuragentManifest = serde_yaml_ng::from_str(yaml).expect("legacy yaml parses");
        assert!(m.distribution.is_none());
        let mut m2 = m.clone();
        m2.distribution = Some("official".into());
        let round: MuragentManifest =
            serde_yaml_ng::from_str(&serde_yaml_ng::to_string(&m2).unwrap()).unwrap();
        assert_eq!(round.distribution.as_deref(), Some("official"));
    }
}
