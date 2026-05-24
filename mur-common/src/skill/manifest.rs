//! Skill manifest — full serde representation of canonical `skill.yaml`.

use super::types::{Category, ContentMode, HostId, Priority, TriggerKind, TrustLevel};
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

    /// Capabilities the skill declares it needs (see Task 14).
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Content {
    /// Layer 2 — injected into the system prompt at session start.
    pub r#abstract: String,

    /// Exactly one of the following is `Some`. Schema validation (Task 5)
    /// enforces this invariant.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub procedure: Option<Procedure>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

impl Content {
    /// Which content mode is populated.
    pub fn mode(&self) -> Option<ContentMode> {
        match (
            self.context.is_some(),
            self.procedure.is_some(),
            self.command.is_some(),
        ) {
            (true, false, false) => Some(ContentMode::Context),
            (false, true, false) => Some(ContentMode::Workflow),
            (false, false, true) => Some(ContentMode::Command),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Trigger {
    #[serde(rename = "type")]
    pub kind: TriggerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pattern: Option<String>,
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
    use serde_yaml_ng;

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
        };
        assert_eq!(c.mode(), None);
    }
}
