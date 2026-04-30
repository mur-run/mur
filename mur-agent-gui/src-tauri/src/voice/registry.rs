//! Installed-voice index — stub for M1.1.2; full impl in M1.2.3.

use anyhow::Result;
use std::path::Path;

#[derive(Default)]
pub struct VoiceRegistry;

impl VoiceRegistry {
    /// Loads (or initialises) the registry under `<app_data>/voices/`.
    /// M1.1.2 stub returns a default empty registry; M1.2.3 implements
    /// JSON persistence + bundled-voice seeding.
    pub async fn load(_app_data_dir: &Path) -> Result<Self> {
        Ok(Self)
    }
}
