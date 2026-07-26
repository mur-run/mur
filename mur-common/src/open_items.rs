//! Open items shared between the CLI and the agent runtime.
//!
//! Two kinds of claim, never mixed. [`ItemSource::Observed`] is derived from
//! state MUR itself holds — a queued job, a file in the inbox — and cannot be
//! wrong about its own existence. [`ItemSource::Reported`] is what an agent
//! wrote down because it decided something was left undone: it catches
//! promises made in conversation that no file records, and nothing but the
//! agent's word says it is real.
//!
//! The types and the reported log live here rather than in `mur-core` because
//! the agent runtime writes to the log and does not (and should not) depend on
//! `mur-core`. Collection of observed items, and rendering, stay in `mur-core`.
//!
//! # Reported storage
//!
//! Append-only JSONL at `<mur_home>/open-items.jsonl`, folded on read. Same
//! idiom as the unified channel: never rewrite history, append the correction.
//! An agent that resolves an item appends a `Resolve` record; nothing is
//! deleted, so "what did it think was open last Tuesday" stays answerable.
//!
//! These are the agent's word and nothing more. `mur open` prints them under
//! their own heading for that reason — see the module docs one level up.

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::Utc;
use serde::{Deserialize, Serialize};

/// Whether MUR observed the item itself, or an agent merely reported it.
///
/// Displays keep the two apart. A panel that presents an agent's recollection
/// with the same confidence as a file on disk teaches the reader to distrust
/// all of it, and the failure mode of a status surface is not being wrong
/// once — it is being ignored forever after.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemSource {
    /// Derived from MUR's own state.
    Observed,
    /// Asserted by an agent.
    Reported,
}

impl ItemSource {
    /// Short marker for dense displays (a TUI footer, a status line).
    pub fn marker(&self) -> &'static str {
        match self {
            ItemSource::Observed => "●",
            ItemSource::Reported => "○",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ItemSource::Observed => "observed",
            ItemSource::Reported => "reported",
        }
    }

    fn rank(&self) -> u8 {
        match self {
            ItemSource::Observed => 0,
            ItemSource::Reported => 1,
        }
    }
}

impl PartialOrd for ItemSource {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for ItemSource {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.rank().cmp(&other.rank())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpenItem {
    /// One line, imperative where possible.
    pub title: String,
    /// The command or place that resolves it, when there is an obvious one.
    pub next: Option<String>,
    pub source: ItemSource,
    /// Where it came from: `"inbox"`, `"fleet:acme"`, `"agent:mur"`.
    pub origin: String,
    pub at: chrono::DateTime<chrono::Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Record {
    Open {
        id: String,
        agent: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next: Option<String>,
        at: chrono::DateTime<chrono::Utc>,
    },
    Resolve {
        id: String,
        at: chrono::DateTime<chrono::Utc>,
    },
}

fn log_path(mur_home: &Path) -> PathBuf {
    mur_home.join("open-items.jsonl")
}

/// Append one agent-reported item. Returns its id.
///
/// The id is derived from agent + title so the same claim repeated across
/// turns folds onto itself instead of stacking. An agent that keeps saying
/// "still need to write the tests" should occupy one line, not one per turn.
pub fn report(mur_home: &Path, agent: &str, title: &str, next: Option<&str>) -> Result<String> {
    let id = item_id(agent, title);
    let rec = Record::Open {
        id: id.clone(),
        agent: agent.to_string(),
        title: title.to_string(),
        next: next.map(|s| s.to_string()),
        at: Utc::now(),
    };
    append(mur_home, &rec)?;
    Ok(id)
}

/// Mark a reported item resolved. Unknown ids are accepted: the log is a
/// record of claims, and "I finished something you never saw me start" is a
/// coherent claim, not an error worth failing a turn over.
pub fn resolve(mur_home: &Path, id: &str) -> Result<()> {
    append(
        mur_home,
        &Record::Resolve {
            id: id.to_string(),
            at: Utc::now(),
        },
    )
}

fn append(mur_home: &Path, rec: &Record) -> Result<()> {
    let path = log_path(mur_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    writeln!(f, "{}", serde_json::to_string(rec)?)?;
    Ok(())
}

/// Fold the log into the items still open. A malformed line is skipped rather
/// than fatal — a half-written record must not take the whole panel down.
pub fn open(mur_home: &Path) -> Vec<OpenItem> {
    let Ok(body) = std::fs::read_to_string(log_path(mur_home)) else {
        return Vec::new();
    };
    let mut live: Vec<(String, OpenItem)> = Vec::new();
    for line in body.lines().filter(|l| !l.trim().is_empty()) {
        let Ok(rec) = serde_json::from_str::<Record>(line) else {
            continue;
        };
        match rec {
            Record::Open {
                id,
                agent,
                title,
                next,
                at,
            } => {
                let item = OpenItem {
                    title,
                    next,
                    source: ItemSource::Reported,
                    origin: format!("agent:{agent}"),
                    at,
                };
                match live.iter_mut().find(|(k, _)| *k == id) {
                    Some(slot) => slot.1 = item,
                    None => live.push((id, item)),
                }
            }
            Record::Resolve { id, .. } => live.retain(|(k, _)| *k != id),
        }
    }
    live.into_iter().map(|(_, v)| v).collect()
}

/// Stable across turns and processes, so a repeated claim folds.
fn item_id(agent: &str, title: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(agent.as_bytes());
    h.update(b"\0");
    h.update(title.trim().to_lowercase().as_bytes());
    format!("{:x}", h.finalize())[..12].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home() -> tempfile::TempDir {
        tempfile::tempdir().unwrap()
    }

    #[test]
    fn no_log_means_no_items() {
        assert!(open(home().path()).is_empty());
    }

    /// An agent that repeats itself every turn must not grow the list every
    /// turn — same claim, same line.
    #[test]
    fn the_same_claim_repeated_folds_onto_one_item() {
        let h = home();
        report(h.path(), "mur", "write the tests", None).unwrap();
        report(h.path(), "mur", "Write The Tests  ", Some("cargo test")).unwrap();
        let items = open(h.path());
        assert_eq!(items.len(), 1);
        // The later record wins, so a refined `next` reaches the reader.
        assert_eq!(items[0].next.as_deref(), Some("cargo test"));
    }

    /// Two agents can independently owe the same-sounding thing.
    #[test]
    fn different_agents_making_the_same_claim_stay_separate() {
        let h = home();
        report(h.path(), "a", "ship it", None).unwrap();
        report(h.path(), "b", "ship it", None).unwrap();
        assert_eq!(open(h.path()).len(), 2);
    }

    #[test]
    fn resolving_removes_it_without_rewriting_history() {
        let h = home();
        let id = report(h.path(), "mur", "write the tests", None).unwrap();
        resolve(h.path(), &id).unwrap();
        assert!(open(h.path()).is_empty());
        // Both records survive on disk — the log is append-only.
        let body = std::fs::read_to_string(log_path(h.path())).unwrap();
        assert_eq!(body.lines().count(), 2);
    }

    /// A truncated write must cost one item, not the whole panel.
    #[test]
    fn a_malformed_line_is_skipped_not_fatal() {
        let h = home();
        report(h.path(), "mur", "real item", None).unwrap();
        let mut f = std::fs::OpenOptions::new()
            .append(true)
            .open(log_path(h.path()))
            .unwrap();
        writeln!(f, "{{\"kind\":\"open\",\"id\":").unwrap();
        drop(f);
        report(h.path(), "mur", "later item", None).unwrap();
        assert_eq!(open(h.path()).len(), 2);
    }

    /// Resolving something never reported is a claim, not a crash.
    #[test]
    fn resolving_an_unknown_id_is_accepted() {
        let h = home();
        resolve(h.path(), "deadbeef").unwrap();
        assert!(open(h.path()).is_empty());
    }
}
