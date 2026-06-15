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
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct Procedure {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<Variable>,
    pub steps: Vec<ProcedureStep>,
}

/// Commander extension: retry configuration for a workflow step.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct RetryConfig {
    pub max_retries: u32,
    #[serde(default)]
    pub backoff_secs: Option<u64>,
}

/// What to do when a workflow step fails.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum FailureAction {
    /// Skip this step and continue
    Skip,
    /// Abort the entire workflow
    #[default]
    Abort,
    /// Retry the step
    Retry,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
pub struct Variable {
    pub name: String,
    #[serde(rename = "type", default)]
    pub var_type: VarType,
    #[serde(default)]
    pub required: bool,
    /// String-encoded default. `default_value` accepted for legacy workflow YAML.
    /// Runtime coerces per `var_type` (Number/Bool parsed, Array decoded as
    /// JSON or comma-separated).
    #[serde(
        default,
        alias = "default_value",
        skip_serializing_if = "Option::is_none"
    )]
    pub default: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Allowed values (renders as a dropdown in the Hub DAG editor).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
}

/// Variable types for workflow/skill parameters (v2 resolved decision #3:
/// ONE `Variable` type lives here; `workflow::Variable` re-exports it).
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, schemars::JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum VarType {
    #[default]
    String,
    Path,
    Url,
    Number,
    Bool,
    /// Array of strings (e.g., multiple URLs, multiple product names)
    Array,
}

impl std::fmt::Display for VarType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VarType::String => write!(f, "string"),
            VarType::Path => write!(f, "path"),
            VarType::Url => write!(f, "url"),
            VarType::Number => write!(f, "number"),
            VarType::Bool => write!(f, "bool"),
            VarType::Array => write!(f, "array"),
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, schemars::JsonSchema)]
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

    // ── Executable-DAG fields (workflow-engine v2 P2; all default so every
    //    existing skill.yaml parses unchanged) ──
    /// Stable step id for `depends_on` references. When omitted, executors
    /// assign the zero-based step index as the id at load time (not serialized).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,

    /// Step ids this step depends on. Empty = root step. Step order derives
    /// from the dependency topology, never from list position (v2 decision #1).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Shell command (command-mode step), run via `sh -c` with exit-code
    /// gating. Intent-mode steps leave this None — in pure CLI runs they are
    /// printed as instructions and marked skipped in the ledger (decision #2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    #[serde(default)]
    pub on_failure: FailureAction,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retry: Option<RetryConfig>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,

    /// Pause for human approval before running. TTY: prompt and wait.
    /// Non-TTY: auto-skip and mark `skipped_approval` in the ledger; `--yes`
    /// auto-approves (v2 decision #5). Wired by the P3 executor.
    #[serde(default)]
    pub needs_approval: bool,

    /// Delegate this step's sub-goal to a specialist MUR agent over A2A
    /// (v3b, Channel mode). When set, the channel-aware executor dials this
    /// agent via `message/send` instead of running `command`/`intent`, and
    /// attributes the reply to `Agent{<canonical agent name>}` in the channel.
    /// Ignored when the executor runs without a channel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegate_to: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
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
    fn procedure_step_dag_fields_roundtrip() {
        let yaml = r#"
description: deploy the app
command: "fly deploy --app {{app_name}}"
id: deploy
depends_on: [build, test]
on_failure: retry
retry:
  max_retries: 2
  backoff_secs: 5
timeout_secs: 300
needs_approval: true
"#;
        let step: ProcedureStep = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(step.id.as_deref(), Some("deploy"));
        assert_eq!(step.depends_on, vec!["build", "test"]);
        assert_eq!(step.on_failure, FailureAction::Retry);
        assert_eq!(step.retry.as_ref().unwrap().max_retries, 2);
        assert_eq!(step.timeout_secs, Some(300));
        assert!(step.needs_approval);

        // Legacy step without any DAG fields parses with defaults.
        let legacy: ProcedureStep =
            serde_yaml_ng::from_str("description: run tests\ntool: Bash\n").unwrap();
        assert!(legacy.id.is_none());
        assert!(legacy.depends_on.is_empty());
        assert_eq!(legacy.on_failure, FailureAction::Abort);
        assert!(!legacy.needs_approval);
    }

    #[test]
    fn procedure_step_parses_delegate_to() {
        let yaml = "description: hand off to qa\ndelegate_to: qa\n";
        let s: ProcedureStep = serde_yaml_ng::from_str(yaml).unwrap();
        assert_eq!(s.delegate_to.as_deref(), Some("qa"));
        // Absent → None (every existing skill.yaml still parses).
        let s2: ProcedureStep = serde_yaml_ng::from_str("description: local step\n").unwrap();
        assert_eq!(s2.delegate_to, None);
    }

    #[test]
    fn variable_accepts_legacy_default_value_alias() {
        // Legacy workflow YAML used `default_value`; the unified type aliases it.
        let v: Variable = serde_yaml_ng::from_str(
            "name: app\ntype: string\nrequired: true\ndefault_value: my-api\n",
        )
        .unwrap();
        assert_eq!(v.default.as_deref(), Some("my-api"));
        assert_eq!(v.var_type, VarType::String);

        // Modern form `default:` parses too, and choices default empty.
        let v2: Variable =
            serde_yaml_ng::from_str("name: env\ntype: string\ndefault: prod\n").unwrap();
        assert_eq!(v2.default.as_deref(), Some("prod"));
        assert!(v2.choices.is_empty());
    }

    #[test]
    fn variable_all_vartypes_parse() {
        for t in ["string", "path", "url", "number", "bool", "array"] {
            let v: Variable = serde_yaml_ng::from_str(&format!("name: x\ntype: {t}\n")).unwrap();
            assert_eq!(v.var_type.to_string(), t);
        }
    }

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
