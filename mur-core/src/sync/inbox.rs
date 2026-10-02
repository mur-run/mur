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
mod tests;
