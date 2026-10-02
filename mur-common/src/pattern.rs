use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

use crate::knowledge::KnowledgeBase;

/// Pattern schema version
pub const SCHEMA_VERSION: u32 = 3;

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
    /// Automatically generated (e.g. starter patterns)
    Automatic,
}

/// Provenance metadata — where and how a pattern was learned.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Origin {
    /// Which tool created this pattern (e.g. "commander", "claude-code")
    pub source: String,
    /// How the knowledge was captured
    pub trigger: OriginTrigger,

    /// Who/what produced this origin event — preferred successor to
    /// `user`/`platform`. Optional for backward compat with pre-sync YAML.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actor: Option<crate::Actor>,

    /// Legacy free-form user identifier. Prefer [`Self::actor`].
    #[deprecated(note = "use actor instead, removed in v2.3")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,

    /// Legacy free-form platform identifier. Prefer [`Self::actor`].
    #[deprecated(note = "use actor.source instead, removed in v2.3")]
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
    /// Get the full content as a single string (for embedding).
    ///
    /// Returns `Cow::Borrowed` for `Plain` and `DualLayer` without principle,
    /// avoiding allocation in the common case.
    pub fn as_text(&self) -> Cow<'_, str> {
        match self {
            Content::DualLayer {
                technical,
                principle,
            } => match principle {
                Some(p) => Cow::Owned(format!("{}\n\n{}", technical, p)),
                None => Cow::Borrowed(technical),
            },
            Content::Plain(s) => Cow::Borrowed(s),
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

/// Per-actor contribution to a pattern's Evidence.
///
/// Stored in `Evidence.contributions` keyed by [`crate::Actor::key`].
/// Allows effectiveness to be computed per-actor for Team pattern leaderboards
/// or personalized retrieval in future Phase 2 work.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Contribution {
    #[serde(default)]
    pub success_signals: u64,
    #[serde(default)]
    pub override_signals: u64,
    pub last_seen: DateTime<Utc>,
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
    pub failure_signals: u64,
    #[serde(default)]
    pub override_signals: u64,
    /// Per-actor signal counts, keyed by `Actor::key()` (e.g. `"Slack:U123ABC"`).
    /// Empty for patterns that have never been touched by the sync protocol.
    #[serde(default)]
    pub contributions: HashMap<String, Contribution>,
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

    /// Per-actor effectiveness ratio computed from `contributions`.
    ///
    /// If actor has contributed to this pattern, returns their local
    /// `success / (success + override)` ratio. If unknown, returns
    /// neutral prior of 0.5.
    pub fn effectiveness_by_actor(&self, actor: &crate::Actor) -> f64 {
        match self.contributions.get(&actor.key()) {
            Some(c) => {
                let total = c.success_signals + c.override_signals;
                if total == 0 {
                    0.5 // neutral prior if actor present but no signals yet
                } else {
                    c.success_signals as f64 / total as f64
                }
            }
            None => 0.5, // neutral prior
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
mod tests;
