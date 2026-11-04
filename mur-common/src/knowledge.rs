//! KnowledgeBase — the shared foundation for workflows (and, formerly, patterns).
//!
//! `Workflow` embeds `KnowledgeBase` via `#[serde(flatten)]` so its YAML stays
//! flat (no nested `base:` key). The value types below (`Content`, `Tier`,
//! `Tags`, `Evidence`, …) used to live in `pattern.rs`; the `Pattern` type was
//! retired in the Pattern→Skill migration (Tier C3) and the shared pieces moved
//! here so nothing has to reach through a dead module.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

use crate::Scope;

/// Knowledge schema version (carried in every `KnowledgeBase::schema`).
pub const SCHEMA_VERSION: u32 = 3;

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

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

/// Shared fields for all knowledge items (patterns, workflows).
///
/// Embedded via `#[serde(flatten)]` so YAML stays flat.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeBase {
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

    /// Ownership/audience scope (Personal / Team / Community).
    /// Orthogonal to `tier` — tier manages temporal half-life, scope manages audience.
    #[serde(default)]
    pub scope: Scope,

    /// Monotonically increasing version counter, managed by VersionedYamlStore.
    /// 0 means "not yet versioned" (pre-schema-3 or pre-bootstrap).
    /// Omitted from YAML when zero so old files stay clean.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub version: u32,

    /// 12-char short SHA of the knowledge-layer commit that wrote this version.
    /// None until the pattern has been committed at least once post-schema-3.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
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
            scope: Scope::default(),
            version: 0,
            revision: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(kb2.schema, 3);
    }

    #[test]
    fn test_knowledgebase_default() {
        let kb = KnowledgeBase::default();
        assert_eq!(kb.schema, 3);
        assert!((kb.confidence - 0.5).abs() < 0.001);
        assert_eq!(kb.maturity, Maturity::Draft);
    }

    #[test]
    fn test_knowledgebase_minimal_yaml() {
        // Minimal YAML with just required fields; serde defaults fill the rest
        let yaml = "name: minimal\ndescription: Minimal test\ncontent: Just text\n";
        let kb: KnowledgeBase = serde_yaml::from_str(yaml).expect("deserialize minimal");
        assert_eq!(kb.name, "minimal");
        assert_eq!(kb.schema, 3); // default_schema
        assert_eq!(kb.tier, Tier::Session);
    }

    #[test]
    fn evidence_contributions_default_empty() {
        let e = Evidence::default();
        assert!(e.contributions.is_empty());
    }

    #[test]
    fn evidence_effectiveness_by_actor_splits_signals() {
        use crate::{Actor, ActorSource};

        let alice = Actor {
            source: ActorSource::Slack,
            native_id: "alice".into(),
            display_name: None,
            resolved_user_id: None,
        };
        let bob = Actor {
            source: ActorSource::Slack,
            native_id: "bob".into(),
            display_name: None,
            resolved_user_id: None,
        };

        let mut contribs = HashMap::new();
        contribs.insert(
            alice.key(),
            Contribution {
                success_signals: 8,
                override_signals: 2,
                last_seen: Utc::now(),
            },
        );
        contribs.insert(
            bob.key(),
            Contribution {
                success_signals: 1,
                override_signals: 4,
                last_seen: Utc::now(),
            },
        );

        let e = Evidence {
            source_sessions: vec![],
            first_seen: None,
            last_validated: None,
            injection_count: 15,
            success_signals: 9,
            failure_signals: 0,
            override_signals: 6,
            contributions: contribs,
        };

        assert!((e.effectiveness_by_actor(&alice) - 0.8).abs() < 0.001);
        assert!((e.effectiveness_by_actor(&bob) - 0.2).abs() < 0.001);
        // Global effectiveness is unchanged by per-actor accessor:
        // effectiveness() = success/(success+override) = 9/15 = 0.6
        assert!((e.effectiveness() - 0.6).abs() < 0.001);
    }

    #[test]
    fn evidence_effectiveness_by_unknown_actor_returns_neutral_prior() {
        use crate::{Actor, ActorSource};

        let unknown = Actor {
            source: ActorSource::MurCli,
            native_id: "never-seen".into(),
            display_name: None,
            resolved_user_id: None,
        };
        let e = Evidence::default();
        // No contribution exists for this actor → neutral 0.5 prior
        assert!((e.effectiveness_by_actor(&unknown) - 0.5).abs() < 0.001);
    }

    #[test]
    fn evidence_yaml_roundtrip_with_contributions() {
        use crate::{Actor, ActorSource};

        let actor = Actor {
            source: ActorSource::CommanderDaemon,
            native_id: "svc-1".into(),
            display_name: None,
            resolved_user_id: None,
        };
        let mut contribs = HashMap::new();
        contribs.insert(
            actor.key(),
            Contribution {
                success_signals: 3,
                override_signals: 1,
                last_seen: DateTime::parse_from_rfc3339("2026-04-18T10:00:00Z")
                    .unwrap()
                    .with_timezone(&Utc),
            },
        );
        let e = Evidence {
            contributions: contribs,
            ..Evidence::default()
        };
        let y = serde_yaml::to_string(&e).unwrap();
        let back: Evidence = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back.contributions.len(), 1);
        assert_eq!(
            back.contributions
                .get("CommanderDaemon:svc-1")
                .unwrap()
                .success_signals,
            3
        );
    }

    #[test]
    fn evidence_yaml_backward_compat_no_contributions_field() {
        // Old YAML lacks the new field
        let old_yaml = r#"
source_sessions: []
first_seen: null
last_validated: null
injection_count: 5
success_signals: 3
override_signals: 1
    "#;
        let e: Evidence = serde_yaml::from_str(old_yaml).unwrap();
        assert!(e.contributions.is_empty());
        assert_eq!(e.success_signals, 3);
    }

    #[test]
    fn knowledge_scope_defaults_personal() {
        // Pre-sync YAML has no `scope:` field
        let old_yaml = r#"
schema: 2
name: legacy-pattern
description: legacy
content: old content
tier: session
    "#;
        let p: KnowledgeBase = serde_yaml::from_str(old_yaml).unwrap();
        assert_eq!(p.scope, crate::Scope::Personal);
    }

    #[test]
    fn knowledge_scope_team_roundtrip() {
        let y = r#"
schema: 2
name: team-pat
description: team pattern
content: team content
tier: project
scope:
  kind: team
  team_id: ops
    "#;
        let p: KnowledgeBase = serde_yaml::from_str(y).unwrap();
        assert_eq!(
            p.scope,
            crate::Scope::Team {
                team_id: "ops".into()
            }
        );
        // Roundtrip verify
        let y2 = serde_yaml::to_string(&p).unwrap();
        let p2: KnowledgeBase = serde_yaml::from_str(&y2).unwrap();
        assert_eq!(p2.scope, p.scope);
    }

    #[test]
    fn knowledge_scope_community_roundtrip() {
        let y = r#"
schema: 2
name: comm-pat
description: community pattern
content: community content
tier: core
scope:
  kind: community
  pack_id: rust-best-practices
    "#;
        let p: KnowledgeBase = serde_yaml::from_str(y).unwrap();
        assert_eq!(
            p.scope,
            crate::Scope::Community {
                pack_id: Some("rust-best-practices".into())
            }
        );
    }
}
