//! KnowledgeBase — the shared foundation for patterns and workflows.
//!
//! Both `Pattern` and `Workflow` embed `KnowledgeBase` via `#[serde(flatten)]`
//! so their YAML stays flat (no nested `base:` key).

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::pattern::{
    Applies, Content, Evidence, Lifecycle, Links, Tags, Tier, default_confidence,
    default_importance, default_schema,
};

/// Maturity level for knowledge items.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum Maturity {
    #[default]
    Draft,
    Emerging,
    Stable,
    Canonical,
}

/// Decay metadata for time-based relevance.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DecayMeta {
    /// Last time this knowledge was actively used/referenced
    pub last_active: Option<DateTime<Utc>>,
    /// Override the tier-based half-life (in days)
    pub half_life_override: Option<u32>,
}

/// Shared fields for all knowledge items (patterns, workflows).
///
/// Embedded via `#[serde(flatten)]` so YAML stays flat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBase {
    /// Schema version (2 for v2)
    #[serde(default = "default_schema")]
    pub schema: u32,

    /// Unique identifier (kebab-case, e.g. "swift-testing-macro")
    pub name: String,

    /// Human-readable one-line description
    pub description: String,

    /// Dual-layer content (technical + principle)
    pub content: Content,

    /// Knowledge tier: session → project → core
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

    /// Scope: where this knowledge applies
    #[serde(default)]
    pub applies: Applies,

    /// Usage evidence and effectiveness tracking
    #[serde(default)]
    pub evidence: Evidence,

    /// Connections to other knowledge items (Zettelkasten-style)
    #[serde(default)]
    pub links: Links,

    /// Lifecycle management
    #[serde(default)]
    pub lifecycle: Lifecycle,

    #[serde(default = "Utc::now")]
    pub created_at: DateTime<Utc>,

    #[serde(default = "Utc::now")]
    pub updated_at: DateTime<Utc>,

    /// Maturity level
    #[serde(default)]
    pub maturity: Maturity,

    /// Decay metadata
    #[serde(default)]
    pub decay: DecayMeta,
}

impl Default for KnowledgeBase {
    fn default() -> Self {
        Self {
            schema: default_schema(),
            name: String::new(),
            description: String::new(),
            content: Content::default(),
            tier: Tier::default(),
            importance: default_importance(),
            confidence: default_confidence(),
            tags: Tags::default(),
            applies: Applies::default(),
            evidence: Evidence::default(),
            links: Links::default(),
            lifecycle: Lifecycle::default(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
            maturity: Maturity::default(),
            decay: DecayMeta::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pattern::Content;

    #[test]
    fn test_knowledgebase_serde_roundtrip() {
        let kb = KnowledgeBase {
            name: "test-kb".into(),
            description: "A test knowledge base".into(),
            content: Content::DualLayer {
                technical: "Use X for Y".into(),
                principle: Some("Because Z".into()),
            },
            tier: Tier::Project,
            importance: 0.8,
            confidence: 0.9,
            maturity: Maturity::Stable,
            ..Default::default()
        };

        let yaml = serde_yaml::to_string(&kb).expect("serialize");
        let kb2: KnowledgeBase = serde_yaml::from_str(&yaml).expect("deserialize");

        assert_eq!(kb2.name, "test-kb");
        assert_eq!(kb2.description, "A test knowledge base");
        assert_eq!(kb2.tier, Tier::Project);
        assert!((kb2.importance - 0.8).abs() < 0.001);
        assert!((kb2.confidence - 0.9).abs() < 0.001);
        assert_eq!(kb2.maturity, Maturity::Stable);
        assert_eq!(kb2.schema, 2);
    }

    #[test]
    fn test_knowledgebase_default() {
        let kb = KnowledgeBase::default();
        assert_eq!(kb.schema, 2);
        assert_eq!(kb.tier, Tier::Session);
        assert!((kb.importance - 0.5).abs() < 0.001);
        assert!((kb.confidence - 0.5).abs() < 0.001);
        assert_eq!(kb.maturity, Maturity::Draft);
    }

    #[test]
    fn test_knowledgebase_minimal_yaml() {
        // Minimal YAML with just required fields; serde defaults fill the rest
        let yaml = "name: minimal\ndescription: Minimal test\ncontent: Just text\n";
        let kb: KnowledgeBase = serde_yaml::from_str(yaml).expect("deserialize minimal");
        assert_eq!(kb.name, "minimal");
        assert_eq!(kb.schema, 2); // default_schema
        assert_eq!(kb.tier, Tier::Session);
    }
}
