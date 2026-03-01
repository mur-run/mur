use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::knowledge::KnowledgeBase;

/// Pattern schema version
pub const SCHEMA_VERSION: u32 = 2;

/// The kind of knowledge a pattern represents.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum PatternKind {
    /// Technical knowledge (code patterns, architecture, tools)
    #[default]
    Technical,
    /// User preference (language, style, format)
    Preference,
    /// Factual knowledge (server addresses, config values)
    Fact,
    /// Procedural knowledge (how-to steps, workflows)
    Procedure,
    /// Behavioral rules (do/don't rules for interaction)
    Behavioral,
}

/// How a pattern's knowledge was originally captured.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum OriginTrigger {
    /// User explicitly said "remember this"
    UserExplicit,
    /// User corrected the AI's behavior
    UserCorrection,
    /// Agent inferred from behavior patterns
    AgentInferred,
    /// Shared from community
    CommunityShared,
    /// Auto-consolidated during memory consolidation
    AutoConsolidated,
}

/// Provenance metadata — where and how a pattern was learned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Origin {
    /// Which tool created this pattern (e.g. "commander", "claude-code")
    pub source: String,
    /// How the knowledge was captured
    pub trigger: OriginTrigger,
    /// Which user this knowledge is about/from
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    /// Platform where it was learned (e.g. "slack", "terminal")
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
    /// Extraction confidence (0.0-1.0) — how sure the tool was about the extraction
    #[serde(default = "default_origin_confidence")]
    pub confidence: f64,
}

fn default_origin_confidence() -> f64 {
    1.0
}

/// A MUR pattern — the atomic unit of learned knowledge.
///
/// YAML files in `~/.mur/patterns/` are the source of truth.
/// LanceDB indexes are always rebuildable from these.
///
/// KnowledgeBase fields are flattened so existing YAML stays compatible.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    /// Shared knowledge fields (flattened into YAML)
    #[serde(flatten)]
    pub base: KnowledgeBase,

    /// The kind of knowledge this pattern represents.
    /// None is treated as Technical for backward compatibility.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<PatternKind>,

    /// Provenance metadata — where and how this pattern was learned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub origin: Option<Origin>,

    /// Attached diagrams, images, etc.
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}

impl Pattern {
    /// Get the effective kind, defaulting to Technical if not set.
    pub fn effective_kind(&self) -> PatternKind {
        self.kind.unwrap_or(PatternKind::Technical)
    }
}

// Allow `pattern.name`, `pattern.content`, etc. via auto-deref.
impl std::ops::Deref for Pattern {
    type Target = KnowledgeBase;
    fn deref(&self) -> &KnowledgeBase {
        &self.base
    }
}
impl std::ops::DerefMut for Pattern {
    fn deref_mut(&mut self) -> &mut KnowledgeBase {
        &mut self.base
    }
}

/// An attachment to a pattern (diagram, image, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Attachment {
    /// Type of attachment
    #[serde(rename = "type")]
    pub att_type: AttachmentType,
    /// Format of the attachment
    pub format: AttachmentFormat,
    /// Path to the attachment file (relative to ~/.mur/)
    pub path: String,
    /// Human-readable description
    #[serde(default)]
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentType {
    Diagram,
    Image,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum AttachmentFormat {
    Mermaid,
    #[serde(rename = "plantuml")]
    PlantUml,
    Png,
    Svg,
}

impl AttachmentFormat {
    /// Whether this format is text-based (can be inlined into prompts).
    pub fn is_text_based(&self) -> bool {
        matches!(self, AttachmentFormat::Mermaid | AttachmentFormat::PlantUml)
    }

    /// Detect format from file extension.
    pub fn from_extension(ext: &str) -> Option<Self> {
        match ext.to_lowercase().as_str() {
            "mmd" | "mermaid" => Some(AttachmentFormat::Mermaid),
            "puml" | "plantuml" => Some(AttachmentFormat::PlantUml),
            "png" => Some(AttachmentFormat::Png),
            "svg" => Some(AttachmentFormat::Svg),
            _ => None,
        }
    }

    /// The markdown code fence language tag for text-based formats.
    pub fn fence_lang(&self) -> &str {
        match self {
            AttachmentFormat::Mermaid => "mermaid",
            AttachmentFormat::PlantUml => "plantuml",
            _ => "",
        }
    }
}

impl AttachmentType {
    /// Infer attachment type from format.
    pub fn from_format(format: &AttachmentFormat) -> Self {
        match format {
            AttachmentFormat::Mermaid | AttachmentFormat::PlantUml => AttachmentType::Diagram,
            AttachmentFormat::Png | AttachmentFormat::Svg => AttachmentType::Image,
        }
    }
}

/// Dual-layer content inspired by LanceDB Pro Plugin Rule 6.
/// Max 500 chars per layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Content {
    /// v2: dual-layer
    DualLayer {
        technical: String,
        #[serde(default)]
        principle: Option<String>,
    },
    /// v1 compat: single string
    Plain(String),
}

impl Default for Content {
    fn default() -> Self {
        Content::Plain(String::new())
    }
}

impl Content {
    /// Get the full content as a single string (for embedding)
    pub fn as_text(&self) -> String {
        match self {
            Content::DualLayer {
                technical,
                principle,
            } => match principle {
                Some(p) => format!("{}\n\n{}", technical, p),
                None => technical.clone(),
            },
            Content::Plain(s) => s.clone(),
        }
    }

    /// Max chars per content layer
    pub const MAX_LAYER_CHARS: usize = 500;
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Tier {
    /// Short-lived, from a single session. Decay: 14 days half-life.
    #[default]
    Session,
    /// Validated project convention. Decay: 90 days half-life.
    Project,
    /// Cross-project core preference. Decay: 365 days half-life.
    Core,
}

impl Tier {
    /// Half-life in days for decay calculation
    pub fn decay_half_life_days(&self) -> u32 {
        match self {
            Tier::Session => 14,
            Tier::Project => 90,
            Tier::Core => 365,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Tags {
    #[serde(default)]
    pub languages: Vec<String>,
    #[serde(default)]
    pub topics: Vec<String>,
    /// Extra user-defined tags
    #[serde(flatten)]
    pub extra: HashMap<String, Vec<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Applies {
    /// Project names or ["*"] for universal
    #[serde(default)]
    pub projects: Vec<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    /// Only inject when using these tools (e.g. "claude-code")
    #[serde(default)]
    pub tools: Vec<String>,
    /// Auto-detect scope from pwd/git remote
    #[serde(default)]
    pub auto_scope: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Evidence {
    #[serde(default)]
    pub source_sessions: Vec<String>,
    pub first_seen: Option<DateTime<Utc>>,
    pub last_validated: Option<DateTime<Utc>>,
    #[serde(default)]
    pub injection_count: u64,
    #[serde(default)]
    pub success_signals: u64,
    #[serde(default)]
    pub override_signals: u64,
}

impl Evidence {
    /// Effectiveness ratio: success / (success + override)
    pub fn effectiveness(&self) -> f64 {
        let total = self.success_signals + self.override_signals;
        if total == 0 {
            0.5 // neutral prior
        } else {
            self.success_signals as f64 / total as f64
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Links {
    /// Related patterns (bidirectional)
    #[serde(default)]
    pub related: Vec<String>,
    /// Patterns this one replaces
    #[serde(default)]
    pub supersedes: Vec<String>,
    /// MUR Commander workflow references (future)
    #[serde(default)]
    pub workflows: Vec<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Lifecycle {
    #[serde(default)]
    pub status: LifecycleStatus,
    /// Custom decay half-life override (days). If None, uses Tier default.
    pub decay_half_life: Option<u32>,
    pub last_injected: Option<DateTime<Utc>>,
    /// Pinned by user — never auto-deprecated
    #[serde(default)]
    pub pinned: bool,
    /// Muted by user — skip injection but don't delete
    #[serde(default)]
    pub muted: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum LifecycleStatus {
    #[default]
    Active,
    Deprecated,
    Archived,
}

pub fn default_schema() -> u32 {
    SCHEMA_VERSION
}
pub fn default_importance() -> f64 {
    0.5
}
pub fn default_confidence() -> f64 {
    0.5
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_attachment_format_is_text_based() {
        assert!(AttachmentFormat::Mermaid.is_text_based());
        assert!(AttachmentFormat::PlantUml.is_text_based());
        assert!(!AttachmentFormat::Png.is_text_based());
        assert!(!AttachmentFormat::Svg.is_text_based());
    }

    #[test]
    fn test_attachment_format_from_extension() {
        assert_eq!(
            AttachmentFormat::from_extension("mmd"),
            Some(AttachmentFormat::Mermaid)
        );
        assert_eq!(
            AttachmentFormat::from_extension("mermaid"),
            Some(AttachmentFormat::Mermaid)
        );
        assert_eq!(
            AttachmentFormat::from_extension("puml"),
            Some(AttachmentFormat::PlantUml)
        );
        assert_eq!(
            AttachmentFormat::from_extension("plantuml"),
            Some(AttachmentFormat::PlantUml)
        );
        assert_eq!(
            AttachmentFormat::from_extension("png"),
            Some(AttachmentFormat::Png)
        );
        assert_eq!(
            AttachmentFormat::from_extension("svg"),
            Some(AttachmentFormat::Svg)
        );
        assert_eq!(AttachmentFormat::from_extension("jpg"), None);
        assert_eq!(AttachmentFormat::from_extension(""), None);
        // Case insensitive
        assert_eq!(
            AttachmentFormat::from_extension("MMD"),
            Some(AttachmentFormat::Mermaid)
        );
    }

    #[test]
    fn test_attachment_format_fence_lang() {
        assert_eq!(AttachmentFormat::Mermaid.fence_lang(), "mermaid");
        assert_eq!(AttachmentFormat::PlantUml.fence_lang(), "plantuml");
        assert_eq!(AttachmentFormat::Png.fence_lang(), "");
    }

    #[test]
    fn test_attachment_type_from_format() {
        assert_eq!(
            AttachmentType::from_format(&AttachmentFormat::Mermaid),
            AttachmentType::Diagram
        );
        assert_eq!(
            AttachmentType::from_format(&AttachmentFormat::PlantUml),
            AttachmentType::Diagram
        );
        assert_eq!(
            AttachmentType::from_format(&AttachmentFormat::Png),
            AttachmentType::Image
        );
        assert_eq!(
            AttachmentType::from_format(&AttachmentFormat::Svg),
            AttachmentType::Image
        );
    }

    #[test]
    fn test_attachment_serde() {
        let att = Attachment {
            att_type: AttachmentType::Diagram,
            format: AttachmentFormat::Mermaid,
            path: "my-pattern/arch.mermaid".to_string(),
            description: "Architecture diagram".to_string(),
        };

        let yaml = serde_yaml::to_string(&att).unwrap();
        assert!(yaml.contains("type: diagram"));
        assert!(yaml.contains("format: mermaid"));
        assert!(yaml.contains("path: my-pattern/arch.mermaid"));
        assert!(yaml.contains("description: Architecture diagram"));

        let deserialized: Attachment = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.att_type, AttachmentType::Diagram);
        assert_eq!(deserialized.format, AttachmentFormat::Mermaid);
    }

    #[test]
    fn test_attachment_svg_serde() {
        let att = Attachment {
            att_type: AttachmentType::Image,
            format: AttachmentFormat::Svg,
            path: "my-pattern/logo.svg".to_string(),
            description: "Logo".to_string(),
        };

        let yaml = serde_yaml::to_string(&att).unwrap();
        let deserialized: Attachment = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.format, AttachmentFormat::Svg);
        assert_eq!(deserialized.att_type, AttachmentType::Image);
    }

    #[test]
    fn test_pattern_kind_serde_roundtrip() {
        // All variants serialize to lowercase
        let cases = vec![
            (PatternKind::Technical, "technical"),
            (PatternKind::Preference, "preference"),
            (PatternKind::Fact, "fact"),
            (PatternKind::Procedure, "procedure"),
            (PatternKind::Behavioral, "behavioral"),
        ];
        for (kind, expected_str) in cases {
            let yaml = serde_yaml::to_string(&kind).unwrap();
            assert!(
                yaml.contains(expected_str),
                "Expected '{}' in '{}'",
                expected_str,
                yaml
            );
            let deserialized: PatternKind = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(deserialized, kind);
        }
    }

    #[test]
    fn test_origin_trigger_serde_roundtrip() {
        let cases = vec![
            (OriginTrigger::UserExplicit, "user_explicit"),
            (OriginTrigger::UserCorrection, "user_correction"),
            (OriginTrigger::AgentInferred, "agent_inferred"),
            (OriginTrigger::CommunityShared, "community_shared"),
            (OriginTrigger::AutoConsolidated, "auto_consolidated"),
        ];
        for (trigger, expected_str) in cases {
            let yaml = serde_yaml::to_string(&trigger).unwrap();
            assert!(yaml.contains(expected_str));
            let deserialized: OriginTrigger = serde_yaml::from_str(&yaml).unwrap();
            assert_eq!(deserialized, trigger);
        }
    }

    #[test]
    fn test_origin_serde_roundtrip() {
        let origin = Origin {
            source: "commander".to_string(),
            trigger: OriginTrigger::UserExplicit,
            user: Some("david".to_string()),
            platform: Some("slack".to_string()),
            confidence: 0.95,
        };
        let yaml = serde_yaml::to_string(&origin).unwrap();
        assert!(yaml.contains("source: commander"));
        assert!(yaml.contains("trigger: user_explicit"));
        assert!(yaml.contains("user: david"));
        assert!(yaml.contains("platform: slack"));

        let deserialized: Origin = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.source, "commander");
        assert_eq!(deserialized.trigger, OriginTrigger::UserExplicit);
        assert_eq!(deserialized.user, Some("david".to_string()));
        assert_eq!(deserialized.platform, Some("slack".to_string()));
        assert!((deserialized.confidence - 0.95).abs() < 0.001);
    }

    #[test]
    fn test_origin_optional_fields_omitted() {
        let origin = Origin {
            source: "cli".to_string(),
            trigger: OriginTrigger::AgentInferred,
            user: None,
            platform: None,
            confidence: 1.0,
        };
        let yaml = serde_yaml::to_string(&origin).unwrap();
        assert!(!yaml.contains("user:"));
        assert!(!yaml.contains("platform:"));
    }

    #[test]
    fn test_pattern_with_kind_and_origin_roundtrip() {
        use crate::knowledge::KnowledgeBase;
        let pattern = Pattern {
            base: KnowledgeBase {
                name: "test-pref".into(),
                description: "A preference".into(),
                content: Content::Plain("Use Chinese".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            origin: Some(Origin {
                source: "commander".into(),
                trigger: OriginTrigger::UserExplicit,
                user: Some("david".into()),
                platform: None,
                confidence: 0.9,
            }),
            attachments: vec![],
        };

        let yaml = serde_yaml::to_string(&pattern).unwrap();
        assert!(yaml.contains("kind: preference"));
        assert!(yaml.contains("source: commander"));

        let deserialized: Pattern = serde_yaml::from_str(&yaml).unwrap();
        assert_eq!(deserialized.kind, Some(PatternKind::Preference));
        assert_eq!(deserialized.effective_kind(), PatternKind::Preference);
        assert!(deserialized.origin.is_some());
        assert_eq!(deserialized.origin.unwrap().source, "commander");
    }

    #[test]
    fn test_pattern_backward_compat_no_kind_no_origin() {
        // Existing YAML without kind/origin fields should deserialize fine
        let yaml = "name: old-pattern\ndescription: Old\ncontent: Some content\n";
        let pattern: Pattern = serde_yaml::from_str(yaml).unwrap();
        assert_eq!(pattern.name, "old-pattern");
        assert!(pattern.kind.is_none());
        assert!(pattern.origin.is_none());
        assert_eq!(pattern.effective_kind(), PatternKind::Technical);
    }

    #[test]
    fn test_pattern_kind_default() {
        assert_eq!(PatternKind::default(), PatternKind::Technical);
    }
}
