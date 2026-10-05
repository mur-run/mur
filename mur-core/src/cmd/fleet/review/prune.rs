//! §7.1 / AC15b: `mur fleet prune-reviews` — the manual, never-timed erase
//! of retained review-session channels.
//!
//! Same posture as `mur monitor prune`: `--older-than` is required, there is
//! no automatic GC behind it, and it only ever takes sessions that are OVER.
//! A review channel is a candidate when:
//!
//! - its session has ended — a `session_stopped` event with no later
//!   `resumed` — or it carries a readable corrupted marker (§8.2 "Prune
//!   equivalence"); and
//! - its fleet definition is gone. A running or paused session keeps its
//!   `review-…` fleet definition (A1), so a channel whose fleet still exists
//!   is never touched, whatever its events say; and
//! - its last activity — the later of its last readable event and the
//!   marker's `detected_at` — is older than the cutoff.
//!
//! An unparseable marker is reported by path and never counts as evidence
//! (§8.2: "Prune never removes a session based on unreadable evidence").

use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use mur_channel::ChannelService;
use serde::Deserialize;

use super::constants::{REVIEW_CORRUPTED_MARKER_FILE, REVIEW_FLEET_PREFIX};
use super::schema::{NoteClassification, ReviewPayload, classify_note_payload};
use crate::cmd::fleet::store;

/// `create_for_fleet` names a fleet's channel `fleet-<fleet name>`.
const FLEET_CHANNEL_PREFIX: &str = "fleet-";

/// The §8.2 marker, read-only here. Only the fields prune needs are
/// required; a marker missing them is unreadable evidence.
#[derive(Debug, Deserialize)]
struct CorruptedMarker {
    detected_at: DateTime<Utc>,
}

/// Why a channel is (or is not) prunable.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Verdict {
    Candidate {
        why: &'static str,
        last: DateTime<Utc>,
    },
    /// Still live, or no evidence it ended — never pruned, not reported.
    Keep,
    /// The marker exists but cannot be read: reported, never pruned.
    UnreadableMarker(PathBuf),
}

/// The review fleet name a review channel belongs to, if it is one.
fn review_session_of(channel_id: &str) -> Option<&str> {
    channel_id
        .strip_prefix(FLEET_CHANNEL_PREFIX)
        .filter(|name| name.starts_with(REVIEW_FLEET_PREFIX))
}

fn classify(
    svc: &ChannelService,
    mur_home: &Path,
    channel_id: &str,
    session: &str,
) -> Result<Verdict> {
    if store::fleet_path(mur_home, session).exists() {
        return Ok(Verdict::Keep);
    }
    let events = svc.load_events(channel_id)?;
    let mut last = events.iter().map(|e| e.ts).max();
    let mut stopped = false;
    for ev in &events {
        if let NoteClassification::Review(env) = classify_note_payload(&ev.payload) {
            match env.payload {
                ReviewPayload::SessionStopped { .. } => stopped = true,
                ReviewPayload::Resumed { .. } | ReviewPayload::ResumedFromCheckpoint { .. } => {
                    stopped = false;
                }
                _ => {}
            }
        }
    }
    let marker_path = svc
        .store()
        .events_path(channel_id)
        .with_file_name(REVIEW_CORRUPTED_MARKER_FILE);
    let mut corrupted = false;
    if marker_path.exists() {
        let parsed = std::fs::read_to_string(&marker_path)
            .ok()
            .and_then(|s| serde_json::from_str::<CorruptedMarker>(&s).ok());
        match parsed {
            Some(m) => {
                corrupted = true;
                last = Some(last.map_or(m.detected_at, |l| l.max(m.detected_at)));
            }
            None if !stopped => return Ok(Verdict::UnreadableMarker(marker_path)),
            None => {}
        }
    }
    let why = match (stopped, corrupted) {
        (true, _) => "stopped",
        (false, true) => "corrupted",
        (false, false) => return Ok(Verdict::Keep),
    };
    Ok(match last {
        Some(last) => Verdict::Candidate { why, last },
        None => Verdict::Keep,
    })
}

/// Erase review channels that ended more than `older_than` ago. With
/// `dry_run`, only lists them.
pub fn prune_reviews(
    mur_home: &Path,
    older_than: &str,
    dry_run: bool,
    out: &mut dyn Write,
    now: DateTime<Utc>,
) -> Result<()> {
    let age = mur_common::limits::parse_duration(older_than).ok_or_else(|| {
        anyhow::anyhow!(
            "unrecognised duration `{older_than}` — use a number of seconds or a \
             unit suffix, e.g. `30d`, `36h`, `90m`"
        )
    })?;
    let age = chrono::Duration::from_std(age)
        .with_context(|| format!("duration `{older_than}` is too large"))?;
    let cutoff = now - age;

    let svc = ChannelService::open(mur_home)?;
    let mut ids = svc.store().list_ids()?;
    ids.sort();
    let mut pruned = 0usize;
    for id in &ids {
        let Some(session) = review_session_of(id) else {
            continue;
        };
        match classify(&svc, mur_home, id, session)? {
            Verdict::Candidate { why, last } if last <= cutoff => {
                if dry_run {
                    writeln!(out, "would prune {session} ({why}, last activity {last})")?;
                } else {
                    svc.delete_channel(id)?;
                    writeln!(out, "pruned {session} ({why}, last activity {last})")?;
                }
                pruned += 1;
            }
            Verdict::UnreadableMarker(path) => writeln!(
                out,
                "skipped {session}: corrupted marker cannot be read ({})",
                path.display()
            )?,
            Verdict::Candidate { .. } | Verdict::Keep => {}
        }
    }
    match (pruned, dry_run) {
        (0, _) => writeln!(
            out,
            "nothing to prune — no stopped or corrupted review session has been idle longer than {older_than}"
        )?,
        (n, true) => writeln!(
            out,
            "{n} review session(s) would be pruned (dry run, nothing erased)"
        )?,
        (n, false) => writeln!(out, "{n} review session(s) pruned")?,
    }
    Ok(())
}

#[cfg(test)]
#[path = "prune_tests.rs"]
mod prune_tests;
