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

/// A pending proposal plus the file it came from.
pub struct PendingMemoryProposal {
    pub path: PathBuf,
    pub proposal: MemoryProposal,
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
            Ok(proposal) => out.push(PendingMemoryProposal { path, proposal }),
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
}
