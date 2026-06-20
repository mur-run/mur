//! Transport seam for `.fleet` bundles. Phase A ships `LocalFile`; future
//! `TeamServer` / `OfficialRegistry` impls slot in without touching build/parse.

use std::path::Path;

use anyhow::{Context, Result};

/// Reads/writes opaque bundle bytes from/to a location identifier.
pub trait FleetBundleTransport {
    fn read(&self, src: &str) -> Result<Vec<u8>>;
    fn write(&self, dst: &str, bytes: &[u8]) -> Result<()>;
}

/// Local-filesystem transport: `src`/`dst` are file paths.
pub struct LocalFile;

impl FleetBundleTransport for LocalFile {
    fn read(&self, src: &str) -> Result<Vec<u8>> {
        std::fs::read(Path::new(src)).with_context(|| format!("read bundle {src}"))
    }
    fn write(&self, dst: &str, bytes: &[u8]) -> Result<()> {
        std::fs::write(Path::new(dst), bytes).with_context(|| format!("write bundle {dst}"))
    }
}
