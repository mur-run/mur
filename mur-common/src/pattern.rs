use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Pattern schema version
pub const SCHEMA_VERSION: u32 = 2;

/// A MUR pattern — the atomic unit of learned knowledge.
///
/// YAML files in `~/.mur/patterns/` are the source of truth.
/// LanceDB indexes are always rebuildable from these.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    /// Schema version (2 for v2)
    #[serde(default = "default_schema")]
    pub schema: u32,

    /// Unique identifier (kebab-case, e.g. "swift-testing-macro")
    pub name: String,

    /// Human-readable one-line description
    pub description: String,

    /// Dual-layer content (technical + principle)
    pub content: Content,

    /// Pattern tier: session → project → core
    #[serde(default)]
    pub tier: Tier,

    /// Importance score (0.0-1.0), adjusted by feedback
    #[serde(default = "default_importance")]
    pub importance: f64,

    /// Extraction confidence (0.0-1.0)
    #[serde(default = "default_confidence")]
    pub confidence: f64,

    /// Classification tags
    #[serde(default)]
    pub tags: Tags,

    /// Scope: where this pattern applies
    #[serde(default)]
    pub applies: Applies,

    /// Usage evidence and effectiveness tracking
    #[serde(default)]
    pub evidence: Evidence,

    /// Connections to other patterns (Zettelkasten-style)
    #[serde(default)]
    pub links: Links,

    /// Lifecycle management
    #[serde(default)]
    pub lifecycle: Lifecycle,

    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,

    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
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

fn default_schema() -> u32 {
    SCHEMA_VERSION
}
fn default_importance() -> f64 {
    0.5
}
fn default_confidence() -> f64 {
    0.5
}
