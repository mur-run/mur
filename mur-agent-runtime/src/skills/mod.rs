pub mod injector;
pub mod sandbox_map;
pub mod trigger_matcher;

use mur_common::skill::loader::{LoadedSkill, SkillScope};
use trigger_matcher::RegisteredTrigger;

/// Drop notes the user has forgotten (`/forget` → `LifecycleState::Destroyed`).
///
/// Nothing in the runtime used to read lifecycle state, which was harmless only
/// because notes never reached a prompt at all. Now that they do, `/forget`'s
/// promise ("it will no longer be injected") needs an actual enforcement point.
/// Mirrors `mur-core`'s `/memories` listing: agent-scoped notes key their stats
/// under the agent home, global ones under the central store.
///
/// Startup-only cost: one small stats read per note, on the same snapshot the
/// rest of the skill set is built from.
pub fn drop_forgotten_notes(
    mur_home: &std::path::Path,
    agent: &str,
    loaded: Vec<LoadedSkill>,
) -> Vec<LoadedSkill> {
    use mur_common::skill::stats::SkillStats;
    loaded
        .into_iter()
        .filter(|s| {
            if mur_common::skill::lifecycle::note_kind(&s.manifest).is_none() {
                return true; // not a note — lifecycle gating is the note story
            }
            let path = match s.scope {
                SkillScope::Agent => SkillStats::path_agent(mur_home, agent, &s.name),
                SkillScope::Global => SkillStats::path(mur_home, &s.name),
            };
            SkillStats::load(&path).ok().flatten().is_none_or(|st| {
                st.lifecycle_state != mur_common::skill::stats::LifecycleState::Destroyed
            })
        })
        .collect()
}

/// The whole skill pipeline in one place: load, drop forgotten notes, apply the
/// profile's denylist. Startup and reload MUST go through this — two copies of
/// a load pipeline is how the two halves of a fact start disagreeing.
pub fn load_for_agent(
    mur_home: &std::path::Path,
    agent: &str,
    enabled: &(dyn Fn(&str) -> bool + Send + Sync),
) -> Vec<LoadedSkill> {
    drop_forgotten_notes(
        mur_home,
        agent,
        mur_common::skill::loader::load_all(mur_home, agent),
    )
    .into_iter()
    .filter(|s| enabled(&s.name))
    .collect()
}

/// Fingerprint of everything [`load_for_agent`] reads, cheap enough to take on
/// every turn.
///
/// Derived from disk rather than bumped by writers, deliberately. A
/// bump-on-write counter needs every mutation path to remember to bump it —
/// `mur skill install`, `mur skill remove`, `mur sync`, the Hub, and whatever
/// is added next — and a forgotten bump is indistinguishable from the staleness
/// it was meant to close. Reading the tree has no write side to forget: a
/// `skill.yaml` edited by hand counts, an agent that somehow drifts is back in
/// sync on its next turn, and there is no read-modify-write to race.
///
/// Measured at ~170µs over 67 skills with a warm cache, on a turn that then
/// spends seconds inside an LLM call. `assemble_system_prompt` already touches
/// the filesystem once per turn (`active_project_id` walks up for the repo
/// root), so this is the same class of cost rather than a new one.
///
/// Process-local by design: `DefaultHasher` promises nothing across versions
/// and needs to, since a restart re-reads the tree from scratch anyway.
fn skills_fingerprint(mur_home: &std::path::Path, agent: &str) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    /// `(len, mtime-nanos)`, or `None` when the file is absent or unreadable —
    /// itself a state worth noticing, since it is how a deletion shows up.
    fn stamp(p: &std::path::Path) -> Option<(u64, u128)> {
        let md = std::fs::metadata(p).ok()?;
        let m = md
            .modified()
            .ok()?
            .duration_since(std::time::UNIX_EPOCH)
            .ok()?
            .as_nanos();
        Some((md.len(), m))
    }

    let agent_home = mur_home.join("agents").join(agent);
    let dirs = [
        mur_common::skill::store::agent_skill_dir(mur_home, agent),
        agent_home.join("knowledge_cache"),
        mur_home.join("skills"),
    ];

    let mut acc: u64 = 0;
    for dir in &dirs {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue; // absent dir is a legitimate state, not an error
        };
        for e in entries.flatten() {
            let p = e.path();
            let mut h = DefaultHasher::new();
            e.file_name().hash(&mut h);
            stamp(&p.join("skill.yaml")).hash(&mut h);
            // stats.json carries the lifecycle state. A `/forget` in another
            // process touches nothing else in the tree, so without this the
            // one mutation the CLI can make to a live agent's memory would be
            // the one this cannot see.
            stamp(&p.join("stats.json")).hash(&mut h);
            // Summed, not chained: `read_dir` order is not guaranteed, and a
            // reordering must not read as a change.
            acc = acc.wrapping_add(h.finish());
        }
    }

    let mut h = DefaultHasher::new();
    acc.hash(&mut h);
    // Trust level orders the injected list and gates loading, so a
    // `mur skill trust` is a change too — one more stat.
    stamp(&mur_common::trust::skills::SkillTrustStore::path(mur_home)).hash(&mut h);
    h.finish()
}

/// One immutable view of the skill set. Readers clone the `Arc` and hold no
/// lock — `assemble_system_prompt` runs inside an async task, and a guard held
/// across an `.await` is a deadlock waiting to happen.
pub struct SkillsSnapshot {
    pub loaded: Vec<LoadedSkill>,
    pub triggers: Vec<RegisteredTrigger>,
}

impl SkillsSnapshot {
    fn new(loaded: Vec<LoadedSkill>) -> Self {
        let triggers = trigger_matcher::register_from(&loaded);
        Self { loaded, triggers }
    }
}

/// Everything [`RuntimeSkills::reload`] needs to rebuild the set from disk.
/// Absent in tests, which build a fixed set and never reload.
struct ReloadSource {
    mur_home: std::path::PathBuf,
    agent: String,
    enabled: Box<dyn Fn(&str) -> bool + Send + Sync>,
}

/// The live skill set.
///
/// Used to be a plain `Vec` built once at boot and never rebuilt, which made
/// the `remember` tool's own promise ("effective next turn") false: the note
/// reached the disk and the process kept serving the boot-time snapshot until
/// a restart. One reload mechanism now serves both triggers — the in-process
/// `remember` tool and the out-of-process `memory/reload` A2A method that
/// murmur's `/remember` and `/forget` dial. Deliberately not a filesystem
/// watcher: a new dependency and a thread to answer a question two callers
/// already know the answer to.
pub struct RuntimeSkills {
    current: std::sync::RwLock<std::sync::Arc<SkillsSnapshot>>,
    source: Option<ReloadSource>,
    /// Fingerprint the current snapshot was loaded at.
    fingerprint: std::sync::atomic::AtomicU64,
}

impl RuntimeSkills {
    /// Fixed set, no reload source. Reloading one of these is an error rather
    /// than a silent no-op.
    pub fn build(loaded: Vec<LoadedSkill>) -> Self {
        Self {
            current: std::sync::RwLock::new(std::sync::Arc::new(SkillsSnapshot::new(loaded))),
            source: None,
            fingerprint: std::sync::atomic::AtomicU64::new(0),
        }
    }

    /// Attach the inputs that make this set reloadable from disk.
    pub fn reloadable(
        mut self,
        mur_home: impl Into<std::path::PathBuf>,
        agent: impl Into<String>,
        enabled: Box<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Self {
        let src = ReloadSource {
            mur_home: mur_home.into(),
            agent: agent.into(),
            enabled,
        };
        // Seed it, so the first turn does not reload a set that was just built.
        self.fingerprint.store(
            skills_fingerprint(&src.mur_home, &src.agent),
            std::sync::atomic::Ordering::Relaxed,
        );
        self.source = Some(src);
        self
    }

    /// Reload when the on-disk tree no longer matches the loaded snapshot.
    ///
    /// The third trigger on the one reload mechanism — the others being the
    /// in-process `remember` tool and the `memory/reload` A2A method — and the
    /// only one that needs no cooperation from whoever changed the files. It is
    /// what makes `mur skill remove` reach a running agent without a fan-out
    /// that would have to dial every agent, report partial failure honestly,
    /// and still miss every agent started afterwards.
    ///
    /// Never fails a turn: a failed reload leaves the previous snapshot
    /// serving and says so in the log.
    pub fn refresh_if_changed(&self) {
        let Some(src) = &self.source else {
            return; // fixed set (tests) — nothing to compare against
        };
        let disk = skills_fingerprint(&src.mur_home, &src.agent);
        if disk == self.fingerprint.load(std::sync::atomic::Ordering::Relaxed) {
            return;
        }
        match self.reload() {
            Ok(n) => tracing::info!(skills = n, "skill tree changed on disk; reloaded"),
            Err(e) => tracing::warn!(
                error = %e,
                "skill tree changed on disk but the reload failed; serving the previous set"
            ),
        }
    }

    /// Current view. Cheap: one `Arc` clone.
    pub fn snapshot(&self) -> std::sync::Arc<SkillsSnapshot> {
        self.current
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Rebuild from disk. Returns how many skills the new set holds.
    pub fn reload(&self) -> anyhow::Result<usize> {
        let src = self
            .source
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("this skill set was built without a reload source"))?;
        // Fingerprint BEFORE loading. If the tree changes while this load is in
        // flight, the value stored is the older one, so the next turn reloads
        // again instead of the change being swallowed.
        let fp = skills_fingerprint(&src.mur_home, &src.agent);
        let loaded = load_for_agent(&src.mur_home, &src.agent, src.enabled.as_ref());
        let n = loaded.len();
        *self.current.write().unwrap_or_else(|e| e.into_inner()) =
            std::sync::Arc::new(SkillsSnapshot::new(loaded));
        self.fingerprint
            .store(fp, std::sync::atomic::Ordering::Relaxed);
        Ok(n)
    }
}

#[cfg(test)]
mod reload_tests {
    use super::*;
    use mur_common::skill::note::{NoteSpec, note_manifest};
    use mur_common::skill::stats::{LifecycleState, SkillStats};

    fn write_note(home: &std::path::Path, name: &str, body: &str) {
        let dir = mur_common::skill::store::agent_skill_dir(home, "a1").join(name);
        std::fs::create_dir_all(&dir).unwrap();
        let m = note_manifest(&NoteSpec {
            name,
            description: "d",
            body,
            kind: mur_common::skill::lifecycle::NoteKind::Rule,
            publisher: "agent:a1",
        });
        mur_common::skill::store::write_to_dir(&dir, &m).unwrap();
        std::fs::write(
            SkillStats::path_agent(home, "a1", name),
            serde_json::to_string(&SkillStats::new(name, "1.0.0", "", chrono::Utc::now())).unwrap(),
        )
        .unwrap();
    }

    fn reloadable(home: &std::path::Path) -> RuntimeSkills {
        let loaded = load_for_agent(home, "a1", &|_| true);
        RuntimeSkills::build(loaded).reloadable(home, "a1", Box::new(|_| true))
    }

    /// The whole point of #4: a memory written to disk AFTER boot is visible to
    /// the next prompt without a restart.
    #[test]
    fn reload_picks_up_a_memory_written_after_boot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        write_note(home, "first", "one");
        let skills = reloadable(home);
        assert_eq!(skills.snapshot().loaded.len(), 1);

        write_note(home, "second", "two");
        assert_eq!(
            skills.snapshot().loaded.len(),
            1,
            "the old snapshot must not change under the reader's feet"
        );
        assert_eq!(skills.reload().unwrap(), 2);
        let snap = skills.snapshot();
        let names: Vec<&str> = snap.loaded.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"second"), "{names:?}");
    }

    /// `/forget` writes Destroyed from another process; a reload must honour it.
    #[test]
    fn reload_drops_a_memory_forgotten_after_boot() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        write_note(home, "gone", "x");
        let skills = reloadable(home);
        assert_eq!(skills.snapshot().loaded.len(), 1);

        let path = SkillStats::path_agent(home, "a1", "gone");
        let mut st = SkillStats::load(&path).unwrap().unwrap();
        st.lifecycle_state = LifecycleState::Destroyed;
        std::fs::write(&path, serde_json::to_string(&st).unwrap()).unwrap();

        assert_eq!(skills.reload().unwrap(), 0);
        assert!(skills.snapshot().loaded.is_empty());
    }

    /// The point of the whole design: a note removed by ANOTHER process — no
    /// dial, no bump, nothing cooperating — reaches the agent on its next turn.
    #[test]
    fn a_note_removed_by_another_process_is_dropped_on_the_next_turn() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        write_note(home, "keep", "one");
        write_note(home, "doomed", "two");
        let skills = reloadable(home);
        assert_eq!(skills.snapshot().loaded.len(), 2);

        // Exactly what `mur skill remove` does, from outside this process.
        std::fs::remove_dir_all(
            mur_common::skill::store::agent_skill_dir(home, "a1").join("doomed"),
        )
        .unwrap();

        skills.refresh_if_changed();
        let snap = skills.snapshot();
        let names: Vec<&str> = snap.loaded.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["keep"], "removed note must be gone");
    }

    /// A note whose body is edited in place — no entry added or removed, so a
    /// directory mtime alone would miss it.
    #[test]
    fn an_edited_note_body_is_picked_up() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        write_note(home, "rule", "reply in English");
        let skills = reloadable(home);
        write_note(home, "rule", "reply in zh-TW");

        skills.refresh_if_changed();
        let snap = skills.snapshot();
        assert_eq!(
            snap.loaded[0].manifest.content.note.as_deref(),
            Some("reply in zh-TW")
        );
    }

    /// `/forget` in the CLI process writes only stats.json. Nothing else in the
    /// tree moves, so this is the case a coarser fingerprint would miss.
    #[test]
    fn a_forget_from_another_process_is_picked_up() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        write_note(home, "gone", "x");
        let skills = reloadable(home);
        assert_eq!(skills.snapshot().loaded.len(), 1);

        let path = SkillStats::path_agent(home, "a1", "gone");
        let mut st = SkillStats::load(&path).unwrap().unwrap();
        st.lifecycle_state = LifecycleState::Destroyed;
        std::fs::write(&path, serde_json::to_string(&st).unwrap()).unwrap();

        skills.refresh_if_changed();
        assert!(skills.snapshot().loaded.is_empty());
    }

    /// The cost guarantee: an unchanged tree must not rebuild anything. Proven
    /// by pointer identity — a reload would hand back a different `Arc`.
    #[test]
    fn an_unchanged_tree_does_not_reload() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        write_note(home, "steady", "x");
        let skills = reloadable(home);

        let before = skills.snapshot();
        for _ in 0..5 {
            skills.refresh_if_changed();
        }
        assert!(
            std::sync::Arc::ptr_eq(&before, &skills.snapshot()),
            "an unchanged tree must not rebuild the snapshot"
        );
    }

    /// A fixed set has nothing to compare against; refreshing must be a quiet
    /// no-op rather than an error or a panic.
    #[test]
    fn refresh_on_a_fixed_set_is_a_noop() {
        let skills = RuntimeSkills::build(vec![]);
        skills.refresh_if_changed();
        assert!(skills.snapshot().loaded.is_empty());
    }

    /// A set built without a source is honest about it rather than silently
    /// pretending the reload happened.
    #[test]
    fn reload_without_a_source_is_an_error() {
        assert!(RuntimeSkills::build(vec![]).reload().is_err());
    }
}

#[cfg(test)]
mod forgotten_tests {
    use super::*;
    use mur_common::skill::note::{NoteSpec, note_manifest};
    use mur_common::skill::stats::{LifecycleState, SkillStats};
    use mur_common::skill::types::TrustLevel;

    fn note(name: &str) -> LoadedSkill {
        LoadedSkill {
            name: name.into(),
            manifest: note_manifest(&NoteSpec {
                name,
                description: "d",
                body: "b",
                kind: mur_common::skill::lifecycle::NoteKind::Rule,
                publisher: "agent:a1",
            }),
            trust: TrustLevel::Sandboxed,
            scope: SkillScope::Agent,
            content_hash: String::new(),
            dir: std::path::PathBuf::new(),
        }
    }

    /// `/forget` sets `Destroyed`; a forgotten note must not survive into the
    /// skill snapshot the prompt is built from. Live note is the control.
    #[test]
    fn forgotten_note_is_dropped_live_note_survives() {
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path();
        let dir = mur_common::skill::store::agent_skill_dir(home, "a1");
        std::fs::create_dir_all(dir.join("gone")).unwrap();
        std::fs::create_dir_all(dir.join("kept")).unwrap();

        for (name, state) in [
            ("gone", LifecycleState::Destroyed),
            ("kept", LifecycleState::Draft),
        ] {
            let mut st = SkillStats::new(name, "1.0.0", "", chrono::Utc::now());
            st.lifecycle_state = state;
            std::fs::write(
                SkillStats::path_agent(home, "a1", name),
                serde_json::to_string(&st).unwrap(),
            )
            .unwrap();
        }

        let kept = drop_forgotten_notes(home, "a1", vec![note("gone"), note("kept")]);
        let names: Vec<&str> = kept.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["kept"], "only the forgotten note is dropped");
    }

    /// A note with no stats file at all (never written, or a global note) is
    /// live — absence must not read as Destroyed.
    #[test]
    fn note_without_stats_is_kept() {
        let tmp = tempfile::TempDir::new().unwrap();
        let kept = drop_forgotten_notes(tmp.path(), "a1", vec![note("fresh")]);
        assert_eq!(kept.len(), 1);
    }
}
