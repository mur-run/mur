//! Agent profile, Agent Card, and LockFile types shared between
//! mur-agent-runtime and mur-core.

use crate::companion::{Formality, Relationship};
use crate::deps::ProgramDep;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

mod companion;
mod entitlements;
mod lifecycle;
mod mcp;
mod transport;

pub use companion::*;
pub use entitlements::*;
pub use lifecycle::*;
pub use mcp::*;
pub use transport::*;

/// Skill metadata broadcast in the Agent Card (Layer 1 + Layer 2).
///
/// Populated by `mur skill install` (registry or agent:// URL). Distinct from
/// `AgentProfile.skills`, which is the legacy per-agent-path list managed by
/// `mur agent skill add`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SkillCardEntry {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub version: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub publisher: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<SkillCardTrigger>,
    /// Layer 2 abstract — injected at session start (~200 tokens).
    /// On-disk YAML key is `abstract` (a Rust reserved word).
    #[serde(default, skip_serializing_if = "String::is_empty", rename = "abstract")]
    pub abstract_text: String,
    /// Provenance chain copied from the installed manifest. Empty for
    /// registry-installed skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfer_chain: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct SkillCardTrigger {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub pattern: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentProfile {
    pub schema: u32,
    pub id: String, // UUIDv7
    pub name: String,
    pub display_name: String,
    /// Coarse human-facing role for grouping/filtering (e.g. "Engineer").
    /// A free label, not a registry — bundled defaults are UI suggestions and
    /// users can type their own. Also the SOFT signal in the dispatch index
    /// (`agent_facts`), where it explains and ranks candidates but never
    /// filters them: what an agent may actually do is decided by
    /// `entitlements`, which the kernel enforces and a stale label cannot
    /// overstate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// How hard this agent's model should work per turn
    /// (`low`/`medium`/`high`/`xhigh`/`max`). `None` leaves the field off,
    /// which is the API default (`high`) — not "no effort".
    ///
    /// Set it where the agent's JOB is known: a single-purpose build
    /// specialist earns `xhigh`, a fan-out research worker `medium`, a
    /// classifier `low`. Narrowed to what the resolved model accepts at the
    /// client boundary, so an agent pinned to an older model degrades rather
    /// than 400s.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<crate::llm::Effort>,
    pub version: String,
    pub persona: Persona,
    pub sys_prompt_file: String,
    pub model: ModelConfig,
    /// Optional pointer into ~/.mur/models.yaml. When set, the runtime
    /// prefers the registry entry over the inline `model:` block.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    /// Per-agent fallback chain (ordered model_refs). Overrides the global
    /// `models.fallback_chain` when non-empty. See the model-switch spec.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallback_chain: Vec<String>,
    /// Per-agent difficulty-routing override. Absent fields inherit the global
    /// `models.routing`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub routing: Option<crate::config::RoutingOverride>,
    /// Per-agent Smart background-routing override. Absent fields inherit the
    /// global `models.smart`; `None` means "follow the global setting".
    /// Promoted out of `routing` — nesting it there meant overriding Smart
    /// silently rewrote this agent's difficulty routing as a side effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub smart: Option<crate::config::SmartOverride>,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerEntry>,
    #[serde(default)]
    pub skills: Vec<String>,
    /// Skills installed via `mur skill install`. Distinct from `skills`
    /// (which holds legacy per-agent paths from `mur agent skill add`).
    /// Broadcast in the Agent Card alongside `skills`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub installed_skills: Vec<SkillCardEntry>,
    /// Per-agent skill denylist (add-on Phase 1). Skill names that are
    /// installed/visible to this agent but suppressed from injection.
    /// Non-destructive: the skill's files/stats are untouched. Empty = all
    /// visible skills enabled (back-compat: absent in old profiles).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_skills: Vec<String>,

    /// Per-agent MCP denylist (add-on Phase 1). `McpServerEntry` names not
    /// spawned for this agent. Non-destructive: the entry + its pin stay in
    /// the profile. Empty = all configured servers enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_mcp: Vec<String>,

    /// Names of per-agent secrets the user handed this agent (murmur
    /// `/secret`, `mur agent secret set`). NAMES ONLY — the values live in the
    /// keychain under `mur-agent/<name>/<NAME>`. The list exists because the
    /// keychain cannot be enumerated: the supervisor reads it pre-seal to know
    /// which accounts to load. Empty = nothing to load (back-compat).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub secrets: Vec<String>,
    /// Plugin-groups imported by this agent (add-on Phase 2). Each is
    /// self-contained (members installed per-agent). Absent/empty in
    /// legacy profiles (back-compat).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addons: Vec<AddonRef>,
    pub transport: TransportConfig,
    pub communication: CommunicationConfig,
    #[serde(default)]
    pub capabilities: Vec<String>,
    pub entitlements: Entitlements,
    #[serde(default)]
    pub notifications: NotificationsConfig,
    pub retry: RetryConfig,
    pub lifecycle: LifecycleConfig,
    /// Cryptographic identity for cross-host A2A (P0a.5+). Default = empty
    /// (legacy P0a profiles continue to load without this block).
    #[serde(default)]
    pub identity: IdentityConfig,
    #[serde(default)]
    pub file_transfer: FileTransferConfig,
    #[serde(default)]
    pub deployment: DeploymentConfig,
    /// Companion subsystem (Phase 1.1+). Default = disabled (legacy profiles
    /// continue to load without this block).
    #[serde(default)]
    pub companion: CompanionConfig,
    /// Human-in-the-loop configuration (Phase 2). Default = disabled.
    #[serde(default)]
    pub hitl: HitlConfig,
    /// Execution limits for this agent's own tasks (spec 2026-09-12 §3.1).
    /// Absent → inherit. Replaces `hitl.max_iterations` / `hitl.max_tokens`,
    /// which stay readable for the migration warning until the runtime
    /// switch (step 4) stops applying them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<crate::limits::Limits>,
    /// Voice I/O configuration (D1). Default = disabled.
    #[serde(default)]
    pub voice: VoiceConfig,
    /// A1: config-driven handler picker. Absent block = all defaults.
    #[serde(default)]
    pub hooks: crate::HooksConfig,
    /// Pubkeys of bridges (and other LLM-less peers) this agent will accept
    /// signed envelopes from. Empty = accept no bridge traffic. Default = empty.
    #[serde(default)]
    pub trusted_peers: Vec<crate::bridge::peer::TrustedPeer>,
    pub created_at: String,
    pub updated_at: String,
    /// Hub companion visual identity (M-h3). Default = default-blob / Normal / Pending.
    #[serde(default)]
    pub appearance: AgentAppearance,
    /// E6: Pattern federation — snapshot filter + outbox config.
    #[serde(default)]
    pub federation: FederationConfig,

    /// A1: declarative UI action list — file_actions rendered as action
    /// buttons in the pending-item selection UI. New top-level key; NOT
    /// nested under `capabilities:`.
    #[serde(default)]
    pub file_actions: Vec<crate::action::FileAction>,

    /// A2 + A3: action pipeline configuration (deletion safety + queue limits).
    #[serde(default)]
    pub action_pipeline: crate::action::ActionPipelineConfig,

    /// External programs this artifact needs at runtime (portable-deps spec).
    /// Absent → empty; resolved by `mur agent/fleet doctor` + `install-deps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_programs: Vec<ProgramDep>,

    /// Capability refs installed into this agent (Pack S3). Absent → empty;
    /// resolved against the local capability registry / bundle store.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_capabilities: Vec<String>,
}

fn default_algorithm() -> String {
    "ed25519".into()
}

/// Algorithms the runtime can generate + verify.
pub const SUPPORTED_ALGORITHMS: &[&str] = &["ed25519"];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct IdentityConfig {
    /// Multibase-encoded Ed25519 public key (base58btc, `z` prefix).
    /// Empty string for legacy P0a profiles; filled on P0a.5 `mur agent create`.
    #[serde(default)]
    pub pubkey: String,
    /// Free-form owner identity (email / SSO sub). None for legacy profiles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner: Option<String>,

    // P0a.6 rekey extensions (all #[serde(default)] — back-compat)
    /// Cryptographic algorithm for this key. Defaults to "ed25519".
    #[serde(default = "default_algorithm")]
    pub algorithm: String,
    /// Monotonic version counter; 0 = initial create, increments on each rotation.
    #[serde(default)]
    pub key_version: u32,
    /// RFC3339 timestamp of when this key was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created_at_key: Option<String>,
    /// Previous public key (before most recent rotation). None if not rotated yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_pubkey: Option<String>,
    /// Version of the previous key. None if not rotated yet.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_key_version: Option<u32>,
    /// RFC3339 timestamp when grace period expires and old key is fully retired.
    /// Only set during rotation; cleared once grace period ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grace_expires_at: Option<String>,
    /// RFC3339 timestamp of the most recent key rotation (normal, not emergency).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rotated_at: Option<String>,
    /// RFC3339 timestamp of emergency key rotation (set only if emergency rekey occurred).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub emergency_rekey_at: Option<String>,
}

impl Default for IdentityConfig {
    fn default() -> Self {
        Self {
            pubkey: String::new(),
            owner: None,
            algorithm: default_algorithm(),
            key_version: 0,
            created_at_key: None,
            previous_pubkey: None,
            previous_key_version: None,
            grace_expires_at: None,
            rotated_at: None,
            emergency_rekey_at: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Persona {
    pub category: PersonaCategory,
    pub description: String,
    pub traits: PersonaTraits,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PersonaCategory {
    Research,
    Automation,
    Monitor,
    Notify,
    Commerce,
    Custom,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PersonaTraits {
    pub tone: String,
    pub risk: String,
    pub verbosity: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelConfig {
    pub provider: String,
    pub name: String,
    #[serde(default)]
    pub params: BTreeMap<String, serde_yaml_ng::Value>,
}

fn default_true() -> bool {
    true
}

impl AgentProfile {
    /// Minimal valid profile for tests — no voice, no MCP, no skills.
    ///
    /// Available in all compilation modes so integration tests in
    /// dependent crates can call it (unlike `#[cfg(test)]` items which
    /// are invisible to downstream test binaries).
    #[doc(hidden)]
    pub fn default_for_tests() -> Self {
        serde_yaml_ng::from_str(include_str!("../../tests/fixtures/minimal_profile.yaml"))
            .expect("minimal profile fixture")
    }

    /// This agent's Smart override, wherever it lives: the promoted `smart`
    /// field, else the legacy `routing.smart` nesting that older profiles and
    /// exported `.muragent` bundles still carry. `None` = follow the global
    /// setting.
    ///
    /// Every reader goes through here. A surface that checked only the
    /// promoted field would report "follows global" for an agent whose legacy
    /// override is actually in force — one fact, two answers.
    pub fn smart_override(&self) -> Option<&crate::config::SmartOverride> {
        self.smart
            .as_ref()
            .or_else(|| self.routing.as_ref().and_then(|r| r.smart.as_ref()))
    }

    /// This agent's effective Smart config: the global values with the agent's
    /// override layered on.
    pub fn effective_smart(
        &self,
        cfg: &crate::config::ModelSwitchConfig,
    ) -> crate::config::SmartConfig {
        cfg.smart.merged(self.smart_override())
    }

    /// This agent's effective difficulty-routing config.
    pub fn effective_routing(
        &self,
        cfg: &crate::config::ModelSwitchConfig,
    ) -> crate::config::RoutingConfig {
        cfg.routing.merged(self.routing.as_ref())
    }

    /// Load an agent's profile from `<mur_home>/agents/<name>/profile.yaml`.
    ///
    /// Canonical read-path counterpart to the atomic-write path used by
    /// `mur agent create`/`mur agent mcp add` (`write_atomic` in
    /// `mur-core::cmd::agent`) — callers that already have `mur_home` in
    /// hand (e.g. provisioning flows, tests) can load a profile without
    /// going through the `MUR_HOME`-env-var-based `resolve_mur_home`.
    pub fn load(mur_home: &std::path::Path, name: &str) -> anyhow::Result<Self> {
        let path = mur_home.join("agents").join(name).join("profile.yaml");
        let yaml = std::fs::read_to_string(&path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", path.display()))?;
        serde_yaml_ng::from_str(&yaml).map_err(|e| anyhow::anyhow!("parse {}: {e}", path.display()))
    }

    /// The imported add-on group a skill/mcp/command name belongs to.
    pub fn group_of(&self, name: &str) -> Option<&AddonRef> {
        self.addons.iter().find(|g| {
            g.skills.iter().any(|n| n == name)
                || g.mcp.iter().any(|n| n == name)
                || g.commands.iter().any(|n| n == name)
        })
    }

    /// Whether `skill_name` is enabled (§3.3): not denied AND, if it
    /// belongs to an imported group, that group is enabled.
    pub fn skill_enabled(&self, skill_name: &str) -> bool {
        name_enabled(&self.disabled_skills, skill_name)
            && self.group_of(skill_name).is_none_or(|g| g.enabled)
    }

    /// Whether MCP server `server_id` is enabled (§3.3).
    pub fn mcp_enabled(&self, server_id: &str) -> bool {
        name_enabled(&self.disabled_mcp, server_id)
            && self.group_of(server_id).is_none_or(|g| g.enabled)
    }

    /// Toggle a skill for this agent without uninstalling it.
    pub fn set_skill_enabled(&mut self, skill_name: &str, enabled: bool) {
        set_denylist(&mut self.disabled_skills, skill_name, enabled);
    }

    /// Toggle an MCP server for this agent without removing it.
    pub fn set_mcp_enabled(&mut self, server_id: &str, enabled: bool) {
        set_denylist(&mut self.disabled_mcp, server_id, enabled);
    }

    /// Toggle an imported plugin-group as a unit. Returns false if no
    /// add-on has that id.
    pub fn set_addon_enabled(&mut self, addon_id: &str, enabled: bool) -> bool {
        match self.addons.iter_mut().find(|g| g.id == addon_id) {
            Some(g) => {
                g.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// Emergency kill-switch (§7): clears every add-on group's `enabled` flag.
    /// Members are already forced off by the group AND-gate in `skill_enabled` /
    /// `mcp_enabled`, so no denylist push is needed — and avoiding it means
    /// `set_addon_enabled(id, true)` fully restores the group without leftover
    /// per-member denials.
    pub fn disable_all_addons(&mut self) {
        for g in &mut self.addons {
            g.enabled = false;
        }
    }

    /// This agent's MCP servers minus any disabled for it.
    pub fn enabled_mcp_servers(&self) -> Vec<McpServerEntry> {
        self.mcp_servers
            .iter()
            .filter(|m| self.mcp_enabled(&m.name))
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod model_ref_tests {
    use super::*;

    #[test]
    fn legacy_profile_without_model_ref_still_parses() {
        let yaml = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            p.model_ref.is_none(),
            "legacy profile must not have model_ref"
        );
    }

    #[test]
    fn round_trip_with_model_ref_preserves_field() {
        let yaml = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
        let mut p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        p.model_ref = Some("anthropic_opus_4_7".into());
        let s = serde_yaml_ng::to_string(&p).unwrap();
        assert!(s.contains("model_ref: anthropic_opus_4_7"), "yaml: {s}");
        let p2: AgentProfile = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(p2.model_ref.as_deref(), Some("anthropic_opus_4_7"));
    }

    #[test]
    fn per_agent_fallback_and_routing_optional_and_legacy_safe() {
        // Load fixture (no fallback_chain / routing) — legacy safe.
        let yaml = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(
            p.fallback_chain.is_empty(),
            "legacy profile must have empty fallback_chain"
        );
        assert!(
            p.routing.is_none(),
            "legacy profile must have no routing override"
        );

        // Round-trip with fallback_chain and routing.
        let mut p = p.clone();
        p.fallback_chain = vec!["claude_opus".into(), "claude_sonnet".into()];
        p.routing = Some(crate::config::RoutingOverride {
            enabled: Some(true),
            ..Default::default()
        });
        let s = serde_yaml_ng::to_string(&p).unwrap();
        assert!(
            s.contains("fallback_chain:"),
            "yaml must contain fallback_chain"
        );
        assert!(s.contains("routing:"), "yaml must contain routing");
        let p2: AgentProfile = serde_yaml_ng::from_str(&s).unwrap();
        assert_eq!(
            p2.fallback_chain,
            vec!["claude_opus", "claude_sonnet"],
            "fallback_chain must round-trip"
        );
        assert_eq!(
            p2.routing.as_ref().unwrap().enabled,
            Some(true),
            "routing.enabled must round-trip"
        );
    }

    #[test]
    fn effective_smart_prefers_the_promoted_field_then_the_legacy_nesting() {
        use crate::config::{ModelSwitchConfig, SmartConfig, SmartOverride};
        let cfg = ModelSwitchConfig {
            smart: SmartConfig {
                enabled: false,
                cheap: Some("g".into()),
                max_escalations: 2,
            },
            ..Default::default()
        };
        // No override at all → the global values, untouched.
        let p = AgentProfile::default_for_tests();
        assert_eq!(p.effective_smart(&cfg), cfg.smart);

        // Legacy profiles carry the override nested under `routing`.
        let mut legacy = AgentProfile::default_for_tests();
        legacy.routing = Some(crate::config::RoutingOverride {
            smart: Some(SmartOverride {
                enabled: Some(true),
                ..Default::default()
            }),
            ..Default::default()
        });
        assert!(
            legacy.effective_smart(&cfg).enabled,
            "legacy nesting is read"
        );
        assert_eq!(
            legacy.effective_smart(&cfg).cheap.as_deref(),
            Some("g"),
            "unset fields still inherit"
        );

        // The promoted field wins when both are present.
        let mut both = legacy.clone();
        both.smart = Some(SmartOverride {
            enabled: Some(false),
            ..Default::default()
        });
        assert!(!both.effective_smart(&cfg).enabled);
    }
}

#[cfg(test)]
mod skill_card_tests {
    use super::*;

    #[test]
    fn installed_skills_default_to_empty_when_absent() {
        let yaml = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
        let p: AgentProfile = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(p.installed_skills.is_empty());
    }

    #[test]
    fn installed_skills_roundtrip_preserves_entries() {
        let base = include_str!("../../tests/fixtures/profile_p0a_minimal.yaml");
        let yaml = format!(
            "{base}installed_skills:\n  - name: s1\n    version: 1.0.0\n    publisher: human:d\n    description: desc\n    category: workflow\n    tags: [web]\n    triggers:\n      - type: command\n        pattern: /find\n    abstract: does things\n    transfer_chain:\n      - agent://alice\n"
        );
        let p: AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert_eq!(p.installed_skills.len(), 1);
        assert_eq!(p.installed_skills[0].name, "s1");
        assert_eq!(p.installed_skills[0].abstract_text, "does things");
        assert_eq!(p.installed_skills[0].transfer_chain, vec!["agent://alice"]);

        let out = serde_yaml_ng::to_string(&p).unwrap();
        assert!(out.contains("abstract: does things"));
        assert!(out.contains("pattern: /find"));

        let back: AgentProfile = serde_yaml_ng::from_str(&out).unwrap();
        assert_eq!(p.installed_skills, back.installed_skills);
    }

    #[test]
    fn installed_skills_minimal_entry_serializes_compactly() {
        // A name-only entry must NOT emit empty string fields.
        let entry = SkillCardEntry {
            name: "minimal".into(),
            ..Default::default()
        };
        let yaml = serde_yaml_ng::to_string(&entry).unwrap();
        assert!(yaml.contains("name: minimal"));
        assert!(
            !yaml.contains("version:"),
            "empty version must be skipped: {yaml}"
        );
        assert!(
            !yaml.contains("publisher:"),
            "empty publisher must be skipped: {yaml}"
        );
        assert!(
            !yaml.contains("abstract:"),
            "empty abstract must be skipped: {yaml}"
        );
    }
}

#[cfg(test)]
mod secrets_field_tests {
    /// The list is NAMES only and must stay absent from the YAML when empty:
    /// every existing profile on disk is rewritten by unrelated edits, and a
    /// new always-present key would churn all of them.
    #[test]
    fn secrets_names_round_trip_and_are_absent_when_empty() {
        let mut p = crate::agent::AgentProfile::default_for_tests();
        let yaml = serde_yaml::to_string(&p).unwrap();
        assert!(
            !yaml.contains("secrets:"),
            "empty list must not be written: {yaml}"
        );
        p.secrets = vec!["GITEA_TOKEN".into()];
        let yaml = serde_yaml::to_string(&p).unwrap();
        let back: crate::agent::AgentProfile = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(back.secrets, vec!["GITEA_TOKEN".to_string()]);
    }
}
