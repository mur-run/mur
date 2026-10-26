//! The profile-backed [`LiveOps`] for `/browser live`: reads and writes the
//! agent's `profile.yaml`, re-pins the live entry, and backfills its grants.
//! Moved out of `browser_live_cmd.rs` verbatim.

use anyhow::Context;
use mur_common::agent::{FilesystemEntitlement, McpServerEntry, McpServerNetwork};

use super::browser_live_cmd::*;
use super::manage;

/// Real profile-backed [`LiveOps`].
pub(super) struct ProfileOps<'a>(pub &'a str);

impl LiveOps for ProfileOps<'_> {
    fn record_research(&mut self, _agent: &str) -> anyhow::Result<()> {
        self.record_research_event()
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
        let policy = manage::Policy {
            fs: &fs,
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
    fn save(&mut self, servers: Vec<McpServerEntry>) -> anyhow::Result<Vec<String>> {
        let fs = self.prepared_fs()?;
        let (path, mut profile) = crate::cmd::agent::load_profile_for_edit(self.0)?;
        let (servers_changed, notes) = apply_repairs(&mut profile, servers, &fs)?;
        // Nothing to persist: skip the write so `updated_at` stays put and the
        // caller can tell "no change" from "saved".
        if servers_changed || !notes.is_empty() {
            crate::cmd::agent::save_profile(&path, &mut profile)?;
        }
        Ok(notes)
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
