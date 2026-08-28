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
}

impl RuntimeSkills {
    /// Fixed set, no reload source. Reloading one of these is an error rather
    /// than a silent no-op.
    pub fn build(loaded: Vec<LoadedSkill>) -> Self {
        Self {
            current: std::sync::RwLock::new(std::sync::Arc::new(SkillsSnapshot::new(loaded))),
            source: None,
        }
    }

    /// Attach the inputs that make this set reloadable from disk.
    pub fn reloadable(
        mut self,
        mur_home: impl Into<std::path::PathBuf>,
        agent: impl Into<String>,
        enabled: Box<dyn Fn(&str) -> bool + Send + Sync>,
    ) -> Self {
        self.source = Some(ReloadSource {
            mur_home: mur_home.into(),
            agent: agent.into(),
            enabled,
        });
        self
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
        let loaded = load_for_agent(&src.mur_home, &src.agent, src.enabled.as_ref());
        let n = loaded.len();
        *self.current.write().unwrap_or_else(|e| e.into_inner()) =
            std::sync::Arc::new(SkillsSnapshot::new(loaded));
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
