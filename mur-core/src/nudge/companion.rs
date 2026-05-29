use anyhow::Result;
use std::path::{Path, PathBuf};

use crate::nudge::candidate::WorkflowCandidate;

/// Message id namespace so the response drain can recognize nudge replies.
pub const NUDGE_ID_PREFIX: &str = "nudge:";

pub fn nudge_msg_id(candidate_id: &str) -> String {
    format!("{NUDGE_ID_PREFIX}{candidate_id}")
}

pub fn candidate_id_from_msg(msg_id: &str) -> Option<String> {
    msg_id.strip_prefix(NUDGE_ID_PREFIX).map(|s| s.to_string())
}

/// Deterministic, locale-aware nudge body. English default for v1.
pub fn nudge_body(c: &WorkflowCandidate, _locale: &str) -> String {
    format!(
        "I noticed you did this across {} sessions:\n\n  {}\n\nWant me to save it as a replayable workflow you can run with `mur run {}`?",
        c.session_count, c.title, c.suggested_name
    )
}

/// Write a nudge as a companion inbox `.md` (same frontmatter format as the
/// runtime's write_inbox_md). Returns the path. No-op (Ok) if a file for this
/// id already exists (create_new semantics).
pub fn write_nudge_inbox(
    inbox_dir: &Path,
    c: &WorkflowCandidate,
    locale: &str,
) -> Result<PathBuf> {
    let id = nudge_msg_id(&c.id);
    // Sanitize the ':' in the id for the filesystem.
    let filename = format!("{}.md", id.replace(':', "_"));
    let file = inbox_dir.join(&filename);

    std::fs::create_dir_all(inbox_dir)?;

    // create_new: skip if already exists (idempotent)
    let mut f = match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&file)
    {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => return Ok(file),
        Err(e) => return Err(e.into()),
    };

    let generated_at = chrono::Utc::now().to_rfc3339();
    let body = nudge_body(c, locale);
    let content = format!(
        "---\nid: {id}\nsituation: workflow_nudge\ntemplate_id: nudge\nlocale: {locale}\ngenerated_at: {generated_at}\n---\n\n{body}\n\n>>> response: <unset>"
    );
    std::io::Write::write_all(&mut f, content.as_bytes())?;

    Ok(file)
}

/// Write each candidate to every companion-enabled agent's inbox.
/// Returns the number of (agent × candidate) messages written.
pub fn deliver_nudges_to_companions(
    mur_dir: &Path,
    candidates: &[WorkflowCandidate],
    locale: &str,
) -> Result<usize> {
    let agents_dir = mur_dir.join("agents");
    if !agents_dir.exists() || candidates.is_empty() {
        return Ok(0);
    }
    let mut written = 0;
    for entry in std::fs::read_dir(&agents_dir)? {
        let dir = entry?.path();
        let profile = dir.join("profile.yaml");
        if !profile.is_file() {
            continue;
        }
        let body = std::fs::read_to_string(&profile)?;
        let prof: mur_common::agent::AgentProfile = match serde_yaml_ng::from_str(&body) {
            Ok(p) => p,
            Err(_) => continue,
        };
        if !prof.companion.enabled {
            continue;
        }
        let inbox = dir.join("companion").join("inbox");
        let loc = if prof.companion.locale.is_empty() {
            locale
        } else {
            &prof.companion.locale
        };
        for c in candidates {
            write_nudge_inbox(&inbox, c, loc)?;
            written += 1;
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::nudge::candidate::WorkflowCandidate;

    fn cand() -> WorkflowCandidate {
        WorkflowCandidate {
            id: "abc123".into(),
            title: "Run tests, then commit, then push".into(),
            suggested_name: "test-commit-push".into(),
            steps_preview: vec!["cargo test".into(), "git commit".into()],
            session_count: 4,
            evidence_session_ids: vec![],
        }
    }

    #[test]
    fn body_mentions_title_and_count() {
        let b = nudge_body(&cand(), "en");
        assert!(b.contains("Run tests, then commit, then push"));
        assert!(b.contains("4")); // session count
    }

    #[test]
    fn message_id_encodes_candidate_id() {
        assert_eq!(nudge_msg_id("abc123"), "nudge:abc123");
        assert_eq!(
            candidate_id_from_msg("nudge:abc123"),
            Some("abc123".to_string())
        );
        assert_eq!(candidate_id_from_msg("other"), None);
    }

    #[test]
    fn writes_nudge_inbox_md_with_response_marker() {
        let dir = tempfile::tempdir().unwrap();
        let inbox = dir.path().join("inbox");
        let c = cand();
        let path = write_nudge_inbox(&inbox, &c, "en").unwrap();
        let s = std::fs::read_to_string(&path).unwrap();
        assert!(s.contains("situation: workflow_nudge"));
        assert!(s.contains("id: nudge:abc123"));
        assert!(s.trim_end().ends_with(">>> response: <unset>"));
        // idempotent: second write for same id is a no-op (already exists)
        assert!(write_nudge_inbox(&inbox, &c, "en").is_ok());
    }

    #[test]
    fn deliver_writes_to_enabled_agent_inboxes_only() {
        let mur = tempfile::tempdir().unwrap();
        // Build minimal profiles using default_for_tests + companion block.
        let mut prof = mur_common::agent::AgentProfile::default_for_tests();
        let base_yaml = serde_yaml_ng::to_string(&prof).unwrap();
        // agent "on": profile with companion.enabled = true
        prof.companion.enabled = true;
        let on_yaml = serde_yaml_ng::to_string(&prof).unwrap();
        // agent "off": profile with companion.enabled = false
        prof.companion.enabled = false;
        let off_yaml = serde_yaml_ng::to_string(&prof).unwrap();
        let on_dir = mur.path().join("agents/on");
        std::fs::create_dir_all(&on_dir).unwrap();
        std::fs::write(on_dir.join("profile.yaml"), on_yaml).unwrap();
        let off_dir = mur.path().join("agents/off");
        std::fs::create_dir_all(&off_dir).unwrap();
        std::fs::write(off_dir.join("profile.yaml"), off_yaml).unwrap();
        let c = cand();
        let n = deliver_nudges_to_companions(mur.path(), &[c], "en").unwrap();
        assert_eq!(n, 1); // only the "on" agent got it
        assert!(on_dir
            .join("companion/inbox/nudge_abc123.md")
            .exists());
        assert!(!off_dir
            .join("companion/inbox/nudge_abc123.md")
            .exists());
    }
}
