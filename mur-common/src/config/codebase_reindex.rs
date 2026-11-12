//! Daemon-side codebase reindex tuning (`codebase_reindex:`).
//!
//! A sandboxed agent cannot write `~/.mur/indexes/codebase/`, so its
//! post-commit hook drops a wake-up into the inbox and the daemon runs the
//! index worker instead. Every key is optional; an absent block means the
//! defaults below.

use super::*;

/// Default: the daemon consumes wake-ups and reconciles stale indexes.
pub const CODEBASE_REINDEX_DEFAULT_ENABLED: bool = true;
/// Default inbox sweep interval.
pub const CODEBASE_REINDEX_DEFAULT_POLL_SECS: u64 = 30;
/// Default interval for a reconcile pass that runs without any wake-up
/// (missed hooks, daemon was down).
pub const CODEBASE_REINDEX_DEFAULT_RECONCILE_SECS: u64 = 3600;
/// Default minimum gap between two attempts for one project.
pub const CODEBASE_REINDEX_DEFAULT_MIN_INTERVAL_SECS: u64 = 120;
/// Default ceiling for the failure backoff.
pub const CODEBASE_REINDEX_DEFAULT_MAX_BACKOFF_SECS: u64 = 3600;
/// Default number of inbox entries handled per sweep.
pub const CODEBASE_REINDEX_DEFAULT_MAX_INTAKE: usize = 256;
/// Default number of index children running at once.
pub const CODEBASE_REINDEX_DEFAULT_MAX_CONCURRENT: usize = 1;
/// Default wall-clock limit before a child is killed.
pub const CODEBASE_REINDEX_DEFAULT_JOB_TIMEOUT_SECS: u64 = 1800;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CodebaseReindexConfig {
    /// Daemon consumes wake-ups and reconciles. Off = no daemon task at all.
    #[serde(default = "cr_default_enabled")]
    pub enabled: bool,
    /// Inbox sweep interval, seconds.
    #[serde(default = "cr_default_poll_secs")]
    pub poll_secs: u64,
    /// Reconcile without a wake-up every this many seconds.
    #[serde(default = "cr_default_reconcile_secs")]
    pub reconcile_secs: u64,
    /// Minimum gap between attempts for one project, seconds. Also the
    /// starting value of the failure backoff.
    #[serde(default = "cr_default_min_interval_secs")]
    pub min_interval_secs: u64,
    /// Failure backoff ceiling, seconds.
    #[serde(default = "cr_default_max_backoff_secs")]
    pub max_backoff_secs: u64,
    /// Inbox entries handled per sweep; the rest wait for the next one.
    #[serde(default = "cr_default_max_intake")]
    pub max_intake: usize,
    /// Concurrent index children.
    #[serde(default = "cr_default_max_concurrent")]
    pub max_concurrent: usize,
    /// Kill a child that runs longer than this, seconds.
    #[serde(default = "cr_default_job_timeout_secs")]
    pub job_timeout_secs: u64,
}

fn cr_default_enabled() -> bool {
    CODEBASE_REINDEX_DEFAULT_ENABLED
}
fn cr_default_poll_secs() -> u64 {
    CODEBASE_REINDEX_DEFAULT_POLL_SECS
}
fn cr_default_reconcile_secs() -> u64 {
    CODEBASE_REINDEX_DEFAULT_RECONCILE_SECS
}
fn cr_default_min_interval_secs() -> u64 {
    CODEBASE_REINDEX_DEFAULT_MIN_INTERVAL_SECS
}
fn cr_default_max_backoff_secs() -> u64 {
    CODEBASE_REINDEX_DEFAULT_MAX_BACKOFF_SECS
}
fn cr_default_max_intake() -> usize {
    CODEBASE_REINDEX_DEFAULT_MAX_INTAKE
}
fn cr_default_max_concurrent() -> usize {
    CODEBASE_REINDEX_DEFAULT_MAX_CONCURRENT
}
fn cr_default_job_timeout_secs() -> u64 {
    CODEBASE_REINDEX_DEFAULT_JOB_TIMEOUT_SECS
}

impl Default for CodebaseReindexConfig {
    fn default() -> Self {
        Self {
            enabled: CODEBASE_REINDEX_DEFAULT_ENABLED,
            poll_secs: CODEBASE_REINDEX_DEFAULT_POLL_SECS,
            reconcile_secs: CODEBASE_REINDEX_DEFAULT_RECONCILE_SECS,
            min_interval_secs: CODEBASE_REINDEX_DEFAULT_MIN_INTERVAL_SECS,
            max_backoff_secs: CODEBASE_REINDEX_DEFAULT_MAX_BACKOFF_SECS,
            max_intake: CODEBASE_REINDEX_DEFAULT_MAX_INTAKE,
            max_concurrent: CODEBASE_REINDEX_DEFAULT_MAX_CONCURRENT,
            job_timeout_secs: CODEBASE_REINDEX_DEFAULT_JOB_TIMEOUT_SECS,
        }
    }
}

impl CodebaseReindexConfig {
    /// Clamp values that are legal YAML but illegal at runtime, called from
    /// `Config::sanitize` at load. A zero period panics
    /// `tokio::time::interval`; a zero intake or concurrency leaves the task
    /// looking enabled while it never works; a zero backoff start never
    /// grows; a ceiling below the start contradicts itself.
    pub(crate) fn sanitize(&mut self) {
        fn nonzero_u64(v: &mut u64, d: u64) {
            if *v == 0 {
                *v = d;
            }
        }
        fn nonzero_usize(v: &mut usize, d: usize) {
            if *v == 0 {
                *v = d;
            }
        }
        nonzero_u64(&mut self.poll_secs, CODEBASE_REINDEX_DEFAULT_POLL_SECS);
        nonzero_u64(
            &mut self.reconcile_secs,
            CODEBASE_REINDEX_DEFAULT_RECONCILE_SECS,
        );
        nonzero_u64(
            &mut self.min_interval_secs,
            CODEBASE_REINDEX_DEFAULT_MIN_INTERVAL_SECS,
        );
        nonzero_u64(
            &mut self.max_backoff_secs,
            CODEBASE_REINDEX_DEFAULT_MAX_BACKOFF_SECS,
        );
        nonzero_u64(
            &mut self.job_timeout_secs,
            CODEBASE_REINDEX_DEFAULT_JOB_TIMEOUT_SECS,
        );
        nonzero_usize(&mut self.max_intake, CODEBASE_REINDEX_DEFAULT_MAX_INTAKE);
        nonzero_usize(
            &mut self.max_concurrent,
            CODEBASE_REINDEX_DEFAULT_MAX_CONCURRENT,
        );
        if self.max_backoff_secs < self.min_interval_secs {
            self.max_backoff_secs = self.min_interval_secs;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_without_block_uses_spec_defaults() {
        let c: Config = serde_yaml::from_str("llm: {}\n").unwrap();
        let r = &c.codebase_reindex;
        assert!(r.enabled);
        assert_eq!((r.poll_secs, r.reconcile_secs), (30, 3600));
        assert_eq!((r.min_interval_secs, r.max_backoff_secs), (120, 3600));
        assert_eq!((r.max_intake, r.max_concurrent), (256, 1));
        assert_eq!(r.job_timeout_secs, 1800);
        assert_eq!(*r, CodebaseReindexConfig::default());
    }

    #[test]
    fn partial_block_keeps_other_defaults() {
        let c: Config =
            serde_yaml::from_str("codebase_reindex:\n  enabled: false\n  max_concurrent: 2\n")
                .unwrap();
        let r = &c.codebase_reindex;
        assert!(!r.enabled);
        assert_eq!(r.max_concurrent, 2);
        assert_eq!(r.poll_secs, CODEBASE_REINDEX_DEFAULT_POLL_SECS);
        assert_eq!(
            r.job_timeout_secs,
            CODEBASE_REINDEX_DEFAULT_JOB_TIMEOUT_SECS
        );
    }

    #[test]
    fn round_trips_through_yaml() {
        let mut c = Config::default();
        c.codebase_reindex.max_intake = 9;
        let back: Config = serde_yaml::from_str(&serde_yaml::to_string(&c).unwrap()).unwrap();
        assert_eq!(back.codebase_reindex, c.codebase_reindex);
    }

    fn load(yaml: &str) -> CodebaseReindexConfig {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.yaml");
        std::fs::write(&path, yaml).unwrap();
        Config::load_or_default(&path).codebase_reindex
    }

    /// Zeroes are legal YAML but illegal at runtime; the loader clamps each
    /// one to its default.
    #[test]
    fn zero_values_clamp_to_defaults_at_load() {
        let r = load(
            "codebase_reindex:\n  poll_secs: 0\n  reconcile_secs: 0\n  min_interval_secs: 0\n  max_backoff_secs: 0\n  max_intake: 0\n  max_concurrent: 0\n  job_timeout_secs: 0\n",
        );
        assert_eq!(r, CodebaseReindexConfig::default());
    }

    #[test]
    fn backoff_ceiling_below_start_is_raised_to_start() {
        let r = load("codebase_reindex:\n  min_interval_secs: 600\n  max_backoff_secs: 60\n");
        assert_eq!((r.min_interval_secs, r.max_backoff_secs), (600, 600));
    }

    /// The clamp must not rewrite legitimate tuning.
    #[test]
    fn positive_values_survive_the_load() {
        let r = load(
            "codebase_reindex:\n  enabled: false\n  poll_secs: 5\n  min_interval_secs: 10\n  max_backoff_secs: 20\n  max_concurrent: 3\n",
        );
        assert!(!r.enabled);
        assert_eq!((r.poll_secs, r.max_concurrent), (5, 3));
        assert_eq!((r.min_interval_secs, r.max_backoff_secs), (10, 20));
    }
}
