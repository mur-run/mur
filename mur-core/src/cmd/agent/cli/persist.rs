//! Per-conversation session persistence as append-only JSONL.
//!
//! Mirrors the runtime's `Ledger` convention (one JSON object per line, append
//! mode) but uses one file per conversation under
//! `~/.mur/agents/<agent>/cli-sessions/<session-id>.jsonl`. Session ids are
//! UUID v7 (time-sortable), so the lexically greatest filename is the newest —
//! that is what `--resume` loads.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Subdirectory under an agent's home that holds CLI chat transcripts.
pub const SESSIONS_SUBDIR: &str = "cli-sessions";
/// File extension for a saved conversation.
pub const SESSION_EXT: &str = "jsonl";
/// How many recent sessions `/sessions` lists.
pub const RECENT_LIMIT: usize = 10;

/// One persisted turn. `system` UI notes are not persisted — only the real
/// user/agent exchange that resume needs to reconstruct.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TurnRecord {
    /// RFC 3339 timestamp.
    pub ts: String,
    /// "user" or "agent".
    pub role: String,
    pub text: String,
    /// The agent turn's task id (used to thread context on resume).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
}

/// A handle to one conversation's transcript file.
pub struct Session {
    path: PathBuf,
}

/// Summary of a saved session for listing/resume.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    pub id: String,
    pub path: PathBuf,
    /// First user message, for a human-readable preview.
    pub preview: String,
    pub turns: usize,
}

fn sessions_dir(home: &Path, agent: &str) -> PathBuf {
    home.join("agents").join(agent).join(SESSIONS_SUBDIR)
}

impl Session {
    /// Create a fresh conversation file with a new time-sortable id.
    pub fn create(home: &Path, agent: &str) -> Result<Self> {
        let dir = sessions_dir(home, agent);
        fs::create_dir_all(&dir)
            .with_context(|| format!("create session dir {}", dir.display()))?;
        let id = uuid::Uuid::now_v7().to_string();
        let path = dir.join(format!("{id}.{SESSION_EXT}"));
        Ok(Self { path })
    }

    /// Wrap an existing transcript file (used by resume).
    pub fn from_path(path: PathBuf) -> Self {
        Self { path }
    }

    /// Append one turn. Append-only; durability matches the runtime ledger
    /// convention (the OS flushes; a crash loses at most the last line).
    pub fn append(&self, role: &str, text: &str, task_id: Option<&str>) -> Result<()> {
        let rec = TurnRecord {
            ts: chrono::Local::now().to_rfc3339(),
            role: role.to_string(),
            text: text.to_string(),
            task_id: task_id.map(str::to_string),
        };
        let mut f = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
            .with_context(|| format!("open session file {}", self.path.display()))?;
        let line = serde_json::to_string(&rec)?;
        writeln!(f, "{line}").with_context(|| format!("write {}", self.path.display()))?;
        Ok(())
    }

    /// Path to the transcript file (used by resume tooling and tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Read all turns from a transcript file, skipping malformed lines.
pub fn load(path: &Path) -> Result<Vec<TurnRecord>> {
    let content =
        fs::read_to_string(path).with_context(|| format!("read session {}", path.display()))?;
    Ok(content
        .lines()
        .filter(|l| !l.trim().is_empty())
        .filter_map(|l| serde_json::from_str::<TurnRecord>(l).ok())
        .collect())
}

/// List recent sessions for an agent, newest first.
pub fn list_recent(home: &Path, agent: &str, limit: usize) -> Result<Vec<SessionInfo>> {
    let dir = sessions_dir(home, agent);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut ids: Vec<(String, PathBuf)> = fs::read_dir(&dir)
        .with_context(|| format!("read session dir {}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|s| s.to_str()) == Some(SESSION_EXT))
        .filter_map(|p| {
            p.file_stem()
                .and_then(|s| s.to_str())
                .map(|id| (id.to_string(), p.clone()))
        })
        .collect();
    // UUID v7 ids sort chronologically; reverse for newest-first.
    ids.sort_by(|a, b| b.0.cmp(&a.0));
    ids.truncate(limit);

    Ok(ids
        .into_iter()
        .map(|(id, path)| {
            let turns = load(&path).unwrap_or_default();
            let preview = turns
                .iter()
                .find(|t| t.role == "user")
                .map(|t| t.text.clone())
                .unwrap_or_default();
            SessionInfo {
                id,
                path,
                preview,
                turns: turns.len(),
            }
        })
        .collect())
}

/// The most recent session for an agent, if any.
pub fn latest(home: &Path, agent: &str) -> Result<Option<SessionInfo>> {
    Ok(list_recent(home, agent, 1)?.into_iter().next())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn append_and_load_roundtrip() {
        let home = tempdir().unwrap();
        let s = Session::create(home.path(), "agentx").unwrap();
        s.append("user", "hello", None).unwrap();
        s.append("agent", "hi there", Some("task-1")).unwrap();

        let turns = load(s.path()).unwrap();
        assert_eq!(turns.len(), 2);
        assert_eq!(turns[0].role, "user");
        assert_eq!(turns[0].text, "hello");
        assert_eq!(turns[1].role, "agent");
        assert_eq!(turns[1].task_id.as_deref(), Some("task-1"));
    }

    #[test]
    fn list_recent_orders_newest_first_with_preview() {
        let home = tempdir().unwrap();
        let s1 = Session::create(home.path(), "a").unwrap();
        s1.append("user", "first convo", None).unwrap();
        // v7 ids are monotonic within a process; s2 is newer than s1.
        let s2 = Session::create(home.path(), "a").unwrap();
        s2.append("user", "second convo", None).unwrap();

        let recent = list_recent(home.path(), "a", RECENT_LIMIT).unwrap();
        assert_eq!(recent.len(), 2);
        assert_eq!(recent[0].preview, "second convo");
        assert_eq!(recent[1].preview, "first convo");
        assert_eq!(recent[0].turns, 1);
    }

    #[test]
    fn list_recent_empty_when_no_dir() {
        let home = tempdir().unwrap();
        assert!(
            list_recent(home.path(), "nobody", RECENT_LIMIT)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn load_skips_malformed_lines() {
        let home = tempdir().unwrap();
        let s = Session::create(home.path(), "a").unwrap();
        s.append("user", "ok", None).unwrap();
        // Corrupt append.
        {
            let mut f = OpenOptions::new().append(true).open(s.path()).unwrap();
            writeln!(f, "{{not valid json").unwrap();
        }
        s.append("agent", "still works", Some("t")).unwrap();
        let turns = load(s.path()).unwrap();
        assert_eq!(turns.len(), 2);
    }
}
