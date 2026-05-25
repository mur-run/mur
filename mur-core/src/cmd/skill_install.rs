//! Stub — `mur skill install` / `mur skill update` handlers.
//! Implemented in Task 9.

use anyhow::Result;

pub fn cmd_install(_source: &str) -> Result<()> {
    anyhow::bail!("`mur skill install` not yet implemented (Task 9)")
}

pub fn cmd_update(_name: &str) -> Result<()> {
    anyhow::bail!("`mur skill update` not yet implemented (Task 9)")
}
