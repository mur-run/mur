//! Signal wire format for cross-process memory sync events.
//!
//! Flows:
//! - commander writes → `~/.mur/commander/outbox/*.yaml` → POST /v1/signals/batch → mur-server
//! - mur CLI `mur fetch` ← GET /v1/signals/pending ← mur-server → `~/.mur/inbox/*.yaml`
//!
//! Schema version is bumped on breaking wire changes. Additive changes (new fields)
//! are serde-default and backward compatible within the same major version.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{Actor, Pattern, Scope};

// ─── FROZEN SCHEMA — v1 ──────────────────────────────────────────────────
// This module is the canonical wire format between commander and mur.
// SCHEMA FREEZE DATE: 2026-05-18
// Spec: docs/superpowers/specs/2026-05-18-commander-feedback-wire-protocol-design.md
//
// Changes to Signal, SignalKind, SignalTarget, Actor, ActorSource, or
// SIGNAL_SCHEMA_VERSION require:
//   1. Bumping SIGNAL_SCHEMA_VERSION to 2
//   2. Coordinated update in the commander repo (closed-source)
//   3. Adding a v2 HTTP endpoint at /v2/signals/...
//   4. Migration plan in a new design spec
//
// Additive changes (new fields with #[serde(default)]) are allowed within v1.
// ─────────────────────────────────────────────────────────────────────────

/// Current schema version of the Signal wire format. FROZEN at v1 — see
/// module-level comment for change rules.
pub const SIGNAL_SCHEMA_VERSION: u32 = 1;

/// A single event envelope: who produced what kind of event about which target,
/// with provenance. Carried verbatim through outbox → server → inbox.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Signal {
    pub id: Uuid,
    pub emitted_at: DateTime<Utc>,
    pub actor: Actor,
    pub target: SignalTarget,
    pub kind: SignalKind,
    pub scope: Scope,
    /// Confidence weight in [0.0, 1.0] applied server-side during aggregation.
    /// Default 1.0 (full weight).
    #[serde(default = "default_confidence")]
    pub confidence: f64,
    /// Wire-format version of this signal. Server-side rejects signals with
    /// unsupported major versions; additive fields with `#[serde(default)]`
    /// keep signals within the same major forward-compatible.
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
}

fn default_confidence() -> f64 {
    1.0
}
fn current_schema_version() -> u32 {
    SIGNAL_SCHEMA_VERSION
}

/// HTTP batch wrapper for `POST /v1/signals/batch`.
///
/// Carries 1–N signals in a single request. `batch_id` enables at-most-once
/// retry semantics: the server deduplicates on `batch_id` (HTTP layer) and on
/// individual `Signal.id` (inbox layer).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalBatch {
    pub batch_id: Uuid,
    /// Must equal `SIGNAL_SCHEMA_VERSION` (1). Server rejects mismatches.
    #[serde(default = "current_schema_version")]
    pub schema_version: u32,
    pub signals: Vec<Signal>,
}

/// Response body for `POST /v1/signals/batch`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SignalBatchResponse {
    pub accepted: usize,
    pub deduplicated: usize,
}

/// What the signal refers to.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SignalTarget {
    /// Refers to an existing pattern by name within a scope.
    Pattern { name: String, scope: Scope },
    /// Carries a fully-formed Pattern as a draft proposal (Channel 2/3).
    /// Boxed to keep the enum variant sizes comparable.
    NewDraftPattern { payload: Box<Pattern> },
}

/// What happened to the target.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SignalKind {
    /// Workflow/step using this pattern completed successfully. (Channel 1)
    ExecutionSuccess,
    /// Workflow/step using this pattern failed. (Channel 1)
    ExecutionFailure { error: String },
    /// User rejected a breakpoint while this pattern was active. (Channel 1, 3x weight)
    UserOverrideAtBreakpoint { reason: Option<String> },
    /// AutoFix ran on a step that used this pattern. (Channel 1, signals pattern inadequacy)
    AutoFixApplied { step: String },
    /// Proposal to add a new pattern. (Channel 2 — chat extraction, Channel 3 — procedural)
    NewPatternProposal { origin_context: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ActorSource;

    fn sample_actor() -> Actor {
        Actor {
            source: ActorSource::CommanderDaemon,
            native_id: "svc-1".into(),
            display_name: None,
            resolved_user_id: None,
        }
    }

    fn sample_signal() -> Signal {
        Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: sample_actor(),
            target: SignalTarget::Pattern {
                name: "rust-err-handling".into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::ExecutionSuccess,
            scope: Scope::Personal,
            confidence: 0.9,
            schema_version: SIGNAL_SCHEMA_VERSION,
        }
    }

    #[test]
    fn signal_roundtrip_execution_success() {
        let s = sample_signal();
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Signal = serde_yaml::from_str(&y).unwrap();
        assert_eq!(back.id, s.id);
        assert!(matches!(back.kind, SignalKind::ExecutionSuccess));
        assert!((back.confidence - 0.9).abs() < 1e-9);
    }

    #[test]
    fn signal_confidence_defaults_to_one() {
        let y = r#"
id: 00000000-0000-0000-0000-000000000001
emitted_at: 2026-04-18T10:00:00Z
actor: { source: commander_daemon, native_id: x }
target: { kind: pattern, name: foo, scope: { kind: personal } }
kind: { type: execution_success }
scope: { kind: personal }
"#;
        let s: Signal = serde_yaml::from_str(y).unwrap();
        assert!((s.confidence - 1.0).abs() < 1e-9);
        assert_eq!(s.schema_version, 1);
    }

    #[test]
    fn signal_kind_execution_failure_carries_error() {
        let s = Signal {
            kind: SignalKind::ExecutionFailure {
                error: "db timeout".into(),
            },
            ..sample_signal()
        };
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Signal = serde_yaml::from_str(&y).unwrap();
        match back.kind {
            SignalKind::ExecutionFailure { error } => assert_eq!(error, "db timeout"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn signal_kind_override_with_reason() {
        let y = r#"
id: 00000000-0000-0000-0000-000000000002
emitted_at: 2026-04-18T10:00:00Z
actor: { source: slack, native_id: U999 }
target: { kind: pattern, name: x, scope: { kind: personal } }
kind: { type: user_override_at_breakpoint, reason: "wrong step" }
scope: { kind: personal }
"#;
        let s: Signal = serde_yaml::from_str(y).unwrap();
        match s.kind {
            SignalKind::UserOverrideAtBreakpoint { reason } => {
                assert_eq!(reason.as_deref(), Some("wrong step"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn signal_kind_override_without_reason() {
        let y = r#"
id: 00000000-0000-0000-0000-000000000003
emitted_at: 2026-04-18T10:00:00Z
actor: { source: slack, native_id: U999 }
target: { kind: pattern, name: x, scope: { kind: personal } }
kind: { type: user_override_at_breakpoint }
scope: { kind: personal }
"#;
        let s: Signal = serde_yaml::from_str(y).unwrap();
        assert!(matches!(
            s.kind,
            SignalKind::UserOverrideAtBreakpoint { reason: None }
        ));
    }

    #[test]
    fn signal_kind_autofix() {
        let s = Signal {
            kind: SignalKind::AutoFixApplied {
                step: "run-tests".into(),
            },
            ..sample_signal()
        };
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Signal = serde_yaml::from_str(&y).unwrap();
        match back.kind {
            SignalKind::AutoFixApplied { step } => assert_eq!(step, "run-tests"),
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn signal_kind_new_pattern_proposal() {
        let s = Signal {
            kind: SignalKind::NewPatternProposal {
                origin_context: "slack DM from alice: use pnpm".into(),
            },
            ..sample_signal()
        };
        let y = serde_yaml::to_string(&s).unwrap();
        let back: Signal = serde_yaml::from_str(&y).unwrap();
        match back.kind {
            SignalKind::NewPatternProposal { origin_context } => {
                assert!(origin_context.contains("alice"));
            }
            _ => panic!("wrong variant"),
        }
    }

    #[test]
    fn signal_target_pattern_roundtrip() {
        let p = SignalTarget::Pattern {
            name: "foo".into(),
            scope: Scope::Team {
                team_id: "ops".into(),
            },
        };
        let y = serde_yaml::to_string(&p).unwrap();
        assert!(y.contains("kind: pattern"));
        let back: SignalTarget = serde_yaml::from_str(&y).unwrap();
        assert!(matches!(back, SignalTarget::Pattern { .. }));
    }

    #[test]
    fn signal_with_new_draft_pattern_roundtrip() {
        use crate::knowledge::KnowledgeBase;
        use crate::pattern::{Content, Tier};

        // Build a minimal Pattern to box into the target payload.
        let kb = KnowledgeBase {
            name: "draft-pat".into(),
            description: "chat-extracted draft".into(),
            content: Content::Plain("use pnpm not npm".into()),
            tier: Tier::Session,
            ..Default::default()
        };
        let pat = Pattern {
            base: kb,
            kind: None,
            origin: None,
            attachments: Vec::new(),
        };

        let sig = Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: sample_actor(),
            target: SignalTarget::NewDraftPattern {
                payload: Box::new(pat.clone()),
            },
            kind: SignalKind::NewPatternProposal {
                origin_context: "slack DM".into(),
            },
            scope: Scope::Personal,
            confidence: 0.75,
            schema_version: SIGNAL_SCHEMA_VERSION,
        };
        let y = serde_yaml::to_string(&sig).unwrap();
        assert!(y.contains("kind: new_draft_pattern"));
        let back: Signal = serde_yaml::from_str(&y).unwrap();
        match back.target {
            SignalTarget::NewDraftPattern { payload } => {
                assert_eq!(payload.name, "draft-pat");
            }
            _ => panic!("expected NewDraftPattern variant"),
        }
    }

    #[test]
    fn schema_version_constant() {
        assert_eq!(SIGNAL_SCHEMA_VERSION, 1);
    }
}
