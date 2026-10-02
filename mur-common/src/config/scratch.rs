//! Per-agent scratch dir (`<mur_home>/tmp/<agent>`) tuning (`scratch:`).

use super::*;

/// Default age after which a top-level scratch entry is pruned at start.
pub const SCRATCH_DEFAULT_RETENTION_DAYS: u32 = 7;
/// Default per-agent size above which `mur agent doctor` warns.
pub const SCRATCH_DEFAULT_WARN_SIZE_MB: u64 = 2048;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScratchConfig {
    /// Entries whose newest mtime is older than this many days are removed
    /// by the supervisor before the sandbox seals.
    #[serde(default = "scratch_default_retention_days")]
    pub retention_days: u32,
    /// `mur agent doctor` warns when an agent's scratch dir exceeds this.
    #[serde(default = "scratch_default_warn_size_mb")]
    pub warn_size_mb: u64,
}

fn scratch_default_retention_days() -> u32 {
    SCRATCH_DEFAULT_RETENTION_DAYS
}

fn scratch_default_warn_size_mb() -> u64 {
    SCRATCH_DEFAULT_WARN_SIZE_MB
}

impl Default for ScratchConfig {
    fn default() -> Self {
        Self {
            retention_days: SCRATCH_DEFAULT_RETENTION_DAYS,
            warn_size_mb: SCRATCH_DEFAULT_WARN_SIZE_MB,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_without_scratch_block_uses_defaults() {
        let c: Config = serde_yaml::from_str("llm: {}\n").unwrap();
        assert_eq!(c.scratch.retention_days, 7);
        assert_eq!(c.scratch.warn_size_mb, 2048);
        let p: ScratchConfig = serde_yaml::from_str("retention_days: 3\n").unwrap();
        assert_eq!((p.retention_days, p.warn_size_mb), (3, 2048));
    }
}
