//! Multi-turn session persistence for Phase 3.3 `mur ask --continue`.
//!
//! One session = one JSONL file at `~/.mur/conversations/ask-session.jsonl`.
//! Each line is a `TurnRecord`. `SessionStore` provides load / archive / append.
//! No summarization, no named sessions — see spec §2 for deferrals.
#![allow(dead_code)] // wired progressively across Tasks 3-7.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs::OpenOptions;
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

use super::{Citation, HitInfo};

/// Rewriter disposition for a turn. Stored in `TurnRecord.rewriter_status`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RewriterStatus {
    /// Turn 1, `--continue` not passed, or session empty → no LLM call.
    Skipped,
    /// Rewriter returned a differing standalone question.
    Rewrote,
    /// Rewriter echoed the raw question verbatim (LangChain "return as is").
    NoRewriteNeeded,
    /// Ollama error on rewrite; retrieval used the raw question.
    FailedFellBackToRaw,
}

/// Single turn event in an Ask session. Append-only JSONL; one line per turn.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    pub v: u32,
    pub turn_id: u32,
    pub ts: DateTime<Utc>,
    pub question: String,
    pub rewritten_question: Option<String>,
    pub hits_used: Vec<HitInfo>,
    pub answer: String,
    pub citations: Vec<Citation>,
    pub degraded_to_mode_b: bool,
    pub rewriter_status: RewriterStatus,
    pub tokens_in: usize,
    pub tokens_out: usize,
    pub duration_ms: u64,
}

/// In-memory view of the Ask session.
pub struct Session {
    pub turns: Vec<TurnRecord>,
    path: PathBuf,
}

impl Session {
    /// Last `n` turns, oldest first. Empty slice if session empty.
    pub fn last_n(&self, n: u32) -> &[TurnRecord] {
        let n = n as usize;
        if self.turns.len() <= n {
            &self.turns[..]
        } else {
            &self.turns[self.turns.len() - n..]
        }
    }

    /// Turn-id for the next `append_turn`.
    pub fn next_turn_id(&self) -> u32 {
        self.turns.last().map(|t| t.turn_id + 1).unwrap_or(1)
    }
}

/// Loader / archiver / appender. All public entry points take `root_override`
/// so tests can point at a tempdir.
pub struct SessionStore;

impl SessionStore {
    /// Load the current session from disk. Returns an empty `Session` if the
    /// file is missing or empty (not an error — caller decides policy).
    /// Malformed lines are skipped with a `tracing::warn!`; the rest load.
    pub fn load_latest(root_override: Option<&str>) -> Result<Session> {
        let path = crate::conversations::paths::ask_session_path(root_override);
        let mut session = Session {
            turns: Vec::new(),
            path: path.clone(),
        };
        if !path.exists() {
            return Ok(session);
        }
        let file = std::fs::File::open(&path).with_context(|| format!("open {path:?}"))?;
        for (i, line) in BufReader::new(file).lines().enumerate() {
            let line = match line {
                Ok(l) => l,
                Err(e) => {
                    tracing::warn!("session line {i}: read error: {e:#}");
                    continue;
                }
            };
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            match serde_json::from_str::<TurnRecord>(line) {
                Ok(t) => session.turns.push(t),
                Err(e) => tracing::warn!("session line {i}: malformed TurnRecord, skipping: {e}"),
            }
        }
        Ok(session)
    }

    /// Append a turn to the session file + update the in-memory turns vec.
    /// Creates the file (and parent dir) if missing.
    /// `sync_all()` before return to guarantee crash durability of the line.
    pub fn append_turn(session: &mut Session, turn: TurnRecord) -> Result<()> {
        if let Some(parent) = session.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let line = serde_json::to_string(&turn).context("serialize TurnRecord")?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&session.path)
            .with_context(|| format!("open session for append {:?}", session.path))?;
        file.write_all(line.as_bytes())?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        session.turns.push(turn);
        Ok(())
    }

    /// Archive the current session into `.history/<utc>.jsonl` and return a
    /// fresh empty `Session`. No-op if the active file is missing or empty.
    /// `retain` caps `.history/` by count (oldest dropped first).
    pub fn archive_and_new(root_override: Option<&str>, retain: u32) -> Result<Session> {
        let path = crate::conversations::paths::ask_session_path(root_override);
        let hist_dir = crate::conversations::paths::ask_session_history_dir(root_override);

        // Archive only if active file exists and is non-empty.
        let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
        if size > 0 {
            std::fs::create_dir_all(&hist_dir)?;
            let stamp = Utc::now().format("%Y-%m-%dT%H-%M-%SZ").to_string();
            let dest = hist_dir.join(format!("{stamp}.jsonl"));
            std::fs::rename(&path, &dest)
                .with_context(|| format!("archive {path:?} -> {dest:?}"))?;
            prune_history(&hist_dir, retain)?;
        }
        Ok(Session {
            turns: Vec::new(),
            path,
        })
    }
}

/// Keep the `retain` most recent files in `hist_dir`; delete older.
/// Sort ascending by name — since files are named `YYYY-MM-DDTHH-MM-SSZ.jsonl`,
/// alphabetical == chronological.
fn prune_history(hist_dir: &std::path::Path, retain: u32) -> Result<()> {
    if !hist_dir.exists() {
        return Ok(());
    }
    let mut entries: Vec<std::path::PathBuf> = std::fs::read_dir(hist_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some("jsonl"))
        .collect();
    if entries.len() <= retain as usize {
        return Ok(());
    }
    entries.sort();
    let drop_count = entries.len() - retain as usize;
    for p in entries.into_iter().take(drop_count) {
        let _ = std::fs::remove_file(&p);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dummy_turn(id: u32, q: &str) -> TurnRecord {
        TurnRecord {
            v: 1,
            turn_id: id,
            ts: chrono::DateTime::parse_from_rfc3339("2026-04-21T15:30:00Z")
                .unwrap()
                .with_timezone(&Utc),
            question: q.into(),
            rewritten_question: None,
            hits_used: vec![],
            answer: format!("answer for {q}"),
            citations: vec![],
            degraded_to_mode_b: false,
            rewriter_status: RewriterStatus::Skipped,
            tokens_in: 100,
            tokens_out: 50,
            duration_ms: 1000,
        }
    }

    #[test]
    fn load_latest_on_missing_file_returns_empty_session() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let session = SessionStore::load_latest(Some(root)).unwrap();
        assert!(session.turns.is_empty());
    }

    #[test]
    fn load_latest_parses_valid_jsonl_in_order() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let p = crate::conversations::paths::ask_session_path(Some(root));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let t1 = serde_json::to_string(&dummy_turn(1, "q1")).unwrap();
        let t2 = serde_json::to_string(&dummy_turn(2, "q2")).unwrap();
        std::fs::write(&p, format!("{t1}\n{t2}\n")).unwrap();

        let session = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(session.turns.len(), 2);
        assert_eq!(session.turns[0].turn_id, 1);
        assert_eq!(session.turns[1].question, "q2");
    }

    #[test]
    fn append_turn_creates_file_if_missing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut session = SessionStore::load_latest(Some(root)).unwrap();
        assert!(session.turns.is_empty());

        SessionStore::append_turn(&mut session, dummy_turn(1, "q1")).unwrap();

        let loaded = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(loaded.turns.len(), 1);
        assert_eq!(loaded.turns[0].question, "q1");
    }

    #[test]
    fn append_turn_appends_to_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut session = SessionStore::load_latest(Some(root)).unwrap();
        SessionStore::append_turn(&mut session, dummy_turn(1, "q1")).unwrap();
        SessionStore::append_turn(&mut session, dummy_turn(2, "q2")).unwrap();

        let loaded = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(loaded.turns.len(), 2);
        assert_eq!(loaded.turns[1].question, "q2");
    }

    #[test]
    fn archive_and_new_renames_prior_to_history() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        // Seed an existing session
        let mut session = SessionStore::load_latest(Some(root)).unwrap();
        SessionStore::append_turn(&mut session, dummy_turn(1, "first")).unwrap();
        assert!(crate::conversations::paths::ask_session_path(Some(root)).exists());

        let fresh = SessionStore::archive_and_new(Some(root), 5).unwrap();
        assert!(fresh.turns.is_empty());
        assert!(
            !crate::conversations::paths::ask_session_path(Some(root)).exists(),
            "active session should be gone after archive"
        );
        let hist = crate::conversations::paths::ask_session_history_dir(Some(root));
        let entries: Vec<_> = std::fs::read_dir(&hist).unwrap().collect();
        assert_eq!(entries.len(), 1, "one archived session expected");
    }

    #[test]
    fn archive_and_new_is_noop_on_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let fresh = SessionStore::archive_and_new(Some(root), 5).unwrap();
        assert!(fresh.turns.is_empty());
        let hist = crate::conversations::paths::ask_session_history_dir(Some(root));
        assert!(!hist.exists() || std::fs::read_dir(&hist).unwrap().count() == 0);
    }

    #[test]
    fn archive_and_new_prunes_history_per_retain_config() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        // Seed 7 rapid archives with retain=5; distinct content so each actually archives.
        for i in 0..7 {
            let mut s = SessionStore::load_latest(Some(root)).unwrap();
            SessionStore::append_turn(&mut s, dummy_turn(1, &format!("q{i}"))).unwrap();
            SessionStore::archive_and_new(Some(root), 5).unwrap();
            // Each archive uses seconds-granularity timestamps, so pause briefly
            // to guarantee distinct filenames.
            std::thread::sleep(std::time::Duration::from_millis(1100));
        }
        let hist = crate::conversations::paths::ask_session_history_dir(Some(root));
        let entries: Vec<_> = std::fs::read_dir(&hist)
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(entries.len(), 5, "retain=5 should cap history at 5");
    }

    #[test]
    fn last_n_returns_correct_slice() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut s = SessionStore::load_latest(Some(root)).unwrap();
        for i in 1..=5 {
            SessionStore::append_turn(&mut s, dummy_turn(i, &format!("q{i}"))).unwrap();
        }
        let last3 = s.last_n(3);
        assert_eq!(last3.len(), 3);
        assert_eq!(last3[0].turn_id, 3);
        assert_eq!(last3[2].turn_id, 5);

        let last10 = s.last_n(10);
        assert_eq!(
            last10.len(),
            5,
            "asking for more than available returns all"
        );
    }

    #[test]
    fn next_turn_id_is_1_when_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let s = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(s.next_turn_id(), 1);
    }

    #[test]
    fn next_turn_id_is_last_plus_1() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let mut s = SessionStore::load_latest(Some(root)).unwrap();
        SessionStore::append_turn(&mut s, dummy_turn(1, "q")).unwrap();
        SessionStore::append_turn(&mut s, dummy_turn(2, "q")).unwrap();
        assert_eq!(s.next_turn_id(), 3);
    }

    #[test]
    fn load_latest_skips_malformed_lines_with_warning() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_str().unwrap();
        let p = crate::conversations::paths::ask_session_path(Some(root));
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        let good1 = serde_json::to_string(&dummy_turn(1, "q1")).unwrap();
        let good2 = serde_json::to_string(&dummy_turn(2, "q2")).unwrap();
        std::fs::write(&p, format!("{good1}\nthis is not JSON at all\n{good2}\n")).unwrap();

        let session = SessionStore::load_latest(Some(root)).unwrap();
        assert_eq!(
            session.turns.len(),
            2,
            "malformed line should be skipped, both good turns preserved"
        );
        assert_eq!(session.turns[0].turn_id, 1);
        assert_eq!(session.turns[1].turn_id, 2);
    }
}
