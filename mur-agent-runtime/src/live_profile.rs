//! Hot reload of the files a running agent reads per turn: its own
//! `profile.yaml` and the `limits:` block of `config.yaml`.
//!
//! Same idea as the skills fingerprint: each turn stats the file, and only a
//! changed stamp costs a re-read. No watcher thread, and no writer has to
//! remember to notify the runtime — a hand edit, the CLI, the Hub and a
//! `remind` grant all land the same way.
//!
//! Two rules make this safe to run unattended:
//!
//! - **A bad file never replaces a good one.** A parse error, a failed
//!   validation, or a profile that names a different agent leaves the last
//!   good snapshot serving; the error is kept for `doctor` and logged once per
//!   file stamp, not once per turn.
//! - **Only hot fields are applied.** The profile a reader sees is the boot
//!   profile with [`AgentProfile::with_hot_fields_over`] laid over it, so a
//!   change to entitlements, sandbox, identity or transport cannot reach a
//!   reader by any route; it is recorded as pending a cold restart instead.
//!   A cold MCP edit keeps the entry the last reload served, so it never
//!   rolls back a hot entry edit applied earlier.

use mur_common::AgentProfile;
use mur_common::agent::{RELOAD_STATE_FILE, ReloadState};
use mur_common::limits::Limits;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

#[path = "live_profile_mcp.rs"]
mod mcp;
#[path = "live_profile_stamp.rs"]
mod stamp;
pub use stamp::FileStamp;

type Loader<T> = Box<dyn Fn(&Path) -> Result<T, String> + Send + Sync>;

/// Version of what a [`LiveFile`] reads: the primary file, plus — only while
/// the primary is absent — its fallback. Once the primary exists the fallback
/// is not part of the version, so edits to it no longer trigger a reload.
pub type Stamp = (Option<FileStamp>, Option<FileStamp>);

struct State<T> {
    current: Arc<T>,
    /// Stamp `current` was loaded at.
    loaded_at: Stamp,
    /// Stamp of the last version that failed to load. Matching it means
    /// "already reported, still broken" — skip the re-read and the warning.
    failed_at: Option<Stamp>,
    generation: u64,
    last_error: Option<String>,
}

/// One file, reloaded on change, never replaced by a version that fails to load.
pub struct LiveFile<T> {
    path: PathBuf,
    /// Read instead of `path` while `path` does not exist (a legacy location).
    fallback: Option<PathBuf>,
    label: &'static str,
    state: RwLock<State<T>>,
    /// Serialises reloads so concurrent turns parse a changed file once.
    reload: Mutex<()>,
    load: Loader<T>,
    /// Called after every state change (success or failure).
    on_change: Option<OnChange>,
}

impl<T> LiveFile<T> {
    /// `initial` is what the process booted with; the file's current stamp is
    /// taken as its version, so the first turn does not reload it.
    pub fn new(
        path: impl Into<PathBuf>,
        label: &'static str,
        initial: T,
        load: impl Fn(&Path) -> Result<T, String> + Send + Sync + 'static,
    ) -> Self {
        let path = path.into();
        let loaded_at = stamp_of(&path, None);
        Self {
            path,
            fallback: None,
            label,
            state: RwLock::new(State {
                current: Arc::new(initial),
                loaded_at,
                failed_at: None,
                generation: 0,
                last_error: None,
            }),
            reload: Mutex::new(()),
            load: Box::new(load),
            on_change: None,
        }
    }

    /// Also watch `fallback` while the primary path is absent. The initial
    /// stamp is retaken so the first turn does not reload either file.
    pub fn with_fallback(mut self, fallback: impl Into<PathBuf>) -> Self {
        let fallback = fallback.into();
        let stamp = stamp_of(&self.path, Some(&fallback));
        self.fallback = Some(fallback);
        self.state
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .loaded_at = stamp;
        self
    }

    /// The version on disk now.
    pub fn stamp(&self) -> Stamp {
        stamp_of(&self.path, self.fallback.as_deref())
    }

    fn with_on_change(mut self, f: OnChange) -> Self {
        self.on_change = Some(f);
        self
    }

    /// The last version that loaded cleanly, re-read first if the file moved.
    /// Never fails: a bad file is reported and the previous version served.
    pub fn current(&self) -> Arc<T> {
        let disk = self.stamp();
        if !self.is_settled(disk) {
            self.reload_from(disk);
        }
        self.read().current.clone()
    }

    /// Bumped on every successful reload. Cheap; does not touch the disk.
    pub fn generation(&self) -> u64 {
        self.read().generation
    }

    /// Why the newest version on disk is not the one being served, if it isn't.
    pub fn last_error(&self) -> Option<String> {
        self.read().last_error.clone()
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, State<T>> {
        self.state.read().unwrap_or_else(|e| e.into_inner())
    }

    fn is_settled(&self, disk: Stamp) -> bool {
        let s = self.read();
        disk == s.loaded_at || s.failed_at == Some(disk)
    }

    fn reload_from(&self, disk: Stamp) {
        let _one = self.reload.lock().unwrap_or_else(|e| e.into_inner());
        // Another turn may have reloaded while this one waited for the lock.
        if self.is_settled(disk) {
            return;
        }
        let result = (self.load)(&self.path);
        {
            let mut s = self.state.write().unwrap_or_else(|e| e.into_inner());
            match result {
                Ok(v) => {
                    s.current = Arc::new(v);
                    s.loaded_at = disk;
                    s.failed_at = None;
                    s.generation += 1;
                    s.last_error = None;
                    tracing::info!(
                        file = %self.path.display(),
                        generation = s.generation,
                        "{} changed on disk; applied from the next turn",
                        self.label
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        file = %self.path.display(),
                        error = %e,
                        "{} changed on disk but did not load; still serving the previous version",
                        self.label
                    );
                    s.failed_at = Some(disk);
                    s.last_error = Some(e);
                }
            }
        }
        if let Some(f) = &self.on_change {
            f();
        }
    }
}

/// The agent's own `profile.yaml`, with only its hot fields live.
pub struct LiveProfile {
    file: LiveFile<AgentProfile>,
    pending_cold: Arc<Mutex<Vec<String>>>,
    /// Hot MCP entry edits the reload gate refused (see `live_profile_mcp`).
    mcp_refusals: Arc<Mutex<mcp::Refusals>>,
}

impl LiveProfile {
    /// `boot` is the profile the process started with — the base every reload
    /// overlays its hot fields onto, and the identity a reload must match.
    pub fn new(agent_home: &Path, boot: AgentProfile) -> Arc<Self> {
        Self::build(agent_home, boot, None)
    }

    fn build(agent_home: &Path, boot: AgentProfile, on_change: Option<OnChange>) -> Arc<Self> {
        let pending_cold = Arc::new(Mutex::new(Vec::new()));
        let home = agent_home.to_path_buf();
        let base = boot.clone();
        let pending = pending_cold.clone();
        let mcp_refusals = Arc::new(Mutex::new(mcp::Refusals::new()));
        let refusals = mcp_refusals.clone();
        // What the last good reload served, so a refused edit keeps that entry.
        let served = Mutex::new(boot.mcp_servers.clone());
        let permitted = mcp::seal_permits(&boot, agent_home);
        let load = move |_: &Path| -> Result<AgentProfile, String> {
            let fresh = crate::profile::Profile::load(&home)
                .map_err(|e| e.to_string())?
                .inner;
            if fresh.name != base.name || fresh.id != base.id {
                return Err(format!(
                    "profile.yaml now names agent `{}` ({}), not this one (`{}`, {}); \
                     the name and id cannot change under a running agent",
                    fresh.name, fresh.id, base.name, base.id
                ));
            }
            if let Some(l) = &fresh.limits {
                mur_common::limits::validate(l)?;
            }
            *pending.lock().unwrap_or_else(|e| e.into_inner()) =
                AgentProfile::reload_diff(&base, &fresh).cold;
            let mut served = served.lock().unwrap_or_else(|e| e.into_inner());
            let mut out = base.with_hot_fields_over(&served, &fresh);
            let (entries, refused) = mcp::gate_entries(
                &base,
                &served,
                std::mem::take(&mut out.mcp_servers),
                &permitted,
            );
            out.mcp_servers = entries;
            served.clone_from(&out.mcp_servers);
            *refusals.lock().unwrap_or_else(|e| e.into_inner()) = refused;
            Ok(out)
        };
        let mut file = LiveFile::new(agent_home.join("profile.yaml"), "profile.yaml", boot, load);
        file.on_change = on_change;
        Arc::new(LiveProfile {
            file,
            pending_cold,
            mcp_refusals,
        })
    }

    pub fn current(&self) -> Arc<AgentProfile> {
        self.file.current()
    }

    pub fn generation(&self) -> u64 {
        self.file.generation()
    }

    /// The MCP entry named `server` in the profile being served, for
    /// [`crate::mcp::pool::McpPool::attach_live`].
    pub fn mcp_entry(&self, server: &str) -> Option<mur_common::agent::McpServerEntry> {
        self.current()
            .mcp_servers
            .iter()
            .find(|e| e.name == server)
            .cloned()
    }

    pub fn state(&self) -> ReloadState {
        ReloadState {
            generation: self.file.generation(),
            last_reload_error: self.file.last_error(),
            config_limits_error: None,
            pending_cold_restart: self
                .pending_cold
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            mcp_refusals: self
                .mcp_refusals
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
        }
    }
}

type OnChange = Box<dyn Fn() + Send + Sync>;

fn stamp_of(path: &Path, fallback: Option<&Path>) -> Stamp {
    let primary = FileStamp::of(path);
    let secondary = match (primary, fallback) {
        (None, Some(f)) => FileStamp::of(f),
        _ => None,
    };
    (primary, secondary)
}

/// The global limits scope — the other scope a turn's bounds read (#1794).
/// Watches `<mur_home>/limits/global.yaml`, and while that file does not
/// exist yet also the legacy `config.yaml limits:` block, so a legacy user's
/// edit still reloads where the kernel lets the agent read it (macOS).
pub fn live_global_limits(mur_home: &Path, boot: Limits) -> LiveFile<Limits> {
    use mur_common::limits::{GlobalSource, global_limits_path, legacy_config_path};
    let home = mur_home.to_path_buf();
    LiveFile::new(
        global_limits_path(mur_home),
        "global limits",
        boot,
        move |_: &Path| {
            let g = mur_common::limits::load_global_strict(&home).map_err(|e| {
                if global_limits_path(&home).exists() {
                    e
                } else {
                    format!(
                        "{e} (legacy location; run `mur limits --global` once to move it to {})",
                        global_limits_path(&home).display()
                    )
                }
            })?;
            mur_common::limits::validate(&g.limits)?;
            if g.source == GlobalSource::Legacy {
                tracing::debug!("global limits read from legacy config.yaml");
            }
            Ok(g.limits)
        },
    )
    .with_fallback(legacy_config_path(mur_home))
}

/// What a turn reads once at its start and keeps for its whole length, so an
/// edit landing mid-turn cannot give one turn two different answers.
pub struct LiveSettings {
    pub profile: Arc<LiveProfile>,
    pub global_limits: LiveFile<Limits>,
}

impl LiveSettings {
    /// Wires both files to `agent_home/reload_state.json` and writes a fresh
    /// state at once, so `doctor` never reads a previous run's leftovers.
    pub fn new(
        agent_home: &Path,
        mur_home: &Path,
        boot: AgentProfile,
        boot_limits: Limits,
    ) -> Arc<Self> {
        let state_path = agent_home.join(RELOAD_STATE_FILE);
        let me = Arc::new_cyclic(|weak: &std::sync::Weak<LiveSettings>| {
            let hook = |w: std::sync::Weak<LiveSettings>, p: PathBuf| -> OnChange {
                Box::new(move || {
                    if let Some(me) = w.upgrade() {
                        me.write_state(&p);
                    }
                })
            };
            LiveSettings {
                profile: LiveProfile::build(
                    agent_home,
                    boot,
                    Some(hook(weak.clone(), state_path.clone())),
                ),
                global_limits: live_global_limits(mur_home, boot_limits)
                    .with_on_change(hook(weak.clone(), state_path.clone())),
            }
        });
        me.write_state(&state_path);
        me
    }

    pub fn state(&self) -> ReloadState {
        ReloadState {
            config_limits_error: self.global_limits.last_error(),
            ..self.profile.state()
        }
    }

    /// Best effort: `doctor` reads this; a failed write only costs visibility.
    fn write_state(&self, path: &Path) {
        if let Err(e) = self.state().save(path) {
            tracing::debug!(error = %e, "could not record reload state");
        }
    }
}

#[cfg(test)]
#[path = "live_profile_tests.rs"]
mod tests;
