//! Skill manifest — full serde representation of canonical `skill.yaml`.

use super::evolution::EvolutionEvent;
use super::mcp::McpRequirement;
use super::types::{Category, ContentMode, HostId, Priority, Provenance, TriggerKind, TrustLevel};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Top-level skill — wraps the manifest with security metadata that lives
/// alongside (but separate from) the publisher-authored fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Skill {
    #[serde(flatten)]
    pub manifest: SkillManifest,

    /// Computed at install time. Never serialized into the source YAML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_sha256: Option<String>,

    /// Set by the trust store at install time, not by the publisher.
    #[serde(default)]
    pub trust_level: TrustLevel,

    /// Capabilities the skill declares it needs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub capabilities_declared: Vec<String>,

    /// DSSE envelope JSON (base64-encoded inside the envelope). `None` for
    /// unsigned skills — they enter at Sandboxed and stay there.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publisher_signature: Option<String>,
}

/// Publisher-authored fields. This is what gets signed and is the unit of
/// content hashing.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SkillManifest {
    pub name: String,
    pub version: String,
    pub publisher: String,
    pub description: String,
    pub category: Category,

    /// Origin of this skill. Defaults to `Human` so every existing manifest
    /// (which has no `provenance:` key) parses as human-authored.
    #[serde(default)]
    pub provenance: Provenance,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hosts: Vec<HostId>,

    pub content: Content,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires: Vec<Requirement>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub triggers: Vec<Trigger>,

    #[serde(default)]
    pub priority: Priority,

    /// Evolution history — each entry records one generation.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evolution_log: Vec<EvolutionEvent>,

    /// Peer transfer provenance — each entry is `agent://<name>`.
    /// Last entry is the immediate source; first entry is the original publisher.
    /// Empty for registry-installed and locally-authored skills.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub transfer_chain: Vec<String>,

    /// MCP tool capabilities this skill needs at runtime. Optional; absent
    /// in M3-era v2.0 manifests. Added in schema v2.1.
    ///
    /// **Signature scope:** signed as part of the manifest. Changing
    /// `mcp_requirements` invalidates an existing publisher signature.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp_requirements: Vec<McpRequirement>,

    /// Timestamp of last modification (for fleet-sync LWW). Used by
    /// `resolve_manifest_lww()` for conflict resolution. Defaults to the Unix epoch
    /// on deserialization if absent (for backwards compat with unsigned skills).
    #[serde(default)]
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    /// Layer 2 — injected into the system prompt at session start.
    pub r#abstract: String,

    /// Exactly one of the following is `Some`. Enforced by schema validation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procedure: Option<Procedure>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Note mode (category: note): free markdown body, stored inline in the
    /// canonical skill.yaml per the 1a storage decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl Content {
    pub fn mode(&self) -> Option<ContentMode> {
        match (
            self.context.is_some(),
            self.procedure.is_some(),
            self.command.is_some(),
            self.note.is_some(),
        ) {
            (true, false, false, false) => Some(ContentMode::Context),
            (false, true, false, false) => Some(ContentMode::Workflow),
            (false, false, true, false) => Some(ContentMode::Command),
            (false, false, false, true) => Some(ContentMode::Note),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Procedure {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
    pub steps: Vec<ProcedureStep>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variable {
    pub name: String,
    #[serde(rename = "type")]
    pub var_type: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<serde_yaml_ng::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcedureStep {
    pub description: String,

    /// Literal tool name. Pre-M6b behaviour: hard binding. Post-M6b: treated
    /// as a hint when `intent` is also set; otherwise still a hard binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,

    /// What the step is trying to accomplish. Free-form string, no central
    /// taxonomy. Resolved at inject time against the agent's MCP inventory.
    /// When set, the resolver prefers a tool whose name matches a glob in
    /// `mcp_requirements` over the literal `tool` field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,

    /// Preferred tool name pattern (glob). Used as a tiebreaker among
    /// resolver candidates. Falls back to literal `tool`, then to any
    /// `mcp_requirements` match for the intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_hint: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    #[serde(rename = "type")]
    pub kind: TriggerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
}

impl Trigger {
    /// Returns the keyword string for `Keyword` triggers, `None` otherwise.
    pub fn exact_keyword(&self) -> Option<&str> {
        if matches!(self.kind, TriggerKind::Keyword) {
            self.pattern.as_deref()
        } else {
            None
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Requirement {
    pub name: String,
    #[serde(default = "default_any_version")]
    pub version: String,
}

fn default_any_version() -> String {
    "*".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_manifest_roundtrips() {
        let yaml = r#"
name: research-prices
version: 1.0.0
publisher: human:david
description: Search product prices
category: workflow
hosts: [mur-agent]
content:
  abstract: Searches product prices.
  procedure:
    variables:
      - name: product_name
        type: string
        required: true
    steps:
      - description: Navigate
        tool: browser.navigate
triggers:
  - type: command
    pattern: /research-prices
priority: normal
"#;
        let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(m.name, "research-prices");
        assert_eq!(m.category, Category::Workflow);
        assert_eq!(m.content.mode(), Some(ContentMode::Workflow));
        let back = serde_yaml_ng::to_string(&m).unwrap();
        let m2: SkillManifest = serde_yaml_ng::from_str(&back).unwrap();
        assert_eq!(m2.name, m.name);
    }

    #[test]
    fn context_mode_detected() {
        let c = Content {
            r#abstract: "a".into(),
            context: Some("ctx".into()),
            procedure: None,
            command: None,
            note: None,
        };
        assert_eq!(c.mode(), Some(ContentMode::Context));
    }

    #[test]
    fn empty_content_returns_no_mode() {
        let c = Content {
            r#abstract: "a".into(),
            context: None,
            procedure: None,
            command: None,
            note: None,
        };
        assert_eq!(c.mode(), None);
    }

    #[test]
    fn mode_returns_note_when_only_note_populated() {
        let c = Content {
            r#abstract: "a".into(),
            context: None,
            procedure: None,
            command: None,
            note: Some("# body".into()),
        };
        assert_eq!(c.mode(), Some(ContentMode::Note));
    }

    #[test]
    fn mode_returns_none_when_note_and_context_both_populated() {
        let c = Content {
            r#abstract: "a".into(),
            context: Some("ctx".into()),
            procedure: None,
            command: None,
            note: Some("# body".into()),
        };
        assert_eq!(c.mode(), None);
    }

    #[test]
    fn skill_without_evolution_log_defaults_to_empty() {
        // YAML without evolution_log field must parse and default to vec![].
        let yaml = r#"
name: no-evol
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: test
"#;
        let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
        assert!(m.evolution_log.is_empty());
    }

    #[test]
    fn skill_with_evolution_log_roundtrips() {
        let yaml = r#"
name: with-evol
version: 0.1.0
publisher: human:test
description: test
category: workflow
content:
  abstract: test
evolution_log:
  - version: "0.1.0"
    generation: 0
    source: "human:test"
    changes: "Initial"
    timestamp: "2026-01-01T00:00:00Z"
"#;
        let m: SkillManifest = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(m.evolution_log.len(), 1);
        assert_eq!(m.evolution_log[0].version, "0.1.0");
        // Round-trip.
        let back = serde_yaml_ng::to_string(&m).unwrap();
        let m2: SkillManifest = serde_yaml_ng::from_str(&back).unwrap();
        assert_eq!(m2.evolution_log.len(), 1);
        assert_eq!(m2.evolution_log[0].generation, 0);
    }

    #[test]
    fn exact_keyword_returns_pattern_for_keyword_triggers() {
        let t = Trigger {
            kind: TriggerKind::Keyword,
            pattern: Some("search".into()),
        };
        assert_eq!(t.exact_keyword(), Some("search"));
    }

    #[test]
    fn exact_keyword_returns_none_for_non_keyword_triggers() {
        let t = Trigger {
            kind: TriggerKind::Command,
            pattern: Some("run".into()),
        };
        assert_eq!(t.exact_keyword(), None);

        let t = Trigger {
            kind: TriggerKind::SessionStart,
            pattern: None,
        };
        assert_eq!(t.exact_keyword(), None);

        let t = Trigger {
            kind: TriggerKind::Manual,
            pattern: None,
        };
        assert_eq!(t.exact_keyword(), None);
    }

    #[test]
    fn exact_keyword_returns_none_when_pattern_is_none() {
        let t = Trigger {
            kind: TriggerKind::Keyword,
            pattern: None,
        };
        assert_eq!(t.exact_keyword(), None);
    }
}
