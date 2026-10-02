use mur_common::agent::{Entitlements, NetworkOutboundMode, SpawnMode};
#[cfg(not(target_os = "windows"))]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

mod build;
mod widen;

#[cfg(test)]
mod tests;

/// General TCP ports a `Restricted` profile opens (`*:port`; hostnames still
/// gated by the app-layer HostGuard). Also the set the runtime SELF profile
/// regains under ProxyOnly when it hosts the in-process egress proxy — the
/// proxy's upstream dials are the one legitimate general egress of the
/// runtime process (see `SandboxPolicy::allow_in_process_proxy_upstream`).
pub const RESTRICTED_GENERAL_PORTS: [u16; 4] = [80, 443, 8080, 8443];

/// Files under the agent's own home dir that are ALWAYS denied, regardless
/// of what the entitlements grant (issue #712, fail-closed): `profile.yaml`
/// is the agent's own entitlements source and `identity.key` its signing
/// identity — an agent that can rewrite either and self-restart escalates
/// past every other control (observed live, 2026-07-16).
///
/// `identity.pub` and `rotations.jsonl` are the key other parties verify this
/// agent against (`mur_channel::sign::resolve_writer_pubkey` reads the chain
/// first, then falls back to the pubkey). For the router agent that key is the
/// human's authority: a HITL answer releases a gate only if it verifies against
/// it (`hitl::authority::is_router_authority`). Replacing either file with a
/// key the agent generated itself lets it sign its own approvals, and
/// `verify_chain` has no trust anchor, so a planted chain passes. Nothing in
/// the runtime writes either file after the seal — `mur agent create` and
/// `mur agent rekey` do, from the user's shell.
///
/// The rest of agent_home stays writable (running.lock, running.sentinel,
/// stderr.log). Shared with the tool-level entitlement gate
/// (`tools::fs_policy`) so the kernel profile and the file tools deny the same
/// set. Which of these stay READABLE is [`SELF_PROTECTED_WRITE_ONLY`].
pub const SELF_PROTECTED_AGENT_FILES: [&str; 4] = [
    "profile.yaml",
    "identity.key",
    "identity.pub",
    "rotations.jsonl",
];

/// The subset of [`SELF_PROTECTED_AGENT_FILES`] denied for WRITE only.
///
/// `profile.yaml` (issue #007): reading its own entitlements escalates nothing
/// and is how the agent explains its limits. `identity.pub` and
/// `rotations.jsonl` are public verification material: peers and the runtime
/// itself read them to verify this agent's signed events, and a read deny
/// would fail-close every channel it writes to. `identity.key` is absent — it
/// stays read-denied, because holding it is signing authority.
///
/// One list for both enforcement points, so the SBPL profile and the file-tool
/// read gate cannot disagree about what is readable.
pub const SELF_PROTECTED_WRITE_ONLY: [&str; 3] =
    ["profile.yaml", "identity.pub", "rotations.jsonl"];

/// Expand a `~`-relative entitlement path the way the sandbox builder does.
///
/// Public because a grant only takes effect if the path exists when the
/// profile is sealed (see the dead-grant drop below). `mur agent perm ...` and
/// `mur agent doctor` must test the *same* path the kernel will, or they end
/// up confidently reporting on a path the sandbox never saw.
pub fn expand_entitlement_path(s: &str) -> PathBuf {
    let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
    if let Some(rest) = s.strip_prefix("~/") {
        home.join(rest)
    } else if s == "~" {
        home
    } else {
        PathBuf::from(s)
    }
}

/// Append the resolved form of every path that differs from it, once.
///
/// Seatbelt evaluates `subpath` against the RESOLVED path, so a grant named by
/// a symlink (a relocated cache, a `/tmp` path) matched nothing and every
/// access under it was denied without a hint. The link path is kept as well:
/// Landlock and the tool-level gate compare the name as written. Paths that do
/// not resolve (a dead deny entry) are left as they are.
fn with_resolved_aliases(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut out = paths.clone();
    for p in &paths {
        if let Ok(real) = std::fs::canonicalize(p)
            && real != *p
            && !out.contains(&real)
        {
            out.push(real);
        }
    }
    out
}

/// Resolved, OS-ready sandbox policy derived from agent entitlements.
/// All paths are absolute (tilde expanded). All fields are ready to
/// feed directly to Landlock / SBPL / Job Object APIs.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
    /// Filesystem grants that were discarded while building this policy, so
    /// the kernel never received them however they read in `profile.yaml`.
    pub dropped: Vec<mur_common::agent::DroppedGrant>,
    /// Paths the process may read (not write).
    pub fs_read: Vec<PathBuf>,
    /// Paths the process may read AND write.
    pub fs_write: Vec<PathBuf>,
    /// Paths that are explicitly denied (override fs_read/fs_write).
    pub fs_deny: Vec<PathBuf>,
    /// Directories containing executable binaries the process may exec.
    pub fs_exec: Vec<PathBuf>,
    /// The agent's process-spawn policy (allowlist / any / none), from
    /// entitlements.processes.spawn.mode.
    pub spawn_mode: SpawnMode,
    /// LITERAL exec grants (Issue 17): every absolute, canonicalized path
    /// resolved from `entitlements.processes.spawn.allowed` bare binary
    /// names, searched across [`crate::exec_dirs::standard_exec_dirs`],
    /// [`system_exec_paths`], and (existence-checked) the active
    /// Xcode/CommandLineTools developer dirs. An entry may resolve to
    /// MULTIPLE literal paths (e.g. the same binary name present under
    /// both Homebrew and the developer tools). Entries that resolve to
    /// nothing are dropped with a warning — fail-closed for that one
    /// binary, never fail-open for the whole profile.
    pub spawn_allowed_paths: Vec<PathBuf>,
    /// PREFIX exec grants (Issue 17) derived from `spawn_allowed_paths`:
    /// for each literal match, the enclosing package/toolchain directory
    /// (the binary's parent, or grandparent when the parent is literally
    /// named `bin`) — e.g. a Homebrew keg's prefix (covering sibling
    /// `libexec/git-core`, `lib`) or a rustup toolchain directory (covering
    /// sibling `lib`, `libexec`). Never a filesystem root, `/usr`, `/opt`,
    /// `/opt/homebrew`, the home directory, or a top-level `/Volumes/<name>`
    /// mount — those are guarded back down to just the binary's own parent
    /// dir, since granting exec over the whole prefix there would be far
    /// broader than the single allowlisted binary. Consumed by
    /// `macos::build_sbpl_profile` as `subpath` (not `path-literal`) allow
    /// clauses, so a toolchain's helper binaries keep working without each
    /// one being individually allowlisted.
    pub spawn_allowed_prefixes: Vec<PathBuf>,
    /// Outbound TCP ports that are allowed. `None` = allow all; `Some([])` = deny all.
    pub net_allow_ports: Option<Vec<u16>>,
    /// Loopback-only TCP port carve-outs (e.g. the in-runtime egress proxy's
    /// listener). Emitted as `remote tcp "localhost:{port}"` on macOS SBPL;
    /// on Linux Landlock (port-only, no host scoping) as a plain
    /// `NetPort ConnectTcp` rule. Only populated in Restricted mode — see
    /// `allow_loopback_ports`.
    pub net_allow_loopback_ports: Vec<u16>,
    /// True when the posture permits loopback carve-outs (Restricted +
    /// ProxyOnly) but NOT Off/Unrestricted. This is the signal that
    /// distinguishes a ProxyOnly `net_allow_ports = Some([])` (deny general TCP,
    /// allow the loopback proxies) from an Off `Some([])` (deny everything) —
    /// the two are identical in `net_allow_ports`. Set by `from_entitlements`;
    /// consulted by `allow_extra_ports` / `allow_loopback_ports`.
    pub net_loopback_allowed: bool,
    /// Outbound hostnames for the reqwest guard layer.
    /// `None` = allow all (Unrestricted). `Some([])` = deny all (Off).
    pub net_allow_hosts: Option<Vec<String>>,
    /// Memory limit in megabytes (for Windows Job Object).
    pub memory_limit_mb: Option<u64>,
    /// MUR's own launch chain: the files that decide what starts next and
    /// with what authority (spec 2026-08-11). macOS emits it as three
    /// ordering tiers in SBPL; Linux drops any overlapping grant fail-closed.
    /// Set from `agent_home` in `from_entitlements`.
    pub launch_chain: crate::sandbox::launch_chain::LaunchChain,
    /// Write grants that overlap the launch chain and were dropped whole,
    /// fail-closed: Landlock cannot carve them (Linux), so they never reached
    /// the kernel. Read by `mur agent runtime-doctor` to report what the
    /// sandbox neutralised (spec 2026-08-11).
    pub dropped_grants: Vec<PathBuf>,
    /// Read grants dropped whole for overlapping the credential store or a
    /// sibling's signing key. Read counterpart of `dropped_grants`; same
    /// fail-closed rule, reported by `mur agent doctor`.
    pub dropped_read_grants: Vec<PathBuf>,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        SandboxPolicy {
            fs_read: Vec::new(),
            fs_write: Vec::new(),
            fs_deny: Vec::new(),
            fs_exec: Vec::new(),
            spawn_mode: SpawnMode::Allowlist,
            spawn_allowed_paths: Vec::new(),
            spawn_allowed_prefixes: Vec::new(),
            net_allow_ports: None,
            net_allow_loopback_ports: Vec::new(),
            net_loopback_allowed: false,
            net_allow_hosts: None,
            memory_limit_mb: None,
            launch_chain: crate::sandbox::launch_chain::LaunchChain::default(),
            dropped: Vec::new(),
            dropped_grants: Vec::new(),
            dropped_read_grants: Vec::new(),
        }
    }
}

/// The set of TCP ports that should receive a Landlock `ConnectTcp` rule:
/// the general allow-list (when outbound is restricted) plus the loopback
/// carve-outs. Returns empty for Unrestricted (`None`, Landlock installs no
/// net rules at all) and for Off (empty general + empty loopback). This is
/// the single source of truth the Linux builder iterates; macOS keeps the two
/// lists separate (it distinguishes `*:port` from `localhost:port`).
pub(crate) fn connect_tcp_ports(policy: &SandboxPolicy) -> Vec<u16> {
    match &policy.net_allow_ports {
        Some(ports) => ports
            .iter()
            .chain(policy.net_allow_loopback_ports.iter())
            .copied()
            .collect(),
        None => Vec::new(),
    }
}

/// Resolve a bare binary name (e.g. `"jq"`) to an absolute path by
/// searching `PATH` env dirs followed by the sandbox's own `fs_exec`
/// directories, returning the first candidate that exists and has at
/// least one executable bit set.
///
/// If `name` is already absolute, it is checked directly (and only
/// returned if it resolves to an executable file). Returns `None` if no
/// candidate resolves — callers must treat that as "drop this entry",
/// never as "allow anyway".
fn resolve_binary_path(name: &str, fs_exec: &[PathBuf]) -> Option<PathBuf> {
    let candidate_path = Path::new(name);
    if candidate_path.is_absolute() {
        return is_executable_file(candidate_path).then(|| candidate_path.to_path_buf());
    }

    let path_dirs = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect::<Vec<_>>())
        .unwrap_or_default();

    path_dirs
        .iter()
        .chain(fs_exec.iter())
        .map(|dir| dir.join(name))
        .find(|candidate| is_executable_file(candidate))
}

/// True if `path` exists, is a regular file, and has at least one
/// executable permission bit set (owner/group/other).
#[cfg(not(target_os = "windows"))]
fn is_executable_file(path: &Path) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file() && meta.permissions().mode() & 0o111 != 0,
        Err(_) => false,
    }
}

/// Windows has no POSIX executable bit; treat any existing regular file
/// as a resolvable candidate (mirrors PATH lookup semantics on Windows).
#[cfg(target_os = "windows")]
fn is_executable_file(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|meta| meta.is_file())
        .unwrap_or(false)
}

/// Derive the PREFIX exec grant (Issue 17) for a resolved literal binary
/// path: the enclosing package/toolchain directory, so sibling binaries
/// under the same Homebrew keg or rustup toolchain (e.g. `libexec/git-core`,
/// `lib`) are exec-permitted without listing each one individually.
///
/// Rule: the binary's parent directory, or its GRANDPARENT when the parent
/// is literally named `bin` (covers `<prefix>/bin/<tool>` layouts — Homebrew
/// kegs, rustup toolchains, CommandLineTools). Falls back to the parent
/// itself when that would otherwise resolve to a filesystem root, `/usr`,
/// `/opt`, `/opt/homebrew`, the user's home directory, or a top-level
/// `/Volumes/<name>` mount (depth <= 2) — granting exec over any of those
/// wholesale would be far broader than the "one toolchain" intent.
fn compute_spawn_prefix(literal: &Path, home: &Path) -> PathBuf {
    let parent = literal.parent().unwrap_or(literal);
    let candidate = if parent.file_name().is_some_and(|n| n == "bin") {
        parent.parent().unwrap_or(parent)
    } else {
        parent
    };

    if is_guarded_prefix(candidate, home) {
        parent.to_path_buf()
    } else {
        candidate.to_path_buf()
    }
}

/// True if `path` is too broad to grant as an exec prefix: a filesystem
/// root, `/usr`, `/opt`, `/opt/homebrew`, the home directory, or a
/// top-level `/Volumes/<name>` mount (depth <= 2, i.e. `/Volumes` or
/// `/Volumes/<name>` itself).
fn is_guarded_prefix(path: &Path, home: &Path) -> bool {
    if path == Path::new("/")
        || path == Path::new("/usr")
        || path == Path::new("/opt")
        || path == Path::new("/opt/homebrew")
        || path == home
    {
        return true;
    }
    if let Ok(rest) = path.strip_prefix("/Volumes") {
        let depth = rest.components().count();
        return depth <= 1;
    }
    false
}
pub(super) fn system_exec_paths(home: &Path) -> Vec<PathBuf> {
    #[cfg(not(target_os = "windows"))]
    {
        vec![
            PathBuf::from("/usr/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/bin"),
            home.join(".local/bin"),
        ]
    }
    #[cfg(target_os = "windows")]
    {
        let _ = home;
        vec![
            PathBuf::from(r"C:\Windows\System32"),
            PathBuf::from(r"C:\Windows"),
        ]
    }
}

/// Directories the `fleet_run` child must be able to write, under `mur_home`.
///
/// Every [`mur_common::paths::RUN_STATE_DIRS`] entry whole, EXCEPT
/// `fleet-state/`, which is carved per fleet: only `fleet-state/<name>/` for
/// each name in `fleet_run.fleets`. A whole-tree grant would let the agent's
/// own bash tool queue a job for ANY fleet — including a cron fleet the
/// operator never allowlisted, which the daemon then runs unattended. Names
/// that fail `valid_fleet_name` are skipped rather than joined, so a
/// hand-edited `../x` in config.yaml cannot become a write grant outside it.
///
/// `fleets/` (definitions, `.stopped`) is never here: a run that can rewrite
/// its fleet's members, limits or HITL pre-approvals, or clear its own
/// kill-switch, is a run that governs itself.
fn fleet_run_write_dirs(mur_home: &Path) -> Vec<PathBuf> {
    let allowed = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"))
        .fleet_run
        .fleets;
    let mut out = Vec::new();
    for dir in mur_common::paths::RUN_STATE_DIRS {
        if dir == mur_common::paths::FLEET_STATE {
            out.extend(
                allowed
                    .iter()
                    .filter(|f| mur_common::fleet::valid_fleet_name(f))
                    .map(|f| mur_common::paths::fleet_state_dir(mur_home, f)),
            );
        } else {
            out.push(mur_home.join(dir));
        }
    }
    out
}

fn system_read_paths() -> Vec<PathBuf> {
    // `mut` is only used inside the #[cfg(target_os = "macos")] block below;
    // allow the lint rather than restructure the initialization.
    #[allow(unused_mut)]
    let mut paths = vec![
        PathBuf::from("/etc"),
        PathBuf::from("/usr/lib"),
        PathBuf::from("/usr/share"),
        PathBuf::from("/lib"),
        PathBuf::from("/lib64"),
        PathBuf::from("/proc/self"),
        PathBuf::from("/dev/urandom"),
        PathBuf::from("/dev/null"),
    ];
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/System/Library"));
        paths.push(PathBuf::from("/private/var/folders"));
        paths.push(PathBuf::from("/private/tmp"));
    }
    paths
}
