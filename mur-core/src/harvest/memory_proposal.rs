//! Review-side of memory proposals (federation P2c): list what agents have
//! proposed, and turn a human decision into either a GLOBAL note (accept) or
//! a deleted file (dismiss). Pure functions — the interactive loop in
//! `cmd/session.rs` just renders them.
//!
//! Decisions delete the proposal file rather than keeping a status ledger:
//! the agent-local note remains the durable source either way, and a
//! re-remember re-proposes deterministically (same file name).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};

use mur_common::skill::note::{MEMORY_PROPOSAL_DIR, MemoryProposal};
use mur_common::skill::store::global_skill_dir;

/// Signature status of a pending proposal, computed at listing time (P2c-2).
#[derive(Debug, Clone, PartialEq)]
pub enum ProposalSigStatus {
    /// Signature present and verified against the agent's on-disk pubkey.
    Verified,
    /// Legacy pre-P2c-2 drop with no signature. Acceptable unless
    /// `MUR_SIGNAL_REQUIRE_SIG` enforces signing.
    Unsigned,
    /// Signature present but wrong, or the claimed agent has no verifiable
    /// identity — never acceptable (fail-closed).
    Invalid(String),
}

/// A pending proposal plus the file it came from.
pub struct PendingMemoryProposal {
    pub path: PathBuf,
    pub proposal: MemoryProposal,
    pub sig_status: ProposalSigStatus,
}

/// The signature proves *who proposed it*: verify against the CLAIMED agent's
/// pubkey at `agents/<agent>/identity.pub`. The agent name is joined into a
/// path, so it gets the same charset guard every other file-drop surface has.
fn sig_status(mur_home: &Path, proposal: &MemoryProposal) -> ProposalSigStatus {
    if proposal.sig.is_none() {
        return ProposalSigStatus::Unsigned;
    }
    let agent = &proposal.agent;
    let name_ok = !agent.is_empty()
        && agent
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !name_ok {
        return ProposalSigStatus::Invalid(format!("'{agent}' is not a valid agent name"));
    }
    let dir = mur_home.join("agents").join(agent);
    match mur_common::identity::AgentIdentity::load_pubkey(&dir) {
        Ok(pubkey) if proposal.verify(&pubkey) => ProposalSigStatus::Verified,
        Ok(_) => ProposalSigStatus::Invalid(format!("signature does not verify for '{agent}'")),
        Err(e) => ProposalSigStatus::Invalid(format!("no verifiable identity for '{agent}': {e}")),
    }
}

/// All pending proposals, oldest first. Unparseable files are skipped with a
/// warning — one corrupt drop must not block the review of the rest.
pub fn pending(mur_home: &Path) -> Result<Vec<PendingMemoryProposal>> {
    let dir = mur_home.join(MEMORY_PROPOSAL_DIR);
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Ok(out); // no dir yet = nothing proposed
    };
    for entry in entries {
        let path = entry?.path();
        if path.extension().is_none_or(|e| e != "yaml") {
            continue;
        }
        match std::fs::read_to_string(&path)
            .map_err(anyhow::Error::from)
            .and_then(|t| serde_yaml_ng::from_str::<MemoryProposal>(&t).map_err(Into::into))
        {
            Ok(proposal) => {
                let sig_status = sig_status(mur_home, &proposal);
                out.push(PendingMemoryProposal {
                    path,
                    proposal,
                    sig_status,
                })
            }
            Err(e) => {
                tracing::warn!(file = %path.display(), error = %e, "skipping unparseable memory proposal")
            }
        }
    }
    out.sort_by_key(|p| p.proposal.proposed_at);
    Ok(out)
}

/// Accept: write the note into the GLOBAL skills store (never overwriting an
/// existing skill) and consume the proposal file. Returns the note name.
pub fn accept(mur_home: &Path, pending: &PendingMemoryProposal) -> Result<String> {
    let manifest = &pending.proposal.manifest;
    // Signature gate first (P2c-2): a bad signature is never acceptable;
    // unsigned is legacy-tolerated unless enforcement is on.
    match &pending.sig_status {
        ProposalSigStatus::Invalid(reason) => {
            bail!("refusing '{}': {reason}", manifest.name)
        }
        ProposalSigStatus::Unsigned if mur_common::signal::require_sig_from_env() => {
            bail!(
                "refusing unsigned proposal '{}' (MUR_SIGNAL_REQUIRE_SIG)",
                manifest.name
            )
        }
        _ => {}
    }
    // Same gates the synced NewDraftSkill path applies: the name is joined
    // into the skills dir, so validate before any filesystem contact.
    if !mur_common::skill::is_valid_skill_name(&manifest.name) {
        bail!("invalid note name '{}'", manifest.name);
    }
    mur_common::skill::validate(manifest).context("proposal manifest invalid")?;
    let dir = global_skill_dir(mur_home, &manifest.name);
    if dir.join("skill.yaml").exists() {
        bail!(
            "a skill named '{}' already exists — dismissing instead of overwriting",
            manifest.name
        );
    }
    mur_common::skill::store::write_to_dir(&dir, manifest)
        .map_err(|e| anyhow::anyhow!("write global note: {e}"))?;
    let _ = std::fs::remove_file(&pending.path);
    Ok(manifest.name.clone())
}

/// Dismiss: consume the proposal file; the agent-local copy stays untouched.
pub fn dismiss(pending: &PendingMemoryProposal) -> Result<()> {
    std::fs::remove_file(&pending.path)
        .with_context(|| format!("remove {}", pending.path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::lifecycle::NoteKind;
    use mur_common::skill::note::{NoteSpec, note_manifest, write_memory_proposal};

    fn drop_proposal(home: &Path, agent: &str, name: &str) {
        let p = MemoryProposal {
            agent: agent.into(),
            proposed_at: chrono::Utc::now(),
            manifest: note_manifest(&NoteSpec {
                name,
                description: "reply language",
                body: "always zh-TW",
                kind: NoteKind::Rule,
                publisher: &format!("agent:{agent}"),
            }),
            sig: None,
            key_version: 0,
        };
        write_memory_proposal(home, &p).unwrap();
    }

    #[test]
    fn propose_accept_lands_a_global_note_and_consumes_the_file() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        drop_proposal(home, "w1", "reply-zh");

        let list = pending(home).unwrap();
        assert_eq!(list.len(), 1);
        let name = accept(home, &list[0]).unwrap();
        assert_eq!(name, "reply-zh");
        assert!(home.join("skills/reply-zh/skill.yaml").exists());
        assert!(pending(home).unwrap().is_empty(), "proposal consumed");
    }

    #[test]
    fn accept_refuses_to_overwrite_and_dismiss_consumes() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        // Pre-existing global skill with the same name.
        let m = note_manifest(&NoteSpec {
            name: "dup",
            description: "d",
            body: "b",
            kind: NoteKind::Fact,
            publisher: "human:t",
        });
        mur_common::skill::store::write_to_dir(&home.join("skills/dup"), &m).unwrap();

        drop_proposal(home, "w1", "dup");
        let list = pending(home).unwrap();
        assert!(accept(home, &list[0]).is_err(), "must not overwrite");
        dismiss(&list[0]).unwrap();
        assert!(pending(home).unwrap().is_empty());
    }

    #[test]
    fn re_remember_replaces_instead_of_stacking() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        drop_proposal(home, "w1", "same-note");
        drop_proposal(home, "w1", "same-note");
        assert_eq!(
            pending(home).unwrap().len(),
            1,
            "deterministic file name dedups"
        );
    }

    // ── P2c-2 signature gate ─────────────────────────────────────────────

    use mur_common::identity::AgentIdentity;

    /// Drop a SIGNED proposal from a registered agent; returns its identity.
    fn drop_signed(home: &Path, agent: &str, name: &str, tamper: bool) -> AgentIdentity {
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(&dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&dir).unwrap();

        let mut p = MemoryProposal {
            agent: agent.into(),
            proposed_at: chrono::Utc::now(),
            manifest: note_manifest(&NoteSpec {
                name,
                description: "reply language",
                body: "always zh-TW",
                kind: NoteKind::Rule,
                publisher: &format!("agent:{agent}"),
            }),
            sig: None,
            key_version: 0,
        };
        p.sign(&id);
        if tamper {
            p.manifest.content.note = Some("always en-US".into()); // post-sign edit
        }
        write_memory_proposal(home, &p).unwrap();
        id
    }

    #[test]
    fn signed_proposal_lists_verified_and_accepts() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        drop_signed(home, "w1", "reply-zh", false);

        let list = pending(home).unwrap();
        assert_eq!(list[0].sig_status, ProposalSigStatus::Verified);
        assert_eq!(accept(home, &list[0]).unwrap(), "reply-zh");
    }

    #[test]
    fn tampered_proposal_lists_invalid_and_accept_refuses() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        drop_signed(home, "w1", "reply-zh", true);

        let list = pending(home).unwrap();
        assert!(matches!(list[0].sig_status, ProposalSigStatus::Invalid(_)));
        let err = accept(home, &list[0]).unwrap_err().to_string();
        assert!(err.contains("refusing"), "got: {err}");
        assert!(
            !home.join("skills/reply-zh/skill.yaml").exists(),
            "tampered proposal must not land"
        );
    }

    #[test]
    fn unsigned_legacy_proposal_still_accepts_by_default() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        drop_proposal(home, "w1", "legacy-note");

        let list = pending(home).unwrap();
        assert_eq!(list[0].sig_status, ProposalSigStatus::Unsigned);
        assert_eq!(accept(home, &list[0]).unwrap(), "legacy-note");
    }
}
