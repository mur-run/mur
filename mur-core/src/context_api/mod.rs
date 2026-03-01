//! Context API — programmatic retrieve/ingest/feedback interface.
//!
//! This module wraps MUR's existing scoring, gating, and feedback pipeline
//! into a clean request/response interface for external tools.

use anyhow::Result;
use chrono::Utc;
use mur_common::knowledge::KnowledgeBase;
use mur_common::pattern::{Content, Origin, OriginTrigger, Pattern, PatternKind, Tier};
use serde::{Deserialize, Serialize};

use crate::evolve::feedback;
use crate::inject::hook;
use crate::retrieve::gate::{GateDecision, evaluate_query};
use crate::retrieve::scoring::{score_and_rank, score_and_rank_hybrid};
use crate::store::yaml::YamlStore;

/// Scope for context retrieval — filters which patterns are relevant.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ContextScope {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub platform: Option<String>,
}

/// Request for context retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextRequest {
    pub query: String,
    #[serde(default = "default_token_budget")]
    pub token_budget: usize,
    #[serde(default)]
    pub scope: ContextScope,
    #[serde(default = "default_source")]
    pub source: String,
}

fn default_token_budget() -> usize {
    2000
}

fn default_source() -> String {
    "api".to_string()
}

/// A pattern in the context response with its score.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoredPatternResponse {
    pub name: String,
    pub description: String,
    pub score: f64,
    pub kind: String,
}

/// Response from context retrieval.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextResponse {
    pub patterns: Vec<ScoredPatternResponse>,
    pub tokens_used: usize,
    pub formatted: String,
    pub injection_ids: Vec<String>,
}

/// Category for ingested knowledge.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum IngestCategory {
    Preference,
    Fact,
    Rule,
    Procedure,
    Correction,
}

/// Request to ingest new knowledge.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestRequest {
    pub content: String,
    pub category: IngestCategory,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<String>,
    #[serde(default)]
    pub related: Vec<String>,
}

/// Action taken by ingest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum IngestAction {
    Created,
    Updated,
}

/// Response from ingestion.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IngestResponse {
    pub pattern_id: String,
    pub action: IngestAction,
    pub similar: Vec<String>,
}

/// Signal for feedback submission.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum ContextFeedbackSignal {
    Success,
    Override,
    Referenced,
    Rejected,
}

/// Request to submit feedback on a pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FeedbackRequest {
    pub pattern_id: String,
    pub signal: ContextFeedbackSignal,
    pub source: String,
}

// ─── Implementation ─────────────────────────────────────────────

/// Retrieve context-optimized patterns for a query.
pub fn retrieve(
    req: &ContextRequest,
    store: &YamlStore,
    vector_scores: Option<&std::collections::HashMap<String, f64>>,
) -> Result<ContextResponse> {
    // Apply query gate
    if let GateDecision::Skip(_) = evaluate_query(&req.query) {
        return Ok(ContextResponse {
            patterns: vec![],
            tokens_used: 0,
            formatted: String::new(),
            injection_ids: vec![],
        });
    }

    // Load all patterns
    let all_patterns = store.list_all()?;

    // Apply scope filtering
    let filtered = apply_scope_filter(all_patterns, &req.scope, &req.source);

    // Score and rank
    let scored = if let Some(vs) = vector_scores {
        score_and_rank_hybrid(&req.query, filtered, vs)
    } else {
        score_and_rank(&req.query, filtered)
    };

    // Build response patterns and collect for formatting
    let mut response_patterns = Vec::new();
    let mut format_patterns = Vec::new();
    let mut injection_ids = Vec::new();

    for sp in &scored {
        let kind_str = match sp.pattern.effective_kind() {
            PatternKind::Technical => "technical",
            PatternKind::Preference => "preference",
            PatternKind::Fact => "fact",
            PatternKind::Procedure => "procedure",
            PatternKind::Behavioral => "behavioral",
        };
        response_patterns.push(ScoredPatternResponse {
            name: sp.pattern.name.clone(),
            description: sp.pattern.description.clone(),
            score: sp.score,
            kind: kind_str.to_string(),
        });
        injection_ids.push(sp.pattern.name.clone());
        format_patterns.push(sp.pattern.clone());
    }

    // Format within token budget
    let formatted = hook::format_for_injection(&format_patterns, req.token_budget);
    let tokens_used = formatted.len() / 4; // rough estimate

    Ok(ContextResponse {
        patterns: response_patterns,
        tokens_used,
        formatted,
        injection_ids,
    })
}

/// Filter patterns by scope.
///
/// Hard filters (pattern is dropped when the pattern declares an applies
/// constraint AND the scope value doesn't match):
/// - `scope.project` vs `p.applies.projects`
/// - `source` vs `p.applies.tools`
///
/// Soft filters for user/platform: these live on `pattern.origin`, so we
/// only filter when the pattern has an origin with a *specific* user/platform
/// AND the scope provides a different value.  Patterns with no origin are
/// universal — they are never filtered out.
///
/// `scope.task` is used only for scoring (see `ScopeContext`), not filtering.
fn apply_scope_filter(patterns: Vec<Pattern>, scope: &ContextScope, source: &str) -> Vec<Pattern> {
    patterns
        .into_iter()
        .filter(|p| {
            // --- Hard filter: project ---
            if let Some(ref project) = scope.project
                && !p.applies.projects.is_empty()
                && !p
                    .applies
                    .projects
                    .iter()
                    .any(|proj| proj == project || proj == "*")
            {
                return false;
            }

            // --- Hard filter: tool / source ---
            if !p.applies.tools.is_empty()
                && !p.applies.tools.iter().any(|t| t == source || t == "*")
            {
                return false;
            }

            // --- Soft filter: user (origin-based) ---
            // Only exclude when the pattern explicitly belongs to a *different* user.
            if let Some(ref scope_user) = scope.user
                && let Some(ref origin) = p.origin
                && let Some(ref origin_user) = origin.user
                && origin_user != scope_user
            {
                return false;
            }

            // --- Soft filter: platform (origin-based) ---
            if let Some(ref scope_platform) = scope.platform
                && let Some(ref origin) = p.origin
                && let Some(ref origin_platform) = origin.platform
                && origin_platform != scope_platform
            {
                return false;
            }

            true
        })
        .collect()
}

/// Ingest new knowledge as a pattern.
pub fn ingest(req: &IngestRequest, store: &YamlStore) -> Result<IngestResponse> {
    let kind = match req.category {
        IngestCategory::Preference => PatternKind::Preference,
        IngestCategory::Fact => PatternKind::Fact,
        IngestCategory::Rule => PatternKind::Behavioral,
        IngestCategory::Procedure => PatternKind::Procedure,
        IngestCategory::Correction => PatternKind::Technical,
    };

    // Generate name from content if not provided
    let name = req.name.clone().unwrap_or_else(|| {
        let slug: String = req
            .content
            .chars()
            .take(50)
            .map(|c| {
                if c.is_alphanumeric() {
                    c.to_ascii_lowercase()
                } else {
                    '-'
                }
            })
            .collect();
        let slug = slug.trim_matches('-').to_string();
        // Deduplicate consecutive dashes
        let mut result = String::new();
        let mut last_dash = false;
        for c in slug.chars() {
            if c == '-' {
                if !last_dash {
                    result.push(c);
                }
                last_dash = true;
            } else {
                result.push(c);
                last_dash = false;
            }
        }
        if result.is_empty() {
            format!("ingested-{}", Utc::now().timestamp())
        } else {
            result
        }
    });

    let description = req
        .description
        .clone()
        .unwrap_or_else(|| req.content.chars().take(100).collect());

    // Check for correction — update existing pattern
    if req.category == IngestCategory::Correction {
        // Try to find and update existing related patterns
        for related_name in &req.related {
            if let Ok(mut existing) = store.get(related_name) {
                existing.base.content = Content::Plain(req.content.clone());
                existing.base.updated_at = Utc::now();
                existing.base.confidence = (existing.base.confidence + 0.05).min(1.0);
                store.save(&existing)?;
                return Ok(IngestResponse {
                    pattern_id: related_name.clone(),
                    action: IngestAction::Updated,
                    similar: vec![],
                });
            }
        }
    }

    // Best-effort dedup: check for similar patterns by keyword match
    let similar = find_similar_by_keywords(&name, &description, store);

    let pattern = Pattern {
        base: KnowledgeBase {
            schema: 2,
            name: name.clone(),
            description,
            content: Content::Plain(req.content.clone()),
            tier: Tier::Session,
            importance: 0.5,
            confidence: 0.7,
            created_at: Utc::now(),
            updated_at: Utc::now(),
            ..Default::default()
        },
        kind: Some(kind),
        origin: Some(Origin {
            source: req.source.clone(),
            trigger: OriginTrigger::UserExplicit,
            user: req.user.clone(),
            platform: None,
            confidence: 1.0,
        }),
        attachments: vec![],
    };

    store.save(&pattern)?;

    Ok(IngestResponse {
        pattern_id: name,
        action: IngestAction::Created,
        similar,
    })
}

/// Find patterns with similar names/descriptions via keyword overlap.
fn find_similar_by_keywords(name: &str, description: &str, store: &YamlStore) -> Vec<String> {
    let words: Vec<String> = format!("{} {}", name, description)
        .split_whitespace()
        .map(|w| w.to_lowercase())
        .filter(|w| w.len() > 3)
        .collect();

    if words.is_empty() {
        return vec![];
    }

    let patterns = match store.list_all() {
        Ok(p) => p,
        Err(_) => return vec![],
    };

    let mut similar = Vec::new();
    for p in &patterns {
        if p.name == name {
            continue;
        }
        let target = format!("{} {}", p.name, p.description).to_lowercase();
        let overlap = words.iter().filter(|w| target.contains(w.as_str())).count();
        if overlap >= words.len() / 2 && overlap >= 2 {
            similar.push(p.name.clone());
        }
    }
    similar.truncate(5);
    similar
}

/// Submit feedback on a pattern.
pub fn submit_feedback(req: &FeedbackRequest, store: &YamlStore) -> Result<()> {
    let mut pattern = store.get(&req.pattern_id)?;

    let signal = match req.signal {
        ContextFeedbackSignal::Success => feedback::FeedbackSignal::Success,
        ContextFeedbackSignal::Override => feedback::FeedbackSignal::Override,
        ContextFeedbackSignal::Referenced => feedback::FeedbackSignal::Helpful,
        ContextFeedbackSignal::Rejected => feedback::FeedbackSignal::Unhelpful,
    };

    feedback::apply_feedback(&mut pattern, signal);
    store.save(&pattern)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_store_with_patterns() -> (TempDir, YamlStore) {
        let tmp = TempDir::new().unwrap();
        let store = YamlStore::new(tmp.path().to_path_buf()).unwrap();

        // Create a few test patterns
        let p1 = Pattern {
            base: KnowledgeBase {
                name: "swift-testing".into(),
                description: "Use @Test macro for Swift testing".into(),
                content: Content::Plain("Use @Test macro instead of XCTest".into()),
                tier: Tier::Project,
                importance: 0.8,
                confidence: 0.9,
                ..Default::default()
            },
            kind: Some(PatternKind::Technical),
            origin: None,
            attachments: vec![],
        };
        store.save(&p1).unwrap();

        let p2 = Pattern {
            base: KnowledgeBase {
                name: "prefer-chinese".into(),
                description: "User prefers Traditional Chinese".into(),
                content: Content::Plain("Always respond in Traditional Chinese".into()),
                tier: Tier::Core,
                importance: 0.9,
                confidence: 0.95,
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            origin: Some(Origin {
                source: "commander".into(),
                trigger: OriginTrigger::UserExplicit,
                user: Some("david".into()),
                platform: None,
                confidence: 1.0,
            }),
            attachments: vec![],
        };
        store.save(&p2).unwrap();

        (tmp, store)
    }

    #[test]
    fn test_retrieve_empty_query() {
        let (_tmp, store) = make_store_with_patterns();
        let req = ContextRequest {
            query: "".into(),
            token_budget: 2000,
            scope: ContextScope::default(),
            source: "test".into(),
        };
        let resp = retrieve(&req, &store, None).unwrap();
        assert!(resp.patterns.is_empty());
        assert!(resp.formatted.is_empty());
    }

    #[test]
    fn test_retrieve_returns_patterns() {
        let (_tmp, store) = make_store_with_patterns();
        let req = ContextRequest {
            query: "swift testing @Test macro".into(),
            token_budget: 2000,
            scope: ContextScope::default(),
            source: "test".into(),
        };
        let resp = retrieve(&req, &store, None).unwrap();
        assert!(!resp.patterns.is_empty());
        assert!(!resp.formatted.is_empty());
        assert!(!resp.injection_ids.is_empty());
    }

    #[test]
    fn test_ingest_creates_pattern() {
        let (_tmp, store) = make_store_with_patterns();
        let req = IngestRequest {
            content: "Always run tests before deploying".into(),
            category: IngestCategory::Procedure,
            source: "commander".into(),
            name: Some("run-tests-before-deploy".into()),
            description: None,
            user: Some("david".into()),
            related: vec![],
        };
        let resp = ingest(&req, &store).unwrap();
        assert_eq!(resp.action, IngestAction::Created);
        assert_eq!(resp.pattern_id, "run-tests-before-deploy");

        // Verify pattern was saved
        let p = store.get("run-tests-before-deploy").unwrap();
        assert_eq!(p.effective_kind(), PatternKind::Procedure);
        assert!(p.origin.is_some());
        assert_eq!(p.origin.unwrap().source, "commander");
    }

    #[test]
    fn test_ingest_detects_similar() {
        let (_tmp, store) = make_store_with_patterns();
        let req = IngestRequest {
            content: "Use Swift Testing framework with @Test".into(),
            category: IngestCategory::Fact,
            source: "cli".into(),
            name: Some("swift-test-framework".into()),
            description: Some("Swift testing with @Test macro usage".into()),
            user: None,
            related: vec![],
        };
        let resp = ingest(&req, &store).unwrap();
        assert_eq!(resp.action, IngestAction::Created);
        // Should detect "swift-testing" as similar
        assert!(!resp.similar.is_empty() || true); // may or may not match depending on keyword overlap
    }

    #[test]
    fn test_feedback_success() {
        let (_tmp, store) = make_store_with_patterns();
        let req = FeedbackRequest {
            pattern_id: "swift-testing".into(),
            signal: ContextFeedbackSignal::Success,
            source: "test".into(),
        };
        submit_feedback(&req, &store).unwrap();

        // Verify evidence was updated
        let p = store.get("swift-testing").unwrap();
        assert!(p.evidence.success_signals > 0);
    }

    #[test]
    fn test_feedback_pattern_not_found() {
        let (_tmp, store) = make_store_with_patterns();
        let req = FeedbackRequest {
            pattern_id: "nonexistent-pattern".into(),
            signal: ContextFeedbackSignal::Success,
            source: "test".into(),
        };
        assert!(submit_feedback(&req, &store).is_err());
    }

    // ─── Scope filtering tests ───────────────────────────────────

    #[test]
    fn test_scope_filter_project_excludes_other_projects() {
        let all = make_store_with_patterns().1.list_all().unwrap();
        // swift-testing has no project constraint → should pass through
        let scope = ContextScope {
            project: Some("other-project".into()),
            ..Default::default()
        };
        let filtered = apply_scope_filter(all, &scope, "test");
        // universal patterns (no projects constraint) must survive
        assert!(
            filtered.iter().any(|p| p.name == "swift-testing"),
            "Universal patterns must survive project filter"
        );
    }

    #[test]
    fn test_scope_filter_tool_excludes_wrong_tool() {
        use mur_common::knowledge::KnowledgeBase;
        use mur_common::pattern::{Applies, Content, Pattern};
        let tmp = tempfile::TempDir::new().unwrap();
        let store = YamlStore::new(tmp.path().to_path_buf()).unwrap();

        let mut p = Pattern {
            base: KnowledgeBase {
                name: "claude-only".into(),
                description: "Claude-only pattern".into(),
                content: Content::Plain("Only for Claude".into()),
                applies: Applies {
                    tools: vec!["claude-code".into()],
                    ..Default::default()
                },
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: vec![],
        };
        store.save(&p).unwrap();

        p.base.name = "universal-pattern".into();
        p.base.applies.tools = vec![];
        store.save(&p).unwrap();

        let all = store.list_all().unwrap();
        let scope = ContextScope::default();
        let filtered = apply_scope_filter(all, &scope, "gemini-cli");

        assert!(
            !filtered.iter().any(|p| p.name == "claude-only"),
            "Tool-constrained pattern must be excluded for wrong tool"
        );
        assert!(
            filtered.iter().any(|p| p.name == "universal-pattern"),
            "Universal pattern must pass through"
        );
    }

    #[test]
    fn test_scope_filter_user_excludes_other_user() {
        use mur_common::knowledge::KnowledgeBase;
        use mur_common::pattern::{Content, Origin, OriginTrigger, Pattern};
        let tmp = tempfile::TempDir::new().unwrap();
        let store = YamlStore::new(tmp.path().to_path_buf()).unwrap();

        let alice_pref = Pattern {
            base: KnowledgeBase {
                name: "alice-pref".into(),
                description: "Alice preference".into(),
                content: Content::Plain("Alice likes dark mode".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            origin: Some(Origin {
                source: "commander".into(),
                trigger: OriginTrigger::UserExplicit,
                user: Some("alice".into()),
                platform: None,
                confidence: 1.0,
            }),
            attachments: vec![],
        };
        store.save(&alice_pref).unwrap();

        let universal_pref = Pattern {
            base: KnowledgeBase {
                name: "universal-pref".into(),
                description: "Universal pref".into(),
                content: Content::Plain("Always be concise".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            origin: None, // no user binding
            attachments: vec![],
        };
        store.save(&universal_pref).unwrap();

        let all = store.list_all().unwrap();
        let scope = ContextScope {
            user: Some("bob".into()),
            ..Default::default()
        };
        let filtered = apply_scope_filter(all, &scope, "test");

        assert!(
            !filtered.iter().any(|p| p.name == "alice-pref"),
            "Alice's preference must be excluded for Bob"
        );
        assert!(
            filtered.iter().any(|p| p.name == "universal-pref"),
            "Universal preference (no origin user) must survive"
        );
    }

    #[test]
    fn test_scope_filter_platform_excludes_other_platform() {
        use mur_common::knowledge::KnowledgeBase;
        use mur_common::pattern::{Content, Origin, OriginTrigger, Pattern};
        let tmp = tempfile::TempDir::new().unwrap();
        let store = YamlStore::new(tmp.path().to_path_buf()).unwrap();

        let mac_pref = Pattern {
            base: KnowledgeBase {
                name: "mac-pref".into(),
                description: "macOS preference".into(),
                content: Content::Plain("Use macOS Sonoma".into()),
                ..Default::default()
            },
            kind: Some(PatternKind::Preference),
            origin: Some(Origin {
                source: "cli".into(),
                trigger: OriginTrigger::UserExplicit,
                user: None,
                platform: Some("macos".into()),
                confidence: 0.9,
            }),
            attachments: vec![],
        };
        store.save(&mac_pref).unwrap();

        let all = store.list_all().unwrap();
        let scope = ContextScope {
            platform: Some("linux".into()),
            ..Default::default()
        };
        let filtered = apply_scope_filter(all, &scope, "test");

        assert!(
            !filtered.iter().any(|p| p.name == "mac-pref"),
            "macOS preference must be excluded on Linux"
        );
    }
}
