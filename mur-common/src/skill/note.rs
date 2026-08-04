//! Shared construction for `Category::Note` skills — ONE manifest literal
//! instead of three drifting copies (`mur notes create`, the runtime's
//! `remember` tool, and the TUI `/remember` command all build the same note).

use chrono::Utc;

use crate::skill::lifecycle::NoteKind;
use crate::skill::manifest::{Content, SkillManifest, Visibility};
use crate::skill::types::{Category, Priority};

/// Inputs that vary between note authors; everything else is fixed note shape.
pub struct NoteSpec<'a> {
    pub name: &'a str,
    /// One-line summary (also becomes `content.abstract`).
    pub description: &'a str,
    /// Markdown body (`content.note`).
    pub body: &'a str,
    pub kind: NoteKind,
    /// Author identity, e.g. `human:local` or `agent:<name>`.
    pub publisher: &'a str,
}

/// Build the canonical note manifest. Callers still run
/// `crate::skill::validate` and choose WHERE to write (global vs agent-local)
/// — scope is the caller's decision, shape is not.
pub fn note_manifest(spec: &NoteSpec<'_>) -> SkillManifest {
    SkillManifest {
        name: spec.name.to_string(),
        version: "1.0.0".into(),
        publisher: spec.publisher.to_string(),
        description: spec.description.to_string(),
        category: Category::Note,
        hosts: vec![],
        scope: Default::default(),
        visibility: Visibility::default(),
        origin: None,
        origin_version: None,
        origin_hash: None,
        fleet: None,
        team: None,
        governance: None,
        project: None,
        content: Content {
            r#abstract: spec.description.to_string(),
            context: None,
            procedure: None,
            command: None,
            note: Some(spec.body.to_string()),
        },
        requires: vec![],
        // Kind lives in the tags: a `rule` tag marks a rule; a plain note is a
        // fact. `lifecycle::note_kind()` is the single reader.
        tags: match spec.kind {
            NoteKind::Rule => vec!["rule".into()],
            NoteKind::Fact => vec![],
        },
        triggers: vec![],
        priority: Priority::Normal,
        evolution_log: vec![],
        transfer_chain: vec![],
        mcp_requirements: vec![],
        provenance: Default::default(),
        updated_at: Utc::now(),
        requires_programs: vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_manifest_validates_and_roundtrips_kind() {
        for (kind, expect) in [
            (NoteKind::Rule, Some(NoteKind::Rule)),
            (NoteKind::Fact, Some(NoteKind::Fact)),
        ] {
            let m = note_manifest(&NoteSpec {
                name: "t-note",
                description: "d",
                body: "b",
                kind,
                publisher: "human:t",
            });
            crate::skill::validate(&m).expect("canonical note must validate");
            assert_eq!(crate::skill::lifecycle::note_kind(&m), expect);
        }
    }
}

// ── Memory proposals (federation P2c) ────────────────────────────────────
// The central-curation leg: an agent that remembers something ALSO proposes
// it for review. The proposal is a file drop under the inbox (the runtime's
// one granted central-store write surface); `mur session out` reviews it and
// only an accepted proposal becomes a GLOBAL note — "visibility follows
// scope, propagation follows maturity" means nothing an agent inferred
// reaches other agents without either usage-earned maturity or this human
// gate.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Proposal drop directory, relative to the MUR home.
pub const MEMORY_PROPOSAL_DIR: &str = "inbox/memory-proposals";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemoryProposal {
    /// Canonical name of the agent that captured the memory.
    pub agent: String,
    pub proposed_at: chrono::DateTime<Utc>,
    /// The full note manifest — same shape the agent wrote locally.
    pub manifest: SkillManifest,
    /// Multibase (Base58Btc) Ed25519 signature over [`proposal_sign_input`]
    /// (P2c-2; v3d precedent — sign-input excludes `sig`). `None` = legacy
    /// unsigned proposal, tolerated on review unless `MUR_SIGNAL_REQUIRE_SIG`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sig: Option<String>,
    /// Key-rotation version; 0 = initial identity key.
    #[serde(default, skip_serializing_if = "crate::signal::is_zero")]
    pub key_version: u32,
}

/// Canonicalization version — bump if the sign-input shape changes.
pub const PROPOSAL_SIG_INPUT_VERSION: u32 = 1;

/// Canonical signed bytes: `sig` excluded; `serde_json` sorts object keys so
/// the encoding is deterministic. The `domain` tag prevents cross-context
/// signature reuse.
fn proposal_sign_input(p: &MemoryProposal) -> Vec<u8> {
    let canon = serde_json::json!({
        "domain": "mur-memory-proposal",
        "v": PROPOSAL_SIG_INPUT_VERSION,
        "agent": p.agent,
        "proposed_at": p.proposed_at,
        "manifest": p.manifest,
        "key_version": p.key_version,
    });
    serde_json::to_vec(&canon).unwrap_or_default()
}

impl MemoryProposal {
    /// Sign this proposal in place with the proposing agent's identity key.
    pub fn sign(&mut self, identity: &crate::identity::AgentIdentity) {
        self.sig = Some(identity.sign_multibase(&proposal_sign_input(self)));
    }

    /// Fail-closed signature check against `pubkey`; unsigned never verifies.
    pub fn verify(&self, pubkey: &[u8; 32]) -> bool {
        match &self.sig {
            Some(sig) => crate::identity::verify_bytes(pubkey, &proposal_sign_input(self), sig),
            None => false,
        }
    }
}

/// Atomically drop `proposal` into the inbox. Returns the written path.
/// Deterministic name (`<agent>-<note>`): re-remembering the same note
/// replaces the pending proposal instead of stacking duplicates.
pub fn write_memory_proposal(
    mur_home: &Path,
    proposal: &MemoryProposal,
) -> std::io::Result<PathBuf> {
    let dir = mur_home.join(MEMORY_PROPOSAL_DIR);
    std::fs::create_dir_all(&dir)?;
    let fname = format!("{}-{}.yaml", proposal.agent, proposal.manifest.name);
    let dest = dir.join(&fname);
    let tmp = dir.join(format!(".{fname}.tmp"));
    let yaml = serde_yaml_ng::to_string(proposal)
        .map_err(|e| std::io::Error::other(format!("serialize proposal: {e}")))?;
    std::fs::write(&tmp, yaml)?;
    std::fs::rename(&tmp, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod proposal_sig_tests {
    use super::*;
    use crate::identity::AgentIdentity;
    use crate::skill::lifecycle::NoteKind;

    fn proposal(agent: &str) -> MemoryProposal {
        MemoryProposal {
            agent: agent.into(),
            proposed_at: Utc::now(),
            manifest: note_manifest(&NoteSpec {
                name: "reply-zh",
                description: "reply language",
                body: "always zh-TW",
                kind: NoteKind::Rule,
                publisher: &format!("agent:{agent}"),
            }),
            sig: None,
            key_version: 0,
        }
    }

    #[test]
    fn sign_verify_roundtrip_survives_yaml() {
        let id = AgentIdentity::generate();
        let mut p = proposal("w1");
        assert!(
            !p.verify(&id.verifying_key_bytes()),
            "unsigned never verifies"
        );
        p.sign(&id);
        assert!(p.verify(&id.verifying_key_bytes()));

        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        let back: MemoryProposal = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(back.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn tampered_manifest_or_agent_fails_verification() {
        let id = AgentIdentity::generate();
        let mut p = proposal("w1");
        p.sign(&id);

        let mut swapped = p.clone();
        swapped.agent = "w2".into(); // impersonation
        assert!(!swapped.verify(&id.verifying_key_bytes()));

        let mut edited = p.clone();
        edited.manifest.content.note = Some("always en-US".into()); // content swap
        assert!(!edited.verify(&id.verifying_key_bytes()));
    }

    #[test]
    fn legacy_unsigned_yaml_deserializes_with_defaults() {
        let p = proposal("w1");
        // Serialize WITHOUT sig (legacy P2c shape) — new fields absent on wire.
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        assert!(!yaml.contains("sig:"));
        let back: MemoryProposal = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(back.sig.is_none());
        assert_eq!(back.key_version, 0);
    }
}
