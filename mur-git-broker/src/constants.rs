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
/// Hard CPU and address-space caps applied to the pack parser child (F13). The wall-clock cap is
/// `BrokerLimits::max_index_wall_secs`.
pub const PARSER_CPU_SECS: u64 = 60;
#[cfg(target_os = "linux")]
pub const PARSER_ADDRESS_SPACE_BYTES: u64 = 2 * 1024 * 1024 * 1024;
/// Mode of the scratch dir the parser writes into.
pub const PARSER_OUT_DIR_MODE: u32 = 0o700;
/// The only files the broker moves out of the parser's scratch dir into `objects/pack/`.
pub const IMPORTED_PACK_FILES: [&str; 3] = ["pack.pack", "pack.idx", "pack.rev"];

/// Owner-only: the pending table holds full action documents.
pub const PENDING_DB_MODE: u32 = 0o600;
/// How long a second writer waits on the SQLite lock before giving up with `Storage`.
pub const PENDING_DB_BUSY_WAIT_MS: u64 = 5_000;
/// Endpoint scheme for a remote on this machine's filesystem.
pub const ENDPOINT_FILE_SCHEME: &str = "file://";
/// Endpoint scheme for a network remote. Lowercase only; there is no `http` in a release build.
pub const ENDPOINT_HTTPS_SCHEME: &str = "https://";
/// An explicit `:443` is a second spelling of "no port", so it is refused.
pub const HTTPS_DEFAULT_PORT: u16 = 443;
/// RFC 1035: a DNS name is at most 253 characters, each label at most 63.
pub const ENDPOINT_HOST_MAX_LEN: usize = 253;
pub const ENDPOINT_LABEL_MAX_LEN: usize = 63;
/// Test seam (`test-support` / `cfg(test)` only): the one plaintext endpoint form, loopback http.
#[cfg(any(test, feature = "test-support"))]
pub const ENDPOINT_LOOPBACK_HTTP_PREFIX: &str = "http://127.0.0.1:";
/// The default `http` port; an explicit `:80` is refused for the same reason as `:443`.
#[cfg(any(test, feature = "test-support"))]
pub const HTTP_DEFAULT_PORT: u16 = 80;
pub const ENROLLMENTS_VERSION: u32 = 1;
/// Hex characters of a repo identity (128 random bits).
pub const IDENTITY_HEX_LEN: usize = 32;
pub const ENROLLMENTS_FILE_MODE: u32 = 0o600;
/// The agent-side tool reads the registry, so it is not owner-only.
pub const REGISTRY_FILE_MODE: u32 = 0o644;
/// A credential username is 1..=this many bytes of `[A-Za-z0-9._-]` (https design §6).
pub const CREDENTIAL_USERNAME_MAX_LEN: usize = 64;

/// A credential file larger than this is refused unread: a token is a few hundred bytes, so a big
/// file is a wrong path, not a secret.
pub const CREDENTIAL_MAX_BYTES: u64 = 4096;
/// The only permission bits a credential file may have (owner read/write, nothing else).
pub const CREDENTIAL_FILE_MODE: u32 = 0o600;
