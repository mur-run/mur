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

/// Subdirectory of the inbox holding signals received over the authenticated
/// commander wire (bearer-token HTTP). See [`Inbox::receive_wire`].
pub const WIRE_SUBDIR: &str = "wire";

/// Canonical inbox file name for a signal (shared by the local and wire drops).
pub fn signal_file_name(signal: &Signal) -> String {
    format!(
        "{}-{}.yaml",
        signal.emitted_at.format("%Y-%m-%dT%H-%M-%S"),
        signal.id
    )
}

/// Receive side of the sync protocol — reads Signal YAML files and applies
/// Evidence updates to patterns via [`YamlStore`].
pub struct Inbox {
    dir: PathBuf,
    mur_home: PathBuf,
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
    /// Derives `mur_home` as the parent of `dir` (e.g. `~/.mur/inbox` → `~/.mur`).
    pub fn new(dir: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        let mur_home = dir
            .parent()
            .ok_or_else(|| anyhow::anyhow!("inbox dir has no parent"))?
            .to_path_buf();
        Ok(Self { dir, mur_home })
    }

    /// Open an inbox at `dir`, using the given `mur_home` for skill resolution
    /// (rather than deriving it from `dir.parent()`).
    pub fn new_with_mur_home(dir: impl AsRef<Path>, mur_home: impl AsRef<Path>) -> Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&dir)?;
        Ok(Self {
            dir,
            mur_home: mur_home.as_ref().to_path_buf(),
        })
    }

    /// Open the default inbox at `$HOME/.mur/inbox/`.
    pub fn default_location() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("no HOME"))?;
        Self::new(home.join(".mur/inbox"))
    }

    /// Persist a signal received from the server into the inbox (used by the
    /// fetcher before `apply_all`).
    pub fn receive(&self, signal: &Signal) -> Result<PathBuf> {
        Self::receive_into(&self.dir, signal)
    }

    /// Like [`Inbox::receive`], but into the `wire/` subdirectory — the
    /// provenance marker for the token-authed commander wire (frozen v1:
    /// signals arrive bearer-authed but unsigned). `apply_all` exempts this
    /// subdirectory from `MUR_SIGNAL_REQUIRE_SIG`; a PRESENT signature is
    /// still verified fail-closed. The exemption stands until the wire grows
    /// operator-signed batches (the governance key is already pinnable via
    /// `mur commander pin`).
    pub fn receive_wire(&self, signal: &Signal) -> Result<PathBuf> {
        let dir = self.wire_dir();
        std::fs::create_dir_all(&dir)?;
        Self::receive_into(&dir, signal)
    }

    /// The `wire/` subdirectory (commander-wire provenance; see
    /// [`Inbox::receive_wire`]).
    pub fn wire_dir(&self) -> PathBuf {
        self.dir.join(WIRE_SUBDIR)
    }

    fn receive_into(dir: &Path, signal: &Signal) -> Result<PathBuf> {
        let name = signal_file_name(signal);
        let path = dir.join(&name);
        let tmp = dir.join(format!(".{}.tmp", name));
        let yaml = serde_yaml::to_string(signal)
            .with_context(|| format!("serialize signal {}", signal.id))?;
        std::fs::write(&tmp, yaml)?;
        std::fs::rename(&tmp, &path)?;
        Ok(path)
    }

    /// Every pending inbox file paired with the signature requirement its
    /// provenance carries: locally-dropped files take the caller's `require`,
    /// `wire/` files never require (token-authed wire — see
    /// [`Inbox::receive_wire`]).
    fn scan(&self, require_sig: bool) -> Result<Vec<(PathBuf, bool)>> {
        let mut files: Vec<(PathBuf, bool)> = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let p = entry?.path();
            if is_inbox_yaml(&p) {
                files.push((p, require_sig));
            }
        }
        if let Ok(entries) = std::fs::read_dir(self.wire_dir()) {
            for entry in entries {
                let p = entry?.path();
                if is_inbox_yaml(&p) {
                    files.push((p, false));
                }
            }
        }
        Ok(files)
    }

    /// P2c-2 ingest gate — the signature proves *who said it*, the scope
    /// check proves *they may say it there*. Unsigned signals pass unless
    /// `require` (legacy drops + the commander wire are unsigned); a PRESENT
    /// signature is always checked fail-closed: it must verify against the
    /// claimed actor's on-disk pubkey (`agents/<actor>/identity.pub`) and the
    /// self-reported scope must be one an agent identity may claim (Personal —
    /// agents have no team/community authority).
    fn check_signal_sig(&self, signal: &Signal, require: bool) -> Result<(), String> {
        if signal.sig.is_none() {
            return if require {
                Err("unsigned signal rejected (MUR_SIGNAL_REQUIRE_SIG)".into())
            } else {
                Ok(())
            };
        }
        let actor = &signal.actor.native_id;
        // The actor name is joined into a path below — allow exactly the
        // charset agent dirs use, or the join is a traversal primitive (same
        // guard the daemon applies to snapshot requests).
        if !valid_agent_name(actor) {
            return Err(format!(
                "signed signal actor '{actor}' is not a valid agent name"
            ));
        }
        let dir = self.mur_home.join("agents").join(actor);
        let pubkey = mur_common::identity::AgentIdentity::load_pubkey(&dir)
            .map_err(|e| format!("no verifiable identity for actor '{actor}': {e}"))?;
        if !signal.verify(&pubkey) {
            return Err(format!("signature verification failed for actor '{actor}'"));
        }
        if signal.scope != mur_common::Scope::Personal {
            return Err(format!(
                "agent '{actor}' may not emit {:?}-scoped signals",
                signal.scope
            ));
        }
        Ok(())
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
        let require_sig = mur_common::signal::require_sig_from_env();
        let mut seen = self.load_seen_ids();
        let mut newly_seen: Vec<Uuid> = Vec::new();

        for (p, require) in self.scan(require_sig)? {
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

            // Signature gate (P2c-2). Rejected files are REMOVED (a bad
            // signature is permanent — retrying can't fix it) but NOT marked
            // seen, so a correctly-signed re-emission of the same id may
            // still apply later.
            if let Err(reason) = self.check_signal_sig(&signal, require) {
                report.errors.push(format!("{}: {reason}", p.display()));
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
                    // Skill signals are handled by apply_skill_signals.
                    SignalKind::SkillExecutionSuccess
                    | SignalKind::SkillExecutionFailure { .. }
                    | SignalKind::NewDraftSkill { .. } => return Ok(false),
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
            SignalTarget::NewDraftSkill { payload } => {
                // Reject a path-traversal name from a remote peer before it
                // touches the filesystem: the name is joined into
                // `<mur_home>/skills/<name>`, so an unvalidated value like
                // `../agents/<other>/skills/x` would let a peer plant an
                // instruction-bearing skill into another agent's context.
                if !mur_common::skill::is_valid_skill_name(&payload.name) {
                    tracing::warn!(
                        name = %payload.name,
                        "rejecting synced NewDraftSkill: invalid skill name"
                    );
                    return Ok(false);
                }
                // Never overwrite a skill the user may have edited
                use mur_common::skill::global_skill_dir;
                let skill_dir = global_skill_dir(&self.mur_home, &payload.name);
                if skill_dir.join("skill.yaml").exists() {
                    return Ok(false);
                }
                std::fs::create_dir_all(&skill_dir)?;
                mur_common::skill::write_to_dir(&skill_dir, payload)?;
                Ok(true)
            }
            // Skill-targeted signals are handled by apply_skill_signals.
            SignalTarget::Skill { .. } => Ok(false),
        }
    }

    /// Apply all skill-targeted signals in the inbox. Mutates `events.jsonl`
    /// and `stats.json` for each target skill.
    pub fn apply_skill_signals(&self) -> Result<ApplyReport> {
        use mur_common::{Signal, SignalTarget};

        let mut report = ApplyReport::default();
        let require_sig = mur_common::signal::require_sig_from_env();
        let mut seen = self.load_seen_ids();
        let mut newly_seen: Vec<uuid::Uuid> = Vec::new();

        for (p, require) in self.scan(require_sig)? {
            let Ok(text) = std::fs::read_to_string(&p) else {
                continue;
            };
            let Ok(signal) = serde_yaml::from_str::<Signal>(&text) else {
                report.errors.push(format!("parse error: {}", p.display()));
                continue;
            };
            if seen.contains(&signal.id) {
                let _ = std::fs::remove_file(&p);
                report.skipped += 1;
                continue;
            }
            let target_is_skill = matches!(signal.target, SignalTarget::Skill { .. });
            if !target_is_skill {
                continue; // handled by apply_all (pattern branch)
            }
            // Signature gate (P2c-2) — same rules as apply_all.
            if let Err(reason) = self.check_signal_sig(&signal, require) {
                report.errors.push(format!("{}: {reason}", p.display()));
                let _ = std::fs::remove_file(&p);
                continue;
            }
            match self.apply_skill_one(&signal) {
                Ok(true) => {
                    report.applied += 1;
                    newly_seen.push(signal.id);
                    let _ = std::fs::remove_file(&p);
                }
                Ok(false) => {
                    report.skipped += 1;
                    let _ = std::fs::remove_file(&p);
                }
                Err(e) => {
                    report.errors.push(format!("{}: {e}", p.display()));
                }
            }
        }
        seen.extend(newly_seen.iter().copied());
        self.save_seen_ids(&seen)?;
        Ok(report)
    }

    fn apply_skill_one(&self, signal: &mur_common::Signal) -> Result<bool> {
        use mur_common::skill::event_log::{SkillEvent, append_event, event_log_path};
        use mur_common::skill::stats::SkillStats;
        use mur_common::{SignalKind, SignalTarget};

        let SignalTarget::Skill { name, .. } = &signal.target else {
            return Ok(false);
        };
        // Skill must be installed locally; skip if not.
        let skill_dir = self.mur_home.join("skills").join(name);
        if !skill_dir.join("skill.yaml").exists() {
            return Ok(false);
        }
        let event = match &signal.kind {
            SignalKind::SkillExecutionSuccess => SkillEvent::Execution {
                ts: signal.emitted_at,
                device_id: "remote".into(),
                outcome: "success".into(),
                error: None,
                step: None,
                duration_ms: None,
                exit_code: None,
                env_class: None,
                confidence: None,
                trigger: None,
            },
            SignalKind::SkillExecutionFailure { error } => SkillEvent::Execution {
                ts: signal.emitted_at,
                device_id: "remote".into(),
                outcome: "failure".into(),
                error: Some(error.clone()),
                step: None,
                duration_ms: None,
                exit_code: None,
                env_class: None,
                confidence: None,
                trigger: None,
            },
            _ => return Ok(false),
        };
        let events_path = event_log_path(&self.mur_home, name);
        append_event(&events_path, &event)?;
        let stats_path = SkillStats::path(&self.mur_home, name);
        SkillStats::merge_in_place(
            &stats_path,
            || SkillStats::new(name, "unknown", "", chrono::Utc::now()),
            |s| {
                mur_common::skill::event_log::apply_new_events_to_stats(
                    s,
                    std::slice::from_ref(&event),
                );
                Ok(())
            },
        )?;
        Ok(true)
    }
}

/// Exactly the charset agent directory names use (see the daemon's snapshot-
/// request guard) — anything else would make `agents/<name>` a traversal.
fn valid_agent_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
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
            sig: None,
            key_version: 0,
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
            sig: None,
            key_version: 0,
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
            sig: None,
            key_version: 0,
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

    #[test]
    fn skill_execution_signal_appends_event_and_updates_stats() {
        use mur_common::skill::event_log::read_events;
        use mur_common::{
            Actor, ActorSource, SIGNAL_SCHEMA_VERSION, Scope, SignalKind, SignalTarget,
        };
        use uuid::Uuid;

        let dir = tempdir().unwrap();
        // Stub skill.yaml so the skill exists
        let skill_dir = dir.path().join("skills/test-skill");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("skill.yaml"),
            "name: test-skill\nversion: 1.0.0\n",
        )
        .unwrap();

        let inbox_dir = dir.path().join("inbox");
        let inbox = Inbox::new_with_mur_home(&inbox_dir, dir.path()).unwrap();

        let signal = mur_common::Signal {
            id: Uuid::new_v4(),
            schema_version: SIGNAL_SCHEMA_VERSION,
            emitted_at: chrono::Utc::now(),
            actor: Actor {
                source: ActorSource::CommanderDaemon,
                native_id: "a".into(),
                display_name: None,
                resolved_user_id: None,
            },
            target: SignalTarget::Skill {
                name: "test-skill".into(),
                scope: Scope::Personal,
            },
            kind: SignalKind::SkillExecutionSuccess,
            scope: Scope::Personal,
            confidence: 1.0,
            sig: None,
            key_version: 0,
        };
        inbox.receive(&signal).unwrap();
        let report = inbox.apply_skill_signals().unwrap();
        assert_eq!(report.applied, 1);

        let events = read_events(&dir.path().join("skills/test-skill/events.jsonl")).unwrap();
        assert_eq!(events.len(), 1);
        assert!(matches!(
            events[0],
            mur_common::skill::event_log::SkillEvent::Execution { ref outcome, .. }
            if outcome == "success"
        ));
    }

    // ── P2c-2 signature gate ─────────────────────────────────────────────

    use mur_common::identity::AgentIdentity;

    /// Register agent `name` under `<home>/agents/` with a real keypair.
    fn agent_fixture(home: &Path, name: &str) -> AgentIdentity {
        let dir = home.join("agents").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let id = AgentIdentity::generate();
        id.save(&dir).unwrap();
        id
    }

    #[test]
    fn signed_signal_verifies_and_applies() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();
        let id = agent_fixture(tmp.path(), "w1");

        let mut sig = signal("p1", SignalKind::ExecutionSuccess, "w1");
        sig.sign(&id);
        inbox.receive(&sig).unwrap();
        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 1, "errors: {:?}", report.errors);
        assert!(report.errors.is_empty());
    }

    #[test]
    fn tampered_signed_signal_is_rejected_and_removed() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();
        let id = agent_fixture(tmp.path(), "w1");

        let mut sig = signal("p1", SignalKind::ExecutionSuccess, "w1");
        sig.sign(&id);
        sig.confidence = 0.1; // tamper AFTER signing
        inbox.receive(&sig).unwrap();

        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("verification failed"));
        // File removed (permanent failure — no poison retry), pattern untouched.
        assert_eq!(
            std::fs::read_dir(tmp.path().join("inbox"))
                .unwrap()
                .filter(|e| is_inbox_yaml(&e.as_ref().unwrap().path()))
                .count(),
            0
        );
        assert_eq!(store.get("p1").unwrap().evidence.success_signals, 0);
    }

    #[test]
    fn signed_signal_with_non_personal_scope_is_rejected() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();
        let id = agent_fixture(tmp.path(), "w1");

        let mut sig = signal("p1", SignalKind::ExecutionSuccess, "w1");
        sig.scope = Scope::Team {
            team_id: "ops".into(),
        };
        sig.sign(&id); // signature VALID — the scope claim itself is the violation
        inbox.receive(&sig).unwrap();

        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("may not emit"));
    }

    #[test]
    fn signed_signal_without_registered_identity_is_rejected() {
        let tmp = tempdir().unwrap();
        let (store, inbox) = setup(tmp.path());
        store.save(&make_pattern("p1")).unwrap();
        // NO agent_fixture: sign with a key the central store has never seen.
        let rogue = AgentIdentity::generate();

        let mut sig = signal("p1", SignalKind::ExecutionSuccess, "ghost");
        sig.sign(&rogue);
        inbox.receive(&sig).unwrap();

        let report = inbox.apply_all(&store).unwrap();
        assert_eq!(report.applied, 0);
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].contains("no verifiable identity"));
    }

    #[test]
    fn wire_drops_are_exempt_from_require_sig() {
        let tmp = tempdir().unwrap();
        let (_store, inbox) = setup(tmp.path());
        let local = signal("p1", SignalKind::ExecutionSuccess, "w1");
        let wire = signal("p2", SignalKind::ExecutionSuccess, "cmdr");
        inbox.receive(&local).unwrap();
        inbox.receive_wire(&wire).unwrap();

        // The wire drop lands under wire/, not the main dir.
        assert!(inbox.wire_dir().join(signal_file_name(&wire)).exists());

        let pairs = inbox.scan(true).unwrap();
        assert_eq!(pairs.len(), 2);
        let require_for = |id: &uuid::Uuid| {
            pairs
                .iter()
                .find(|(p, _)| p.to_string_lossy().contains(&id.to_string()))
                .unwrap()
                .1
        };
        assert!(
            require_for(&local.id),
            "local drop must require a signature"
        );
        assert!(!require_for(&wire.id), "wire drop must be exempt");
    }

    #[test]
    fn require_mode_rejects_unsigned_and_traversal_actor_names() {
        let tmp = tempdir().unwrap();
        let (_store, inbox) = setup(tmp.path());

        // Unsigned tolerated by default, rejected under require.
        let unsigned = signal("p1", SignalKind::ExecutionSuccess, "w1");
        assert!(inbox.check_signal_sig(&unsigned, false).is_ok());
        assert!(inbox.check_signal_sig(&unsigned, true).is_err());

        // A signed signal claiming a path-traversal actor never reaches the join.
        let id = AgentIdentity::generate();
        let mut evil = signal("p1", SignalKind::ExecutionSuccess, "../w1");
        evil.sign(&id);
        let err = inbox.check_signal_sig(&evil, false).unwrap_err();
        assert!(err.contains("not a valid agent name"));
    }
}
