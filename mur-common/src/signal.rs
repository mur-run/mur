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

use crate::skill::manifest::SkillManifest;
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
    /// Multibase (Base58Btc) Ed25519 signature over [`sign_input`] — federation
    /// P2c-2, following the v3d `ChannelEvent` precedent. `None` = legacy
    /// unsigned signal (tolerated on ingest unless `MUR_SIGNAL_REQUIRE_SIG`).
    /// Additive `#[serde(default)]` field — allowed within frozen schema v1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// Key-rotation version; 0 = initial identity key.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub key_version: u32,
}

fn default_confidence() -> f64 {
    1.0
}
fn current_schema_version() -> u32 {
    SIGNAL_SCHEMA_VERSION
}
pub(crate) fn is_zero(v: &u32) -> bool {
    *v == 0
}

/// Canonicalization version — bump if the sign-input shape changes so an old
/// signature is never silently checked against a new canonicalization.
pub const SIGNAL_SIG_INPUT_VERSION: u32 = 1;

/// Canonical signed bytes for a [`Signal`]: every semantic field, `sig`
/// excluded. `serde_json` sorts object keys (no preserve_order), so this is
/// deterministic for a given input. The `domain` tag prevents a signature
/// minted here from verifying in any other MUR signing context.
fn sign_input(s: &Signal) -> Vec<u8> {
    let canon = serde_json::json!({
        "domain": "mur-signal",
        "v": SIGNAL_SIG_INPUT_VERSION,
        "id": s.id,
        "emitted_at": s.emitted_at,
        "actor": s.actor,
        "target": s.target,
        "kind": s.kind,
        "scope": s.scope,
        "confidence": s.confidence,
        "schema_version": s.schema_version,
        "key_version": s.key_version,
    });
    serde_json::to_vec(&canon).unwrap_or_default()
}

/// Parse `MUR_SIGNAL_REQUIRE_SIG`: only explicit truthy values enable
/// signature enforcement (`=0` / `=false`, or unset, must NOT turn it on) —
/// default-off is migration safety AND the commander wire (frozen v1, signals
/// arrive bearer-token-authed but unsigned). One parser for every reader,
/// mirroring `MUR_CHANNEL_REQUIRE_SIG`.
pub fn require_sig_from_env() -> bool {
    std::env::var("MUR_SIGNAL_REQUIRE_SIG")
        .map(|v| matches!(v.as_str(), "1" | "true" | "TRUE" | "yes"))
        .unwrap_or(false)
}

impl Signal {
    /// Sign this signal in place with the emitting agent's identity key.
    /// The signature covers every field except `sig` itself.
    pub fn sign(&mut self, identity: &crate::identity::AgentIdentity) {
        self.sig = Some(identity.sign_multibase(&sign_input(self)));
    }

    /// Fail-closed signature check against `pubkey`. An unsigned signal
    /// never verifies — callers decide whether unsigned is tolerated.
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        match &self.sig {
            Some(sig) => crate::identity::verify_bytes(pubkey, &sign_input(self), sig),
            None => false,
        }
    }
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
    /// Refers to an installed skill by name.
    Skill { name: String, scope: Scope },
    /// Carries a fully-formed SkillManifest as a draft proposal.
    NewDraftSkill { payload: Box<SkillManifest> },
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
    /// Skill execution succeeded. (Channel 1)
    SkillExecutionSuccess,
    /// Skill execution failed. (Channel 1)
    SkillExecutionFailure { error: String },
    /// Proposal to add a new skill. (Channel 2 / Channel 3)
    NewDraftSkill {
        payload: Box<SkillManifest>,
        origin_context: String,
    },
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
            sig: None,
            key_version: 0,
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
            sig: None,
            key_version: 0,
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

    #[test]
    fn sign_verify_roundtrip_and_yaml_preserves_sig() {
        let id = crate::identity::AgentIdentity::generate();
        let mut s = sample_signal();
        s.sign(&id);
        assert!(s.verify(&id.verifying_key_bytes()));

        let yaml = serde_yaml::to_string(&s).unwrap();
        let back: Signal = serde_yaml::from_str(&yaml).unwrap();
        assert!(back.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn tampered_field_fails_verification() {
        let id = crate::identity::AgentIdentity::generate();
        let mut s = sample_signal();
        s.sign(&id);
        s.scope = Scope::Team {
            team_id: "ops".into(),
        }; // scope-escalation attempt after signing
        assert!(!s.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn wrong_key_and_unsigned_fail_verification() {
        let id = crate::identity::AgentIdentity::generate();
        let other = crate::identity::AgentIdentity::generate();
        let mut s = sample_signal();
        assert!(
            !s.verify(&id.verifying_key_bytes()),
            "unsigned never verifies"
        );
        s.sign(&id);
        assert!(!s.verify(&other.verifying_key_bytes()));
    }

    #[test]
    fn legacy_unsigned_yaml_deserializes_with_defaults() {
        // Pre-P2c-2 signal yaml — no sig/key_version fields.
        let y = r#"
id: 00000000-0000-0000-0000-000000000009
emitted_at: 2026-04-18T10:00:00Z
actor: { source: commander_daemon, native_id: x }
target: { kind: pattern, name: foo, scope: { kind: personal } }
kind: { type: execution_success }
scope: { kind: personal }
"#;
        let s: Signal = serde_yaml::from_str(y).unwrap();
        assert!(s.sig.is_none());
        assert_eq!(s.key_version, 0);
        // And an unsigned signal serializes WITHOUT the new keys (wire-stable).
        let out = serde_yaml::to_string(&s).unwrap();
        assert!(!out.contains("sig:"));
        assert!(!out.contains("key_version:"));
    }

    #[test]
    fn signal_target_skill_roundtrips() {
        let t = SignalTarget::Skill {
            name: "my-skill".into(),
            scope: Scope::Personal,
        };
        let s = serde_json::to_string(&t).unwrap();
        assert!(s.contains("\"kind\":\"skill\""), "got: {s}");
        let back: SignalTarget = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, SignalTarget::Skill { .. }));
    }

    #[test]
    fn signal_kind_new_draft_skill_roundtrips() {
        let k = SignalKind::NewDraftSkill {
            payload: Box::new(
                serde_json::from_str::<SkillManifest>(
                    r#"{"name":"x","version":"1","publisher":"human:t","description":"d","category":"context","content":{"abstract":"a"}}"#,
                )
                .unwrap(),
            ),
            origin_context: "test".into(),
        };
        let s = serde_json::to_string(&k).unwrap();
        let back: SignalKind = serde_json::from_str(&s).unwrap();
        assert!(matches!(back, SignalKind::NewDraftSkill { .. }));
    }
}
