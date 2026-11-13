//! The profile-backed [`LiveOps`] for `/browser live`: reads and writes the
//! agent's `profile.yaml`, re-pins the live entry, and backfills its grants.
//! Moved out of `browser_live_cmd.rs` verbatim.

use anyhow::Context;
use mur_common::agent::{FilesystemEntitlement, McpServerEntry, McpServerNetwork};

use super::browser_live_cmd::*;
use super::browser_live_effect::Effect;
use super::manage;

/// Real profile-backed [`LiveOps`].
pub(crate) struct ProfileOps<'a>(pub &'a str);

impl LiveOps for ProfileOps<'_> {
    fn record_research(&mut self, _agent: &str) -> anyhow::Result<()> {
        self.record_research_event()
    }
    fn record_profile_bound(
        &mut self,
        agent: &str,
        old: Option<&str>,
        new: &str,
    ) -> anyhow::Result<()> {
        let home = crate::cmd::agent::resolve_mur_home()?;
        crate::cmd::agent::mcp_network::record_profile_bound(
            &home,
            agent,
            LIVE_ENTRY,
            old,
            new,
            chrono::Utc::now(),
        )
    }
    fn servers(&mut self) -> anyhow::Result<Vec<McpServerEntry>> {
        Ok(crate::cmd::agent::load_profile_for_edit(self.0)?
            .1
            .mcp_servers)
    }
    fn mcp_add(&mut self, argv: &[String], network: McpServerNetwork) -> anyhow::Result<String> {
        let fs = self.prepared_fs()?;
        let command = live_command(std::env::current_exe());
        let browsers = mur_browser::chromium::system_browsers_dir();
        let spawn_dirs: Vec<String> = live_spawn_dir(browsers.as_deref()).into_iter().collect();
        let spawn = live_spawn();
        let policy = manage::Policy {
            fs: &fs,
            spawn: &spawn,
            spawn_dirs: &spawn_dirs,
        };
        Ok(
            manage::mcp_add_with_policy(
                self.0,
                LIVE_ENTRY,
                &command,
                argv,
                Some(network),
                &policy,
            )?
            .0,
        )
    }
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<(Vec<String>, Effect)> {
        Ok(self
            .locked_save(None, servers)?
            .expect("an unconditional save is never stale"))
    }
    fn save_if_unchanged(
        &mut self,
        expected: &[McpServerEntry],
        servers: Vec<McpServerEntry>,
    ) -> anyhow::Result<Option<(Vec<String>, Effect)>> {
        self.locked_save(Some(expected), servers)
    }
    fn pending_repairs(&mut self, servers: &[McpServerEntry]) -> anyhow::Result<Vec<String>> {
        // Same computation as `save`, on a copy that is never written. No
        // `prepared_fs` here: a preview must not create directories.
        let home = crate::cmd::agent::resolve_mur_home()?;
        let registry = mur_browser::chromium::system_server_registry_dir();
        let fs = live_fs(&home, registry.as_deref());
        let (_, mut profile) = crate::cmd::agent::load_profile_for_edit(self.0)?;
        Ok(apply_repairs(&mut profile, servers.to_vec(), &fs)?.1)
    }
}

/// What the live server execs besides itself and the browser build dir: the
/// interpreter `mur browser record` launches the Playwright server with. The
/// install probe runs unsealed and cannot see this missing; the sealed agent
/// then fails the exec with EPERM and the browser tools never list (#2400).
fn live_spawn() -> Vec<String> {
    vec![crate::cmd::browser::perms::NODE.to_string()]
}

/// Re-pin the live entry onto this binary and backfill its fs/spawn grants on
/// `profile` in memory. Returns whether `servers` differ from the profile's,
/// and one note per repair. Shared by `save` and `pending_repairs` so the
/// preview names exactly what the write will do.
fn apply_repairs(
    profile: &mut mur_common::AgentProfile,
    servers: Vec<McpServerEntry>,
    fs: &FilesystemEntitlement,
) -> anyhow::Result<(bool, Vec<String>)> {
    let mut servers = servers;
    let mut notes = Vec::new();
    let mut spawn_command = None;
    if let Some(entry) = servers.iter_mut().find(|e| e.name == LIVE_ENTRY) {
        // Hash before any write: a binary we cannot hash must not be
        // pinned, and bailing here leaves the profile untouched.
        let command = live_command(std::env::current_exe());
        let resolved = crate::cmd::agent_mcp_pin::resolve_command(&command)
            .with_context(|| format!("resolve browser live command `{command}`"))?;
        let sha = crate::cmd::agent_mcp_pin::compute_binary_sha256(&resolved)?;
        notes.extend(repin_command(entry, &command, sha));
        spawn_command = Some(command);
    }
    let servers_changed = profile.mcp_servers != servers;
    profile.mcp_servers = servers;
    notes.extend(manage::merge_fs(&mut profile.entitlements.filesystem, fs));
    if let Some(command) = spawn_command {
        notes.extend(ensure_spawn_allowed(
            &mut profile.entitlements.processes.spawn.allowed,
            &command,
        ));
        notes.extend(manage::merge_spawn(
            &mut profile.entitlements.processes.spawn.allowed,
            &live_spawn(),
        ));
        let browsers = mur_browser::chromium::system_browsers_dir();
        if let Some(dir) = live_spawn_dir(browsers.as_deref()) {
            notes.extend(ensure_spawn_dir(
                &mut profile.entitlements.processes.spawn.allowed_dirs,
                &dir,
            ));
        }
    }
    Ok((servers_changed, notes))
}

impl ProfileOps<'_> {
    /// Load, optionally compare, repair, and write `profile.yaml` while
    /// holding [`PROFILE_LOCK`]. `None` back means `expected` no longer
    /// matches what is on disk and nothing was written.
    fn locked_save(
        &self,
        expected: Option<&[McpServerEntry]>,
        servers: Vec<McpServerEntry>,
    ) -> anyhow::Result<Option<(Vec<String>, Effect)>> {
        let fs = self.prepared_fs()?;
        let home = crate::cmd::agent::resolve_mur_home()?;
        let _lock = lock_profile(&home.join("agents").join(self.0))?;
        let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(self.0)?;
        if expected.is_some_and(|e| e != profile.mcp_servers.as_slice()) {
            return Ok(None);
        }
        let before = profile.clone();
        let (servers_changed, notes) = apply_repairs(&mut profile, servers, &fs)?;
        // Nothing to persist: skip the write so `updated_at` stays put and the
        // caller can tell "no change" from "saved".
        if servers_changed || !notes.is_empty() {
            crate::cmd::agent::save_profile(&path, &mut profile)?;
        }
        // Classified like the running agent will: by the reload diff.
        Ok(Some((notes, Effect::of_write(&before, &profile))))
    }

    fn record_research_event(&self) -> anyhow::Result<()> {
        let home = crate::cmd::agent::resolve_mur_home()?;
        crate::cmd::agent::mcp_network::record_broad_audited_enabled(
            &home,
            self.0,
            LIVE_ENTRY,
            chrono::Utc::now(),
        )
    }

    /// [`live_fs`] for this install, vetted and ready to seal: same guards as
    /// `perm allow-*`, and write dirs created, since the seal drops a path
    /// that does not exist yet. Runs before any profile write.
    fn prepared_fs(&self) -> anyhow::Result<FilesystemEntitlement> {
        let home = crate::cmd::agent::resolve_mur_home()?;
        let registry = mur_browser::chromium::system_server_registry_dir();
        let fs = live_fs(&home, registry.as_deref());
        for (paths, write) in [(&fs.read, false), (&fs.write, true)] {
            for p in paths {
                crate::cmd::agent::perm::reject_ungrantable_path(self.0, p, write)?;
                if write {
                    std::fs::create_dir_all(p)
                        .with_context(|| format!("create browser live dir {p}"))?;
                }
            }
        }
        Ok(fs)
    }
}

/// Per-agent lock file serializing the browser writers' read-compare-write of
/// `profile.yaml`. Separate from `running.lock`, whose pid contents mean
/// "agent is running". Never removed.
const PROFILE_LOCK: &str = "profile.yaml.lock";

/// Exclusive `flock` on `<agent_dir>/profile.yaml.lock`, held until dropped.
/// Blocking: holders only span one load-and-write. `fs2` for the reasons in
/// `cli/login.rs::acquire_login_lock`.
fn lock_profile(agent_dir: &std::path::Path) -> anyhow::Result<std::fs::File> {
    use fs2::FileExt;
    let path = agent_dir.join(PROFILE_LOCK);
    let f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(false)
        .open(&path)
        .with_context(|| format!("open {}", path.display()))?;
    f.lock_exclusive()
        .with_context(|| format!("lock {}", path.display()))?;
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> mur_common::AgentProfile {
        serde_yaml::from_str(include_str!(
            "../../../../../mur-common/tests/fixtures/minimal_profile.yaml"
        ))
        .unwrap()
    }

    fn live_entry() -> McpServerEntry {
        McpServerEntry {
            name: LIVE_ENTRY.into(),
            command: "mur".into(),
            ..Default::default()
        }
    }

    /// #2400: the sealed live server execs `node`; without the grant the exec
    /// fails with EPERM. A save on an existing entry backfills it, once.
    #[test]
    fn repairs_grant_the_interpreter_the_live_server_execs() {
        let node = crate::cmd::browser::perms::NODE.to_string();
        let mut p = profile();
        assert!(!p.entitlements.processes.spawn.allowed.contains(&node));
        let fs = FilesystemEntitlement::default();

        let (_, notes) = apply_repairs(&mut p, vec![live_entry()], &fs).unwrap();
        assert!(p.entitlements.processes.spawn.allowed.contains(&node));
        assert!(
            notes.iter().any(|n| n == "allowed spawn of node"),
            "{notes:?}"
        );

        let servers = p.mcp_servers.clone();
        let (_, again) = apply_repairs(&mut p, servers, &fs).unwrap();
        assert!(!again.iter().any(|n| n.contains("node")), "{again:?}");
        let grants = p.entitlements.processes.spawn.allowed.iter();
        assert_eq!(grants.filter(|a| **a == node).count(), 1);
    }

    /// No live entry, no grant: saving other servers must not widen spawn.
    #[test]
    fn repairs_without_a_live_entry_grant_no_spawn() {
        let mut p = profile();
        let before = p.entitlements.processes.spawn.allowed.clone();
        let fs = FilesystemEntitlement::default();
        let (_, notes) = apply_repairs(&mut p, Vec::new(), &fs).unwrap();
        assert_eq!(p.entitlements.processes.spawn.allowed, before);
        assert!(notes.is_empty(), "{notes:?}");
    }
}
