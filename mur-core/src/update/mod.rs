//! `mur update` self-update implementation.
//!
//! The update flow is driven by `run()`. Network and platform-specific code is
//! split into submodules to keep each file under the 800-line rule and to make
//! unit testing possible.

pub mod release;
pub mod source;
pub mod swap;

use anyhow::Result;

#[derive(Debug, Clone, Copy)]
pub struct UpdateOptions {
    pub check_only: bool,
}

pub fn run(_opts: UpdateOptions) -> Result<()> {
    anyhow::bail!("not yet implemented")
}
