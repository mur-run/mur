//! Where installed agents live under `<mur_home>/agents/`: the one shape test
//! (`installed_agent`) and the one enumerator (`agent_dirs`).
//!
//! `agents/` also holds directories that are not agents: `.purged.<name>.<n>`
//! (a purge's rename-away, deleted after release) and `.mur.seeding*` (the Hub
//! seed's staging). Every name here goes through `validate_agent_name`, which
//! rejects a leading or embedded `.`, so those never reach a caller.

use std::path::{Path, PathBuf};

use crate::agent_name::validate_agent_name;
use crate::lock_file::agent_exists;
use crate::paths::AGENTS;

/// The agent name when `agent_home` is exactly `<mur_home>/agents/<name>` and
/// `<name>` passes `validate_agent_name`; `None` for any other path (a cache
/// home, a nested directory, a `.purged.*` leftover).
///
/// The comparison is lexical on purpose: it answers "is this the shape of an
/// installed home", not "does it exist".
pub fn installed_agent<'a>(agent_home: &'a Path, mur_home: &Path) -> Option<&'a str> {
    let name = agent_home.file_name()?.to_str()?;
    validate_agent_name(name).ok()?;
    (agent_home == mur_home.join(AGENTS).join(name)).then_some(name)
}

/// Every directory under `<mur_home>/agents/` whose name is a valid agent
/// name, as `(name, path)`, sorted by name. A missing `agents/` is empty.
/// Plain files and symlinks are not directories here and are skipped.
pub fn agent_dirs(mur_home: &Path) -> impl Iterator<Item = (String, PathBuf)> + use<> {
    let root = mur_home.join(AGENTS);
    let mut found: Vec<(String, PathBuf)> = std::fs::read_dir(&root)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            validate_agent_name(&name).ok()?;
            Some((name, e.path()))
        })
        .collect();
    found.sort();
    found.into_iter()
}

/// [`agent_dirs`] restricted to agents that exist ([`agent_exists`]).
pub fn installed_agents(mur_home: &Path) -> impl Iterator<Item = (String, PathBuf)> + use<> {
    let mur_home = mur_home.to_path_buf();
    agent_dirs(&mur_home).filter(move |(name, _)| agent_exists(&mur_home, name))
}

#[cfg(test)]
mod tests;
