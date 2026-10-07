//! Every name and number the broker uses. Policy overrides the numbers; nothing else may
//! hardcode them (CLAUDE.md rule 2).
pub const ACTION_VERSION: &str = "mur.git-push.v1";
pub const ACTION_HASH_DOMAIN: &str = "mur.git-push.v1\n";
/// Only refs under this prefix may be pushed (spec §5).
pub const ALLOWED_REF_PREFIX: &str = "refs/heads/agent/";
pub const PREFETCH_REF_PREFIX: &str = "refs/prefetch/";
pub const APPROVAL_EXEC_WINDOW_SECS: i64 = 5 * 60;
pub const SHA1_HEX_LEN: usize = 40;
pub const SHA256_HEX_LEN: usize = 64;
/// Files/dirs the broker creates and the parser may never write (F28).
pub const CONTROL_PATHS: [&str; 7] = [
    "config",
    "HEAD",
    "hooks",
    "info",
    "objects/info",
    "packed-refs",
    "refs",
];
/// Present in a private repo ⇒ ancestry cannot be trusted (§5 items 2, 6).
pub const FORBIDDEN_REPO_PATHS: [&str; 6] = [
    "info/grafts",
    "shallow",
    "objects/info/alternates",
    "objects/info/commit-graph",
    "objects/info/commit-graphs",
    "objects/pack/multi-pack-index",
];
pub const ALLOWED_REPO_EXTENSIONS: [&str; 3] = ["promisor", "partialclone", "objectformat"];
pub const DEFAULT_MAX_PACK_BYTES: u64 = 64 * 1024 * 1024;
pub const DEFAULT_MAX_OBJECT_COUNT: u64 = 100_000;
pub const DEFAULT_MAX_BLOB_BYTES: u64 = 16 * 1024 * 1024;
pub const DEFAULT_MAX_INDEX_WALL_SECS: u64 = 60;
pub const DEFAULT_MAX_PREFETCH_BYTES: u64 = 256 * 1024 * 1024;
pub const DEFAULT_MAX_PREFETCH_WALL_SECS: u64 = 120;
pub const DEFAULT_MAX_PREFETCH_DISK_BYTES: u64 = 512 * 1024 * 1024;
pub const DEFAULT_MAX_CONCURRENT_PREFETCH: usize = 2;
pub const DEFAULT_MAX_PENDING_PER_AGENT: usize = 5;
pub const DEFAULT_MAX_NEW_REQUESTS_PER_WINDOW: usize = 10;
pub const DEFAULT_REQUEST_WINDOW_SECS: i64 = 3600;
pub const DEFAULT_GIT_TIMEOUT_SECS: u64 = 30;
pub const DEFAULT_PUSH_TIMEOUT_SECS: u64 = 120;
/// Directory name of the private repo under the root the caller hands to `PrivateRepo::create`.
pub const PRIVATE_REPO_DIR: &str = "private.git";
/// How often a running prefetch is checked against its byte budget.
pub const PREFETCH_WATCH_INTERVAL_MS: u64 = 100;
/// Where an update's current remote tip is fetched to.
pub const PREFETCH_OLD_REF: &str = "refs/prefetch/old";
/// Prefix for the creation-base refs fetched when a push creates a new branch.
pub const PREFETCH_BASE_REF_PREFIX: &str = "refs/prefetch/base/";
