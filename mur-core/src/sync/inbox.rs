//! Receive side of the sync protocol — reads Signal YAML files from
//! `~/.mur/inbox/` and applies Evidence updates to patterns via [`YamlStore`].

use anyhow::{Context, Result};
use chrono::Utc;
use mur_common::pattern::Contribution;
use mur_common::{Signal, SignalKind, SignalTarget};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use uuid::Uuid;

use crate::store::yaml::YamlStore;

/// Receive side of the sync protocol — reads Signal YAML files and applies
/// Evidence updates to patterns via [`YamlStore`].
pub struct Inbox {
    dir: PathBuf,
}

/// Summary of an [`Inbox::apply_all`] run.
#[derive(Debug, Default)]
pub struct ApplyReport {
    pub applied: u64,
    pub skipped: u64,
    pub errors: Vec<String>,
}

impl Inbox {
    /// Open an inbox rooted at the given directory (creates it if missing).
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// Open the default inbox at `$HOME/.mur/inbox/`.
    pub fn default_location() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME"))?;
        Self::new(home.join(".mur/inbox"))
    }

    /// Persist a signal received from the server into the inbox (used by the
    /// fetcher before `apply_all`).
    pub fn receive(&self, signal: &Signal) -> Result<PathBuf> {
        let name = format!(
            "{}-{}.yaml",
            signal.emitted_at.format("%Y-%m-%dT%H-%M-%S"),
            signal.id
        );
        let path = self.dir.join(&name);
        let tmp = self.dir.join(format!(".{}.tmp", name));
        let yaml = serde_yaml::to_string(signal)
            .with_context(|| format!("serialize signal {}", signal.id))?;
        std::fs::write(&tmp, yaml)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Apply every YAML file in the inbox (non-hidden) to the given store.
    ///
    /// Successfully-applied or intentionally-skipped files are removed from
    /// the inbox; failures stay in place to be retried next run.
    ///
    /// Signal IDs are tracked in `.seen.yaml` to prevent double-counting when
    /// the same signal UUID is re-emitted (e.g. after a retry in FlushService).
    pub fn apply_all(&self, store: &YamlStore) -> Result<ApplyReport> {
        let mut report = ApplyReport::default();
        let mut seen = self.load_seen_ids();
        let mut newly_seen: Vec<Uuid> = Vec::new();

        for entry in std::fs::read_dir(&self.dir)? {
            let p = entry?.path();
            if !is_inbox_yaml(&p) {
                continue;
            }

            let yaml = match std::fs::read_to_string(&p) {
                Ok(s) => s,
                Err(e) => {
                    report
                        .errors
                        .push(format!("{}: read error: {e}", p.display()));
                    continue;
                }
            };
            let signal: Signal = match serde_yaml::from_str(&yaml) {
                Ok(s) => s,
                Err(e) => {
                    report
                        .errors
                        .push(format!("{}: parse error: {e}", p.display()));
                    continue;
                }
            };

            // Skip duplicate signal IDs (idempotency guard against FlushService retries)
            if seen.contains(&signal.id) {
                report.skipped += 1;
                let _ = std::fs::remove_file(&p);
                continue;
            }

            match self.apply_one(store, &signal) {
                Ok(true) => {
                    report.applied += 1;
                    newly_seen.push(signal.id);
                    let _ = std::fs::remove_file(&p);
                }
                Ok(false) => {
                    report.skipped += 1;
                    newly_seen.push(signal.id);
                    let _ = std::fs::remove_file(&p);
                }
                Err(e) => {
                    report.errors.push(format!("{}: {e}", p.display()));
                    // Keep file for retry — do NOT record as seen
                }
            }
        }

        if !newly_seen.is_empty() {
            seen.extend(newly_seen);
            let _ = self.save_seen_ids(&seen);
        }

        Ok(report)
    }

    fn load_seen_ids(&self) -> HashSet<Uuid> {
        let path = self.dir.join(".seen.yaml");
        std::fs::read_to_string(&path)
            .ok()
            .and_then(|s| serde_yaml::from_str::<Vec<Uuid>>(&s).ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default()
    }

    fn save_seen_ids(&self, seen: &HashSet<Uuid>) -> Result<()> {
        let mut ids: Vec<Uuid> = seen.iter().copied().collect();
        ids.sort();
        // Cap at 2048 to prevent unbounded growth
        if ids.len() > 2048 {
            let start = ids.len() - 2048;
            ids = ids[start..].to_vec();
        }
        let yaml = serde_yaml::to_string(&ids)?;
        let tmp = self.dir.join(".seen.tmp");
        std::fs::write(&tmp, &yaml)?;
        std::fs::rename(&tmp, self.dir.join(".seen.yaml"))?;
        Ok(())
    }

    fn apply_one(&self, store: &YamlStore, signal: &Signal) -> Result<bool> {
        match &signal.target {
            SignalTarget::Pattern { name, .. } => {
                if !store.exists(name) {
                    // Pattern not present locally — skip (not an error)
                    return Ok(false);
                }
                let mut pattern = store.get(name)?;

                let actor_key = signal.actor.key();
                let contribution = pattern
                    .evidence
                    .contributions
                    .entry(actor_key)
                    .or_insert_with(|| Contribution {
                        success_signals: 0,
                        override_signals: 0,
                        last_seen: Utc::now(),
                    });
                contribution.last_seen = signal.emitted_at;

                match &signal.kind {
                    SignalKind::ExecutionSuccess => {
                        contribution.success_signals += 1;
                        pattern.evidence.success_signals += 1;
                    }
                    SignalKind::ExecutionFailure { .. } => {
                        pattern.evidence.failure_signals += 1;
                    }
                    SignalKind::UserOverrideAtBreakpoint { .. } => {
                        contribution.override_signals += 3; // spec §4.1 guard rail: 3x weight
                        pattern.evidence.override_signals += 3;
                    }
                    SignalKind::AutoFixApplied { .. } => {
                        contribution.override_signals += 1;
                        pattern.evidence.override_signals += 1;
                    }
                    SignalKind::NewPatternProposal { .. } => {
                        // A proposal should have arrived as NewDraftPattern target;
                        // Pattern-targeted NewPatternProposal is a shape mismatch.
                        return Ok(false);
                    }
                }
                store.save(&pattern)?;
                Ok(true)
            }
            SignalTarget::NewDraftPattern { payload } => {
                // Never overwrite a pattern the user may have edited
                if store.exists(&payload.name) {
                    return Ok(false);
                }
                store.save(payload)?;
                Ok(true)
            }
        }
    }
}

fn is_inbox_yaml(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    if p.extension().and_then(|s| s.to_str()) != Some("yaml") {
        return false;
    }
    p.file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|n| !n.starts_with('.'))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::yaml::YamlStore;
    use mur_common::knowledge::KnowledgeBase;
    use mur_common::pattern::{Content, Pattern, Tier};
    use mur_common::{Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope};
    use tempfile::tempdir;
    use uuid::Uuid;

    /// Build a minimal Pattern with a fixed name, saveable by YamlStore.
    fn make_pattern(name: &str) -> Pattern {
        Pattern {
            base: KnowledgeBase {
                name: name.into(),
                description: "test pattern".into(),
                content: Content::Plain("test".into()),
                tier: Tier::Session,
                ..Default::default()
            },
            kind: None,
            origin: None,
            attachments: Vec::new(),
        }
    }

    fn signal(target_name: &str, kind: SignalKind, actor_native: &str) -> Signal {
        Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: Actor {
                source: ActorSource::Slack,
                native_id: actor_native.into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Pattern {
                name: target_name.into(),
                scope: Scope::Personal,
            },
            kind,
            scope: Scope::Personal,
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
        }
    }

    fn setup(tmp_dir: &Path) -> (YamlStore, Inbox) {
        let store = YamlStore::new(tmp_dir.join("patterns")).unwrap();
        let inbox = Inbox::new(tmp_dir.join("inbox")).unwrap();
        (store, inbox)
    }

    #[test]
    fn apply_execution_success_updates_contributions_and_global() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();

        let sig = signal("p1", SignalKind::ExecutionSuccess, "alice");
        inbox.receive(&sig).unwrap();
        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.skipped, 0);
        assert_eq!(report.errors.len(), 0);

        let p = store.get("p1").unwrap();
        assert_eq!(p.evidence.success_signals, 1);
        let contrib = p
            .evidence
            .contributions
            .get("Slack:alice")
            .expect("alice contrib");
        assert_eq!(contrib.success_signals, 1);
        assert_eq!(contrib.override_signals, 0);
    }

    #[test]
    fn apply_override_weights_3x() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();

        let sig = signal(
            "p1",
            SignalKind::UserOverrideAtBreakpoint { reason: None },
            "alice",
        );
        inbox.receive(&sig).unwrap();
        inbox.apply_all(&store).unwrap();

        let p = store.get("p1").unwrap();
        assert_eq!(p.evidence.override_signals, 3);
        assert_eq!(
            p.evidence
                .contributions
                .get("Slack:alice")
                .unwrap()
                .override_signals,
            3
        );
    }

    #[test]
    fn apply_autofix_weights_1x() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();

        let sig = signal(
            "p1",
            SignalKind::AutoFixApplied { step: "s".into() },
            "alice",
        );
        inbox.receive(&sig).unwrap();
        inbox.apply_all(&store).unwrap();

        let p = store.get("p1").unwrap();
        assert_eq!(p.evidence.override_signals, 1);
    }

    #[test]
    fn apply_failure_updates_global_only_not_contribution() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();

        let sig = signal(
            "p1",
            SignalKind::ExecutionFailure {
                error: "db down".into(),
            },
            "alice",
        );
        inbox.receive(&sig).unwrap();
        inbox.apply_all(&store).unwrap();

        let p = store.get("p1").unwrap();
        assert_eq!(p.evidence.failure_signals, 1);
        assert_eq!(p.evidence.success_signals, 0);
        assert_eq!(p.evidence.override_signals, 0);
        // Contribution entry is created for last_seen tracking but no success/override increments
        let contrib = p
            .evidence
            .contributions
            .get("Slack:alice")
            .expect("contrib entry");
        assert_eq!(contrib.success_signals, 0);
        assert_eq!(contrib.override_signals, 0);
    }

    #[test]
    fn apply_skips_signal_for_nonexistent_pattern() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        // Note: do NOT save any pattern

        let sig = signal("nonexistent", SignalKind::ExecutionSuccess, "alice");
        inbox.receive(&sig).unwrap();
        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.skipped, 1);
        assert_eq!(report.errors.len(), 0);
    }

    #[test]
    fn apply_creates_new_draft_pattern() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        let pat = make_pattern("draft-x");
        let sig = Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: Actor {
                source: ActorSource::Slack,
                native_id: "alice".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::NewDraftPattern {
                payload: Box::new(pat),
            },
            kind: SignalKind::NewPatternProposal {
                origin_context: "chat".into(),
            },
            scope: Scope::Personal,
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
        };
        inbox.receive(&sig).unwrap();
        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 1);
        assert_eq!(report.skipped, 0);
        // Pattern was created as draft
        assert!(store.exists("draft-x"));
    }

    #[test]
    fn apply_skips_new_draft_pattern_when_pattern_already_exists() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        // Pre-existing pattern with the same name
        store.save(&make_pattern("draft-x")).unwrap();
        let pat = make_pattern("draft-x");
        let sig = Signal {
            id: Uuid::new_v4(),
            emitted_at: Utc::now(),
            actor: Actor {
                source: ActorSource::Slack,
                native_id: "alice".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::NewDraftPattern {
                payload: Box::new(pat),
            },
            kind: SignalKind::NewPatternProposal {
                origin_context: "chat".into(),
            },
            scope: Scope::Personal,
            confidence: 1.0,
            schema_version: SIGNAL_SCHEMA_VERSION,
        };
        inbox.receive(&sig).unwrap();
        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.skipped, 1);
        assert_eq!(report.applied, 0);
    }

    #[test]
    fn apply_all_preserves_bad_yaml_in_place() {
        let tmp = tempdir().unwrap();
        let inbox_dir = tmp.path().join("inbox");
        let (store, inbox) = setup(tmp.path());
        // Write a bogus YAML file directly (drop store to avoid unused warning)
        let _ = store;
        std::fs::write(
            inbox_dir.join("2026-04-18T10-00-00-bad.yaml"),
            "not a signal",
        )
        .unwrap();
        let store2 = YamlStore::new(tmp.path().join("patterns")).unwrap();
        let report = inbox.apply_all(&store2).unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("parse error"));
        // File should NOT have been removed
        assert!(inbox_dir.join("2026-04-18T10-00-00-bad.yaml").exists());
    }

    #[test]
    fn two_actors_contributions_tracked_separately() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();

        inbox
            .receive(&signal("p1", SignalKind::ExecutionSuccess, "alice"))
            .unwrap();
        inbox
            .receive(&signal("p1", SignalKind::ExecutionSuccess, "alice"))
            .unwrap();
        inbox
            .receive(&signal(
                "p1",
                SignalKind::UserOverrideAtBreakpoint { reason: None },
                "bob",
            ))
            .unwrap();
        inbox.apply_all(&store).unwrap();

        let p = store.get("p1").unwrap();
        assert_eq!(p.evidence.success_signals, 2);
        assert_eq!(p.evidence.override_signals, 3);
        let alice = p.evidence.contributions.get("Slack:alice").unwrap();
        let bob = p.evidence.contributions.get("Slack:bob").unwrap();
        assert_eq!(alice.success_signals, 2);
        assert_eq!(bob.override_signals, 3);
    }

    #[test]
    fn duplicate_signal_id_is_not_double_counted() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();

        // First receive + apply
        let sig = signal("p1", SignalKind::ExecutionSuccess, "alice");
        inbox.receive(&sig).unwrap();
        inbox.apply_all(&store).unwrap();

        // Receive the SAME signal UUID again (retry scenario) with different filename
        // by writing directly with a different timestamp prefix
        let yaml = serde_yaml::to_string(&sig).unwrap();
        let dup_path = inbox.dir.join("2099-01-01T00-00-00-dup.yaml");
        std::fs::write(&dup_path, yaml).unwrap();
        let report = inbox.apply_all(&store).unwrap();

        // The duplicate should be skipped (not applied again)
        assert_eq!(report.applied, 0);
        assert_eq!(report.skipped, 1);

        let p = store.get("p1").unwrap();
        // Still only 1 success signal — not incremented twice
        assert_eq!(p.evidence.success_signals, 1);
    }
}
