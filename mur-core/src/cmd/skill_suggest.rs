//! `mur skill suggest` — list harvest-proposal candidates worth turning into
//! skills/workflows. (The emergence/fingerprint miner was removed in
//! workflow-engine v2 P1a; the ambient-capture harvest gate is the source.)

use anyhow::Result;
use std::path::Path;

use crate::nudge::candidate::CandidateSource;
use crate::nudge::harvest_source::HarvestProposalSource;

pub struct SuggestOptions {
    /// Max most-recent sessions to scan. Default 20. (Kept for CLI compat;
    /// the proposal inbox is already bounded by the harvest gate.)
    pub max_sessions: usize,
    /// Minimum distinct sessions a pattern must appear in. Default 3.
    pub threshold: usize,
}

pub fn cmd_suggest(home: &Path, opts: SuggestOptions) -> Result<()> {
    let source = HarvestProposalSource::new(home.join("inbox").join("workflow-proposals"));
    let mut candidates = source.candidates(opts.threshold)?;
    candidates.truncate(opts.max_sessions);

    if candidates.is_empty() {
        println!(
            "No pending workflow proposals. (Proposals appear after sessions pass the harvest gate — see `mur session out`.)"
        );
        return Ok(());
    }

    println!("Found {} pending workflow proposal(s):\n", candidates.len());
    for (i, c) in candidates.iter().enumerate() {
        println!(
            "{}. {} ({} session{})",
            i + 1,
            c.suggested_name,
            c.session_count,
            if c.session_count == 1 { "" } else { "s" }
        );
        println!("   title: {}", c.title);
        if !c.steps_preview.is_empty() {
            println!("   steps: {}", c.steps_preview.join(" → "));
        }
        if let Some(first_session) = c.evidence_session_ids.first() {
            println!(
                "   generate: mur skill generate --from-session {}",
                first_session
            );
        }
        println!();
    }
    println!("Review and accept with `mur session out`.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harvest::proposal::{Proposal, ProposalStatus, save_in_dir};

    #[test]
    fn lists_pending_proposals() {
        let tmp = tempfile::TempDir::new().unwrap();
        let inbox = tmp.path().join("inbox").join("workflow-proposals");
        save_in_dir(
            &inbox,
            &Proposal {
                id: "s1".into(),
                project: None,
                title: "Deploy api".into(),
                suggested_name: "deploy-api".into(),
                steps: vec!["cargo build".into()],
                skeleton: vec!["cargo build".into()],
                event_count: 5,
                duration_secs: 60,
                occurrences: 2,
                created_at: "2026-06-11T00:00:00Z".into(),
                status: ProposalStatus::Pending,
                similar_to: None,
            },
        )
        .unwrap();
        cmd_suggest(
            tmp.path(),
            SuggestOptions {
                max_sessions: 20,
                threshold: 3,
            },
        )
        .unwrap();
    }

    #[test]
    fn empty_inbox_is_fine() {
        let tmp = tempfile::TempDir::new().unwrap();
        cmd_suggest(
            tmp.path(),
            SuggestOptions {
                max_sessions: 20,
                threshold: 3,
            },
        )
        .unwrap();
    }
}
