//! Stats aggregator: receives `StatsEvent` from the runtime's fan-out
//! adapter and flushes counter deltas into per-skill `stats.json` sidecars.
//!
//! Flush triggers: every 64 events or every 2 seconds, whichever comes first.

use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{Mutex, mpsc};

use crate::skill::stats::SkillStats;

pub const FLUSH_EVERY_EVENTS: usize = 64;
pub const FLUSH_EVERY: Duration = Duration::from_secs(2);

/// Mirrors `Event::SkillExecuted` without coupling mur-common to the
/// runtime's `Event` enum.
#[derive(Debug, Clone)]
pub struct StatsEvent {
    pub skill_name: String,
    pub skill_version: String,
    pub manifest_digest: String,
    pub success: bool,
    pub failure: bool,
    pub now: DateTime<Utc>,
}

#[derive(Debug, Default)]
struct Delta {
    usage: u64,
    success: u64,
    failure: u64,
    last_used_at: Option<DateTime<Utc>>,
    last_success_at: Option<DateTime<Utc>>,
    first_success_seen: Option<DateTime<Utc>>,
    manifest_digest: String,
    skill_version: String,
}

pub struct StatsAggregator {
    #[allow(dead_code)]
    mur_home: PathBuf,
    #[allow(dead_code)]
    deltas: Arc<Mutex<HashMap<String, Delta>>>,
}

impl StatsAggregator {
    /// Spawn the background flush task. Returns the handle so callers can
    /// hold a reference while the task runs. The task exits when `rx` is
    /// closed (all senders dropped).
    pub fn spawn(mur_home: PathBuf, mut rx: mpsc::Receiver<StatsEvent>) -> Self {
        let deltas: Arc<Mutex<HashMap<String, Delta>>> = Arc::default();
        let deltas_clone = Arc::clone(&deltas);
        let mur_home_clone = mur_home.clone();

        tokio::spawn(async move {
            let mut tick = tokio::time::interval(FLUSH_EVERY);
            let mut event_budget = FLUSH_EVERY_EVENTS;
            loop {
                tokio::select! {
                    _ = tick.tick() => {
                        flush(&mur_home_clone, &deltas_clone).await;
                    }
                    Some(ev) = rx.recv() => {
                        merge_one(&deltas_clone, ev).await;
                        event_budget = event_budget.saturating_sub(1);
                        if event_budget == 0 {
                            flush(&mur_home_clone, &deltas_clone).await;
                            event_budget = FLUSH_EVERY_EVENTS;
                        }
                    }
                    else => break, // channel closed — drain + exit
                }
            }
            flush(&mur_home_clone, &deltas_clone).await;
        });

        Self { mur_home, deltas }
    }
}

async fn merge_one(deltas: &Mutex<HashMap<String, Delta>>, ev: StatsEvent) {
    let mut map = deltas.lock().await;
    let d = map.entry(ev.skill_name.clone()).or_default();
    d.usage += 1;
    if ev.success {
        d.success += 1;
        d.last_success_at = Some(ev.now);
        if d.first_success_seen.is_none() {
            d.first_success_seen = Some(ev.now);
        }
    } else if ev.failure {
        d.failure += 1;
    }
    // For NotEvaluated, count as usage only (no success/failure increment)
    d.last_used_at = Some(ev.now);
    if d.manifest_digest.is_empty() {
        d.manifest_digest = ev.manifest_digest;
        d.skill_version = ev.skill_version;
    }
}

async fn flush(mur_home: &std::path::Path, deltas: &Mutex<HashMap<String, Delta>>) {
    let map = {
        let mut guard = deltas.lock().await;
        std::mem::take(&mut *guard)
    };

    for (skill_name, delta) in map {
        if delta.usage == 0 {
            continue;
        }
        let path = SkillStats::path(mur_home, &skill_name);
        let default = || {
            SkillStats::new(
                &skill_name,
                &delta.skill_version,
                &delta.manifest_digest,
                Utc::now(),
            )
        };
        let _ = SkillStats::merge_in_place(&path, default, |s| {
            // If manifest changed, reset counters but preserve pinned + state
            if s.is_stale(&delta.manifest_digest) {
                s.reset_for_new_manifest(&delta.skill_version, &delta.manifest_digest, Utc::now());
            }
            s.usage_count += delta.usage;
            s.success_count += delta.success;
            s.failure_count += delta.failure;
            if let Some(t) = delta.last_used_at {
                s.last_used_at = Some(match s.last_used_at {
                    Some(existing) => existing.max(t),
                    None => t,
                });
            }
            if let Some(t) = delta.last_success_at {
                s.last_success_at = Some(match s.last_success_at {
                    Some(existing) => existing.max(t),
                    None => t,
                });
            }
            if let Some(t) = delta.first_success_seen {
                s.first_successful_use_at = Some(match s.first_successful_use_at {
                    Some(existing) => existing.min(t),
                    None => t,
                });
            }
            Ok(())
        });
    }
}
