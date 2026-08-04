use mur_common::agent::{Entitlements, NetworkOutboundMode, SpawnMode};
#[cfg(not(target_os = "windows"))]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

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
/// past every other control (observed live, 2026-07-16). Only these two
/// files are denied; the rest of agent_home stays writable (running.lock,
/// running.sentinel, stderr.log). The runtime itself reads both once at
/// startup, BEFORE the sandbox seals, so the deny costs it nothing. Shared
/// with the tool-level entitlement gate (`tools::fs_policy`) so the kernel
/// profile and the file tools deny the same set.
pub const SELF_PROTECTED_AGENT_FILES: [&str; 2] = ["profile.yaml", "identity.key"];

/// Resolved, OS-ready sandbox policy derived from agent entitlements.
/// All paths are absolute (tilde expanded). All fields are ready to
/// feed directly to Landlock / SBPL / Job Object APIs.
#[derive(Debug, Clone)]
pub struct SandboxPolicy {
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
        }
    }
}

impl SandboxPolicy {
    pub fn from_entitlements(ent: &Entitlements, agent_home: &Path) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

        let expand = |s: &str| -> PathBuf {
            if let Some(rest) = s.strip_prefix("~/") {
                home.join(rest)
            } else if s == "~" {
                home.clone()
            } else {
                PathBuf::from(s)
            }
        };

        // USER-DECLARED read/write entitlement paths are existence-checked at
        // profile-build time and dropped (fail-closed, warned) if missing.
        // Rationale (Issue 16): a `fs_write`/`fs_read` entry that names a
        // nonexistent path (e.g. a removed git worktree) still gets emitted
        // as an ordinary SBPL `subpath` grant with no existence requirement,
        // so `sandbox_init_with_parameters` accepts it silently — but the
        // resulting unresolvable grant was observed to destabilize *other*
        // unrelated `file-write*` checks under the same compiled policy
        // (30s tool-call hangs, not EPERM, until the agent restarts with the
        // dead path recreated or removed from entitlements). Mirrors the
        // fail-closed discipline `resolve_binary_path`/`is_executable_file`
        // already apply to `spawn_allowed_paths` below.
        let mut fs_read: Vec<PathBuf> = ent
            .filesystem
            .read
            .iter()
            .map(|s| expand(s))
            .filter(|p| {
                if std::fs::metadata(p).is_ok() {
                    true
                } else {
                    tracing::warn!(
                        path = %p.display(),
                        "filesystem read entitlement path does not exist on disk; \
                         agent will NOT have read access to it (dropping dead grant \
                         to avoid destabilizing the sandbox profile — Issue 16)"
                    );
                    false
                }
            })
            .collect();
        let mut fs_write: Vec<PathBuf> = ent
            .filesystem
            .write
            .iter()
            .map(|s| expand(s))
            .filter(|p| {
                if std::fs::metadata(p).is_ok() {
                    true
                } else {
                    tracing::warn!(
                        path = %p.display(),
                        "filesystem write entitlement path does not exist on disk; \
                         agent will NOT have write access to it (dropping dead grant \
                         to avoid destabilizing the sandbox profile — Issue 16)"
                    );
                    false
                }
            })
            .collect();
        // fs_deny entries are kept verbatim even if the path doesn't exist:
        // dropping a dead deny entry would be fail-OPEN — if the path later
        // appears (mount, create, restore) the agent would silently regain
        // access we meant to permanently deny. Deny-side dead paths are
        // harmless (confirmed: no hang mechanism triggers off `fs_deny`).
        let mut fs_deny: Vec<PathBuf> = ent.filesystem.deny.iter().map(|s| expand(s)).collect();

        // Self-protection (issue #712): unconditionally deny the agent's own
        // profile.yaml + identity.key, even when a write grant covers them
        // (e.g. a broad grant on the agents root, or agent_home itself which
        // is force-granted just below). Without this, an agent can rewrite
        // its own entitlements and self-restart to apply them — the seal is
        // generated FROM the profile, so profile self-write defeats it. On
        // macOS the SBPL builder emits fs_deny after the write allows
        // (last-match-wins) so this beats any overlapping grant; Landlock
        // cannot express deny-within-allow, so the same paths are also
        // injected into the tool-level gate (`tools::fs_policy::self_protected`).
        for f in SELF_PROTECTED_AGENT_FILES {
            let p = agent_home.join(f);
            if !fs_deny.contains(&p) {
                fs_deny.push(p);
            }
        }

        // agent_home is always read+write — runtime cannot function without it.
        // (Its existence is a precondition of the runtime starting at all —
        // profile.yaml must already live there — so no create_dir_all needed.)
        if !fs_write.contains(&agent_home.to_path_buf()) {
            fs_write.push(agent_home.to_path_buf());
        }

        // The shared channel store (`<mur_home>/channels`) is runtime-owned: a
        // delegated agent appends its OWN signed reply there (peer-writes-own,
        // v3d-2). Always grant write regardless of the user's fs entitlement,
        // else `channel/delegate` self-reply silently fails on agents whose write
        // allowlist omits ~/.mur/channels (agent_home is <mur_home>/agents/<name>).
        if let Some(channels) = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(|m| m.join("channels"))
            && !fs_write.contains(&channels)
        {
            // Ensure the directory exists before granting it — same idiom as
            // the VLC `runtime_dir` precedent below in supervisor.rs. Unlike
            // user-declared entries, this path is runtime-owned so we create
            // it rather than drop the grant (Issue 16: a dead grant here
            // would destabilize other file-write* checks under this policy).
            let _ = std::fs::create_dir_all(&channels);
            fs_write.push(channels);
        }

        // `<mur_home>/open-items.jsonl` is the `open_item` built-in's only
        // write target. That tool's doc comment claimed an agent needs no
        // filesystem grant to use it, but nothing ever put the file in this
        // allowlist, so "record a todo" failed for every sandboxed agent —
        // and the failure surfaced as a bare `open <path>` with the errno
        // dropped, so the agent could only guess at the cause. Same
        // create-before-grant idiom as `channels`: an append-only log starts
        // out absent, and Landlock skips rules on paths that don't exist at
        // seal time.
        if let Some(items) = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(|m| m.join(mur_open_items::LOG_FILE))
            && !fs_write.contains(&items)
        {
            let _ = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&items);
            fs_write.push(items);
        }

        // `<mur_home>/index` holds three things with very different trust
        // requirements: the channels read-model subdir (`index/channels/`,
        // channels.db + WAL/SHM), the `*.lance` retrieval stores, and
        // `capabilities.json` (which the daemon injects UNSIGNED into the
        // operator's Claude session on every SessionStart — a prompt-
        // injection surface). Only `index/channels/` is granted here: a
        // delegated agent's self-append refreshes channels.db's
        // `updated_at` row, and without this grant SQLite maps the denied
        // write to SQLITE_READONLY, so every peer-writes-own append reports
        // a false failure (G3, live fleet run 2026-07-09). The rest of
        // `index/` is deliberately excluded — sandboxed members must not be
        // able to write capabilities.json (it feeds that unsigned inject)
        // or the lance stores (they shape retrieval). Same create-before-
        // grant idiom as `channels` (Landlock skips rules on paths that
        // don't exist at seal time).
        if let Some(channel_index_dir) = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(|m| m.join("index").join("channels"))
            && !fs_write.contains(&channel_index_dir)
        {
            let _ = std::fs::create_dir_all(&channel_index_dir);
            fs_write.push(channel_index_dir);
        }

        // fleet_run carve-ins (config-gated, deny-by-default): when THIS agent
        // is allowlisted in `~/.mur/config.yaml` `fleet_run.agents`, the
        // spawned `mur fleet run` / `mur deep-research` child (which inherits
        // this sandbox) needs the fleet-runner state dirs writable and the
        // `mur` binary spawnable. Gate lives in the global config — writable
        // by no agent — so a prompt-injected agent cannot widen it. Same
        // create-before-grant idiom as `channels` above. Reads need no grant
        // here (macOS reads are allow-default; the narrow read set for
        // Landlock is added to fs_read below alongside system paths).
        let mut fleet_run_enabled = false;
        if let (Some(mur_home), Some(agent_name)) = (
            agent_home.parent().and_then(|p| p.parent()),
            agent_home.file_name().and_then(|n| n.to_str()),
        ) && crate::tools::fleet_run::agent_enabled(mur_home, agent_name)
        {
            fleet_run_enabled = true;
            for dir in ["fleets", "commander", "conversations", "artifacts"] {
                let d = mur_home.join(dir);
                if !fs_write.contains(&d) {
                    let _ = std::fs::create_dir_all(&d);
                    fs_write.push(d);
                }
            }
            for p in [mur_home.join("models.yaml"), mur_home.join("cache")] {
                if std::fs::metadata(&p).is_ok() && !fs_read.contains(&p) {
                    fs_read.push(p);
                }
            }
        }

        // Standard system read paths: libraries, certs, DNS config.
        let system_read = system_read_paths();
        for p in system_read {
            if !fs_read.contains(&p) {
                fs_read.push(p);
            }
        }

        // Standard binary exec paths (needed for MCP spawn + shell tools).
        let fs_exec = system_exec_paths(&home);

        // Search dirs for resolving bare `spawn.allowed` binary names
        // (Issue 17): the shared exec_dirs list (Homebrew/Cargo/user-local,
        // kept in lockstep with the bash tool's PATH augmentation), the
        // standard system exec dirs, and — existence-checked, no subprocess
        // spawned — the active Xcode/CommandLineTools developer dirs. The
        // active developer dir is read directly from the
        // `/var/db/xcode_select_link` symlink target (this is exactly what
        // `xcode-select -p` resolves; reading the symlink avoids spawning a
        // subprocess to determine it, which the sandboxed exec chain cannot
        // rely on being permitted — Issue 17).
        let mut spawn_search_dirs: Vec<PathBuf> = crate::exec_dirs::standard_exec_dirs();
        spawn_search_dirs.extend(system_exec_paths(&home));
        if let Ok(xcode_dir) = std::fs::read_link("/var/db/xcode_select_link") {
            let usr_bin = xcode_dir.join("usr/bin");
            if usr_bin.exists() {
                spawn_search_dirs.push(usr_bin);
            }
        }
        let clt_usr_bin = PathBuf::from("/Library/Developer/CommandLineTools/usr/bin");
        if clt_usr_bin.exists() {
            spawn_search_dirs.push(clt_usr_bin);
        }

        // Rustup toolchain bin dirs (Issue 17): the `cargo`/`rustc`/etc.
        // shims installed in `~/.cargo/bin` are rustup PROXIES that re-exec
        // the active toolchain's real binary under
        // `<rustup_home>/toolchains/<toolchain>/bin/` at runtime. Seatbelt
        // must see that real exec path too, so search every toolchain's
        // `bin` dir directly (existence-checked, no subprocess spawned).
        let rustup_home = std::env::var_os("RUSTUP_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".rustup"));
        if let Ok(entries) = std::fs::read_dir(rustup_home.join("toolchains")) {
            for entry in entries.flatten() {
                let bin_dir = entry.path().join("bin");
                if bin_dir.is_dir() {
                    spawn_search_dirs.push(bin_dir);
                }
            }
        }

        // Resolve each allowlisted binary name to EVERY absolute,
        // canonicalized, executable path it matches (Issue 17): a bare name
        // may resolve to multiple literal paths (e.g. the same binary name
        // present under both Homebrew and the developer tools), and all of
        // them must be granted so the resolved path matches whichever one
        // actually executes at spawn time. An entry that names an absolute
        // path is canonicalized and kept only if executable — no directory
        // search. Entries that resolve to nothing are dropped with a
        // warning — fail-closed for that one binary, never fail-open for
        // the profile. Canonicalization failures are dropped the same way
        // (Issue 16 discipline: never emit an unresolvable grant).
        let spawn_mode = ent.processes.spawn.mode;
        let mut spawn_allowed_paths: Vec<PathBuf> = Vec::new();
        // fleet_run: the child is the `mur` binary itself — chain it into the
        // allowlist resolution below (same resolve/canonicalize/dedup path).
        let mut spawn_names: Vec<String> = ent.processes.spawn.allowed.clone();
        if fleet_run_enabled && !spawn_names.iter().any(|n| n == "mur") {
            spawn_names.push("mur".to_string());
        }
        for name in &spawn_names {
            let mut matched_any = false;
            if name.contains(std::path::MAIN_SEPARATOR) {
                let candidate = Path::new(name);
                if is_executable_file(candidate)
                    && let Ok(canon) = std::fs::canonicalize(candidate)
                {
                    matched_any = true;
                    let differs = canon != candidate;
                    if !spawn_allowed_paths.contains(&canon) {
                        spawn_allowed_paths.push(canon);
                    }
                    // A relocated-home ancestor (e.g. a symlinked package
                    // dir) means the exec path Seatbelt actually checks at
                    // spawn time may be either the original or the
                    // canonical form — keep both (dedup above/below still
                    // applies to each individually).
                    if differs && !spawn_allowed_paths.contains(&candidate.to_path_buf()) {
                        spawn_allowed_paths.push(candidate.to_path_buf());
                    }
                }
            } else {
                for dir in &spawn_search_dirs {
                    let candidate = dir.join(name);
                    if !is_executable_file(&candidate) {
                        continue;
                    }
                    match std::fs::canonicalize(&candidate) {
                        Ok(canon) => {
                            matched_any = true;
                            let differs = canon != candidate;
                            if !spawn_allowed_paths.contains(&canon) {
                                spawn_allowed_paths.push(canon);
                            }
                            // See the relocated-home comment above: keep
                            // both forms when they differ.
                            if differs && !spawn_allowed_paths.contains(&candidate) {
                                spawn_allowed_paths.push(candidate.clone());
                            }
                        }
                        Err(err) => {
                            tracing::warn!(
                                binary = %name,
                                path = %candidate.display(),
                                error = %err,
                                "spawn allowlist candidate could not be canonicalized; \
                                 dropping this match"
                            );
                        }
                    }
                }
            }
            if !matched_any {
                tracing::warn!(
                    binary = %name,
                    "spawn allowlist entry could not be resolved to an executable; dropping"
                );
            }
        }

        // Strict-mode shell guarantee: the runtime, not the profile author,
        // is responsible for keeping the bash TOOL functional once the
        // system exec-path exemption is fenced off. Resolve the same
        // `bash` the bash tool itself spawns (see `tools/bash.rs` --
        // `Command::new("bash")`, a PATH lookup) by searching the same
        // `spawn_search_dirs` used for allowlist resolution above --
        // on macOS this lands on `/bin/bash` -- and push its canonicalized
        // path into `spawn_allowed_paths` automatically. Strict contract:
        // the bash tool can still launch its shell; nothing else is
        // implied -- every other system binary stays fenced.
        if spawn_mode == SpawnMode::Strict {
            for dir in &spawn_search_dirs {
                let candidate = dir.join("bash");
                if !is_executable_file(&candidate) {
                    continue;
                }
                if let Ok(canon) = std::fs::canonicalize(&candidate) {
                    if !spawn_allowed_paths.contains(&canon) {
                        spawn_allowed_paths.push(canon.clone());
                    }
                    if canon != candidate && !spawn_allowed_paths.contains(&candidate) {
                        spawn_allowed_paths.push(candidate.clone());
                    }
                    break;
                }
            }
        }

        // Derive the prefix grants from the resolved literals (Issue 17):
        // see the `spawn_allowed_prefixes` field doc for the parent/
        // grandparent-if-`bin` rule and the guard list.
        let mut spawn_allowed_prefixes: Vec<PathBuf> = Vec::new();
        for literal in &spawn_allowed_paths {
            let prefix = compute_spawn_prefix(literal, &home);
            if prefix.exists() && !spawn_allowed_prefixes.contains(&prefix) {
                spawn_allowed_prefixes.push(prefix);
            }
        }

        let (net_allow_ports, net_allow_hosts, net_loopback_allowed) =
            match ent.network.outbound.mode {
                NetworkOutboundMode::Unrestricted => (None, None, false),
                NetworkOutboundMode::Restricted => {
                    let ports = Some(RESTRICTED_GENERAL_PORTS.to_vec());
                    let hosts = Some(ent.network.outbound.allow_hosts.clone());
                    (ports, hosts, true)
                }
                NetworkOutboundMode::ProxyOnly => {
                    // Deny general TCP (empty-but-present list), keep the host
                    // allowlist so the runtime's own client can resolve its
                    // loopback LLM endpoint. Loopback carve-outs are added by the
                    // port-assembly helpers; net_loopback_allowed = true is what
                    // lets them fire despite the empty general list.
                    (
                        Some(vec![]),
                        Some(ent.network.outbound.allow_hosts.clone()),
                        true,
                    )
                }
                NetworkOutboundMode::Off => (Some(vec![]), Some(vec![]), false),
            };

        SandboxPolicy {
            fs_read,
            fs_write,
            fs_deny,
            fs_exec,
            spawn_mode,
            spawn_allowed_paths,
            spawn_allowed_prefixes,
            net_allow_ports,
            net_allow_loopback_ports: Vec::new(),
            net_loopback_allowed,
            net_allow_hosts,
            memory_limit_mb: Some(ent.limits.memory_mb),
        }
    }

    /// Grant outbound to `extra` TCP ports for the agent's own LLM endpoint.
    /// Under Restricted (non-empty general list) they join the general list
    /// (`*:port`). Under ProxyOnly (general list present-but-empty AND
    /// `net_loopback_allowed`) the LLM is a loopback cc-proxy, so route its port
    /// to the loopback carve-out instead of opening a general `*:port`. No-op
    /// under Off (empty list, flag false) and Unrestricted (`None`).
    pub fn allow_extra_ports(&mut self, extra: &[u16]) {
        match self.net_allow_ports.as_ref().map(|p| p.is_empty()) {
            Some(false) => {
                if let Some(ports) = &mut self.net_allow_ports {
                    for p in extra {
                        if !ports.contains(p) {
                            ports.push(*p);
                        }
                    }
                }
            }
            Some(true) if self.net_loopback_allowed => {
                for p in extra {
                    if !self.net_allow_loopback_ports.contains(p) {
                        self.net_allow_loopback_ports.push(*p);
                    }
                }
            }
            _ => {}
        }
    }

    /// Grant loopback-only access to `extra` TCP ports (the egress proxy, and —
    /// under ProxyOnly — the LLM cc-proxy). Fires for Restricted (non-empty
    /// general list) and ProxyOnly (`net_loopback_allowed`); a no-op under Off
    /// (empty list, flag false) and Unrestricted (`None`).
    pub fn allow_loopback_ports(&mut self, extra: &[u16]) {
        let permitted =
            matches!(&self.net_allow_ports, Some(p) if !p.is_empty()) || self.net_loopback_allowed;
        if permitted {
            for p in extra {
                if !self.net_allow_loopback_ports.contains(p) {
                    self.net_allow_loopback_ports.push(*p);
                }
            }
        }
    }

    /// Reopen the Restricted general-port set on the RUNTIME's own (self)
    /// profile when it hosts the in-process egress proxy. Under ProxyOnly the
    /// worker's entitlements deny all general TCP — but the egress proxy runs
    /// inside the runtime process, so that deny also killed the proxy's
    /// UPSTREAM dials (`TcpStream::connect` → EPERM, os error 1) and every
    /// sandboxed child's granted egress died after `CONNECT ALLOW`. The child
    /// profiles are built separately (`sandbox::child::spawn_sandboxed`) and
    /// keep the strict ProxyOnly deny, so the choke point for untrusted MCP
    /// children is unchanged; the runtime's own LLM client remains
    /// HostGuard-gated exactly as under Restricted. No-op for Off (air-gapped
    /// stays air-gapped), Restricted, and Unrestricted.
    pub fn allow_in_process_proxy_upstream(&mut self) {
        if matches!(&self.net_allow_ports, Some(p) if p.is_empty()) && self.net_loopback_allowed {
            self.net_allow_ports = Some(RESTRICTED_GENERAL_PORTS.to_vec());
        }
    }

    /// Grant write access to additional paths the runtime owns but that live
    /// outside `agent_home` — e.g. the shared `~/.mur/runtime` media state
    /// (`watch.json`, VLC snapshot dir) that the co-watching scheduler must
    /// persist to and clean up. Idempotent.
    pub fn allow_extra_write_paths(&mut self, paths: &[PathBuf]) {
        for p in paths {
            if !self.fs_write.contains(p) {
                self.fs_write.push(p.clone());
            }
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
fn system_exec_paths(home: &Path) -> Vec<PathBuf> {
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

#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, InboundNetwork, NetworkEntitlement,
        NetworkOutboundMode, OutboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
    };

    fn minimal_entitlements() -> Entitlements {
        Entitlements {
            network: NetworkEntitlement {
                inbound: InboundNetwork { ports: vec![] },
                outbound: OutboundNetwork {
                    mode: NetworkOutboundMode::Restricted,
                    allow_hosts: vec!["api.anthropic.com".to_string()],
                    protocols: vec!["tcp".to_string()],
                    resolve_dns: Default::default(),
                },
            },
            filesystem: FilesystemEntitlement {
                read: vec!["~/Documents".to_string()],
                write: vec!["~/Downloads".to_string()],
                deny: vec!["~/.ssh".to_string()],
            },
            processes: ProcessesEntitlement {
                spawn: SpawnEntitlement {
                    mode: SpawnMode::Allowlist,
                    allowed: vec![],
                },
            },
            syscalls: Default::default(),
            limits: Default::default(),
            llm: Default::default(),
            tools: vec![],
            fail_closed_on_sandbox_error: true,
        }
    }

    #[test]
    fn agent_home_always_in_write() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        assert!(policy.fs_write.contains(&home));
    }

    #[test]
    fn own_profile_and_identity_key_always_denied() {
        // Issue #712: even a broad write grant covering the agents root (or
        // the agent's own dir) must not make the agent's profile.yaml /
        // identity.key writable — the self-escalation loop (self-edit +
        // self-restart) stays closed regardless of entitlements.
        let tmp = tempfile::tempdir().expect("tempdir");
        let mur_home = tmp.path();
        let agent_home = mur_home.join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        let mut ent = minimal_entitlements();
        ent.filesystem.write = vec![
            mur_home.join("agents").to_string_lossy().into_owned(),
            agent_home.to_string_lossy().into_owned(),
        ];
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
        for f in SELF_PROTECTED_AGENT_FILES {
            assert!(
                policy.fs_deny.contains(&agent_home.join(f)),
                "{f} must always be in fs_deny"
            );
        }
        // Only those two files are denied — the rest of agent_home
        // (running.lock, running.sentinel, stderr.log) stays writable.
        assert!(policy.fs_write.contains(&agent_home));
    }

    #[test]
    fn channels_dir_always_in_write() {
        // agent_home = <mur_home>/agents/<name> → channels = <mur_home>/channels,
        // granted even though minimal_entitlements lists no write paths.
        let home = PathBuf::from("/tmp/mur/agents/rs");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        assert!(
            policy
                .fs_write
                .contains(&PathBuf::from("/tmp/mur/channels"))
        );
    }

    #[test]
    fn open_items_log_is_writable_so_the_built_in_tool_works() {
        // The `open_item` tool has exactly one write target and no entitlement
        // of its own. Without this grant, "record a todo" failed for every
        // sandboxed agent while the tool's own docs said it needed nothing.
        let tmp = tempfile::tempdir().expect("tempdir");
        let mur_home = tmp.path();
        let agent_home = mur_home.join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
        let log = mur_home.join(mur_open_items::LOG_FILE);
        assert!(policy.fs_write.contains(&log), "{:?}", policy.fs_write);
        // Created up front: a rule on a path that does not exist yet is
        // dropped at seal time, and the log is append-only from absent.
        assert!(log.exists());
    }

    #[test]
    fn fleet_run_carveins_are_config_gated() {
        // Not allowlisted (no config.yaml) → no fleets/commander/conversations grants.
        let tmp = tempfile::tempdir().expect("tempdir");
        let mur_home = tmp.path();
        let agent_home = mur_home.join("agents").join("mur");
        std::fs::create_dir_all(&agent_home).unwrap();
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
        assert!(!policy.fs_write.contains(&mur_home.join("fleets")));

        // Allowlisted in config.yaml → the three dirs are carved in.
        std::fs::write(
            mur_home.join("config.yaml"),
            "fleet_run:\n  agents: [mur]\n  fleets: [deep-research]\n",
        )
        .unwrap();
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
        for dir in ["fleets", "commander", "conversations"] {
            assert!(
                policy.fs_write.contains(&mur_home.join(dir)),
                "{dir} should be carved in for an allowlisted agent"
            );
        }
        // A different (non-allowlisted) agent stays denied.
        let other_home = mur_home.join("agents").join("dr_worker_1");
        std::fs::create_dir_all(&other_home).unwrap();
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &other_home);
        assert!(!policy.fs_write.contains(&mur_home.join("fleets")));
    }

    #[test]
    fn tilde_expands_to_home_dir() {
        // Exercised via `deny` rather than `read`/`write`: deny entries are
        // exempt from the dead-grant existence filter by design (a stale
        // deny path is kept verbatim rather than dropped, since dropping it
        // would be fail-open). `~/Documents` may not exist on CI runners
        // (e.g. Ubuntu, no home Documents dir), so asserting through `read`
        // makes this test's outcome depend on runner environment. Routing
        // it through `deny` still exercises the same `expand` tilde
        // substitution logic while staying environment-independent.
        let mut ent = minimal_entitlements();
        ent.filesystem.deny.push("~/Documents".to_string());
        let agent_home = PathBuf::from("/tmp/agent_home_test");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
        let expected = dirs::home_dir().unwrap().join("Documents");
        assert!(
            policy.fs_deny.contains(&expected),
            "~/Documents should expand to {expected:?}, got: {:?}",
            policy.fs_deny
        );
    }

    #[test]
    fn deny_paths_propagated() {
        let agent_home = PathBuf::from("/tmp/agent_home_test");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &agent_home);
        let expected = dirs::home_dir().unwrap().join(".ssh");
        assert!(
            policy.fs_deny.contains(&expected),
            "~/.ssh should expand to {expected:?}, got: {:?}",
            policy.fs_deny
        );
    }

    #[test]
    fn restricted_mode_populates_allow_hosts() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        assert_eq!(
            policy.net_allow_hosts,
            Some(vec!["api.anthropic.com".to_string()])
        );
    }

    #[test]
    fn unrestricted_mode_allows_all_hosts() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::Unrestricted;
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&ent, &home);
        assert_eq!(policy.net_allow_hosts, None);
    }

    #[test]
    fn off_mode_blocks_all_hosts() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::Off;
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&ent, &home);
        assert_eq!(policy.net_allow_hosts, Some(vec![]));
    }

    #[test]
    fn proxy_only_denies_general_tcp_but_keeps_host_allowlist() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::ProxyOnly;
        ent.network.outbound.allow_hosts = vec!["localhost".into(), "127.0.0.1".into()];
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&ent, &home);
        // General TCP denied (empty port list, but PRESENT — not None/unrestricted).
        assert_eq!(policy.net_allow_ports, Some(vec![]));
        // Host allowlist retained (NOT emptied like Off) so the LLM host resolves.
        assert_eq!(
            policy.net_allow_hosts,
            Some(vec!["localhost".to_string(), "127.0.0.1".to_string()])
        );
        // Flag set so the port helpers add loopback carve-outs (distinguishes
        // ProxyOnly's Some([]) from Off's Some([]), which leaves this false).
        assert!(policy.net_loopback_allowed);
    }

    #[test]
    fn proxy_only_self_profile_widens_for_in_process_proxy_upstream() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::ProxyOnly;
        let home = PathBuf::from("/tmp/agent_home");
        let mut policy = SandboxPolicy::from_entitlements(&ent, &home);
        policy.allow_in_process_proxy_upstream();
        // The runtime SELF profile regains the Restricted general-port set so
        // the in-process egress proxy can dial upstream (os error 1 fix);
        // child profiles are built separately and stay ProxyOnly-strict.
        assert_eq!(
            policy.net_allow_ports,
            Some(RESTRICTED_GENERAL_PORTS.to_vec())
        );
    }

    #[test]
    fn off_mode_is_never_widened_by_proxy_upstream() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::Off;
        let home = PathBuf::from("/tmp/agent_home");
        let mut policy = SandboxPolicy::from_entitlements(&ent, &home);
        policy.allow_in_process_proxy_upstream();
        // Off is air-gapped: no proxy carve-out may reopen it.
        assert_eq!(policy.net_allow_ports, Some(vec![]));
    }

    #[test]
    fn restricted_and_unrestricted_unchanged_by_proxy_upstream() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::Restricted;
        let home = PathBuf::from("/tmp/agent_home");
        let mut policy = SandboxPolicy::from_entitlements(&ent, &home);
        policy.allow_in_process_proxy_upstream();
        assert_eq!(
            policy.net_allow_ports,
            Some(RESTRICTED_GENERAL_PORTS.to_vec())
        );

        ent.network.outbound.mode = NetworkOutboundMode::Unrestricted;
        let mut policy = SandboxPolicy::from_entitlements(&ent, &home);
        policy.allow_in_process_proxy_upstream();
        assert_eq!(policy.net_allow_ports, None);
    }

    #[test]
    fn allow_extra_ports_adds_llm_port_in_restricted_mode() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::Restricted;
        let mut policy = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
        policy.allow_extra_ports(&[11434]);
        let ports = policy.net_allow_ports.unwrap();
        assert!(
            ports.contains(&11434),
            "ollama port must be granted: {ports:?}"
        );
        // Idempotent — re-adding doesn't duplicate.
        let mut p2 = SandboxPolicy::from_entitlements(&ent, &PathBuf::from("/tmp/a"));
        p2.allow_extra_ports(&[443]);
        assert_eq!(
            p2.net_allow_ports
                .unwrap()
                .iter()
                .filter(|&&p| p == 443)
                .count(),
            1
        );
    }

    #[test]
    fn allow_extra_ports_respects_off_and_unrestricted() {
        // Off mode (Some([])) must NOT be silently re-opened.
        let mut off = minimal_entitlements();
        off.network.outbound.mode = NetworkOutboundMode::Off;
        let mut p_off = SandboxPolicy::from_entitlements(&off, &PathBuf::from("/tmp/a"));
        p_off.allow_extra_ports(&[11434]);
        assert_eq!(p_off.net_allow_ports, Some(vec![]));

        // Unrestricted (None) already allows everything — stays None.
        let mut unr = minimal_entitlements();
        unr.network.outbound.mode = NetworkOutboundMode::Unrestricted;
        let mut p_unr = SandboxPolicy::from_entitlements(&unr, &PathBuf::from("/tmp/a"));
        p_unr.allow_extra_ports(&[11434]);
        assert_eq!(p_unr.net_allow_ports, None);
    }

    #[test]
    fn proxy_only_port_assembly_routes_llm_and_egress_to_loopback() {
        let mut policy = SandboxPolicy::default();
        policy.net_allow_ports = Some(Vec::new()); // ProxyOnly: deny general TCP
        policy.net_loopback_allowed = true; // …but loopback carve-outs permitted
        // LLM port (cc-proxy) must land in LOOPBACK, not general ports.
        policy.allow_extra_ports(&[8088]);
        // Egress proxy port must be accepted even though general list is empty.
        policy.allow_loopback_ports(&[54321]);
        assert_eq!(
            policy.net_allow_ports,
            Some(vec![]),
            "general TCP stays denied"
        );
        assert!(
            policy.net_allow_loopback_ports.contains(&8088),
            "LLM port routed to loopback"
        );
        assert!(
            policy.net_allow_loopback_ports.contains(&54321),
            "egress proxy port accepted"
        );
    }

    #[test]
    fn off_mode_port_assembly_stays_empty() {
        // Off is ALSO net_allow_ports = Some([]), but net_loopback_allowed = false
        // (the default) — neither helper may re-open loopback. This is the guard the
        // flag exists for.
        let mut policy = SandboxPolicy::default();
        policy.net_allow_ports = Some(Vec::new());
        // net_loopback_allowed left false
        policy.allow_extra_ports(&[8088]);
        policy.allow_loopback_ports(&[54321]);
        assert!(
            policy.net_allow_loopback_ports.is_empty(),
            "Off adds no loopback carve-outs"
        );
    }

    #[test]
    fn restricted_port_assembly_unchanged() {
        // Non-empty general list = generic Restricted: LLM port still goes to
        // net_allow_ports (unchanged behavior).
        let mut policy = SandboxPolicy::default();
        policy.net_allow_ports = Some(vec![80, 443]);
        policy.allow_extra_ports(&[8088]);
        assert!(policy.net_allow_ports.as_ref().unwrap().contains(&8088));
        assert!(!policy.net_allow_loopback_ports.contains(&8088));
    }

    #[test]
    fn allow_extra_write_paths_adds_and_dedups() {
        let ent = minimal_entitlements();
        let agent_home = PathBuf::from("/tmp/a");
        let mut policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
        let extra = PathBuf::from("/home/u/.mur/runtime/vlc-snapshots");
        policy.allow_extra_write_paths(std::slice::from_ref(&extra));
        assert!(policy.fs_write.contains(&extra));
        // Idempotent — re-adding doesn't duplicate.
        policy.allow_extra_write_paths(std::slice::from_ref(&extra));
        assert_eq!(policy.fs_write.iter().filter(|p| **p == extra).count(), 1);
    }

    #[cfg(not(target_os = "windows"))]
    fn make_fake_executable(dir: &Path, name: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").expect("write fake executable");
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).expect("chmod fake executable");
        path
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn resolve_binary_path_finds_executable_in_fs_exec_dir() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin_path = make_fake_executable(tmp.path(), "fake-tool");

        let resolved = resolve_binary_path("fake-tool", &[tmp.path().to_path_buf()]);
        assert_eq!(resolved, Some(bin_path));
    }

    #[test]
    fn resolve_binary_path_drops_missing_binary_without_panic() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let resolved =
            resolve_binary_path("definitely-does-not-exist", &[tmp.path().to_path_buf()]);
        assert_eq!(resolved, None);
    }

    #[test]
    fn from_entitlements_drops_dead_read_write_but_keeps_dead_deny() {
        // Issue 16 regression: a user-declared fs_read/fs_write entitlement
        // path that does not exist on disk (e.g. a removed git worktree)
        // must be dropped at profile-build time rather than emitted as a
        // dead SBPL `subpath` grant — a dead grant there was observed to
        // destabilize other, unrelated file-write* checks under the same
        // compiled sandbox policy (30s tool-call hangs, not EPERM). A dead
        // `fs_deny` entry, by contrast, must be KEPT verbatim: dropping it
        // would be fail-OPEN if the path later reappears.
        let tmp = tempfile::tempdir().expect("tempdir");
        let live_dir = tmp.path().join("live");
        std::fs::create_dir(&live_dir).expect("mkdir live");
        let dead_dir = tmp.path().join("dead");
        std::fs::create_dir(&dead_dir).expect("mkdir dead");
        std::fs::remove_dir(&dead_dir).expect("rmdir dead (now nonexistent)");

        let mut ent = minimal_entitlements();
        ent.filesystem.read = vec![
            live_dir.to_string_lossy().to_string(),
            dead_dir.to_string_lossy().to_string(),
        ];
        ent.filesystem.write = vec![
            live_dir.to_string_lossy().to_string(),
            dead_dir.to_string_lossy().to_string(),
        ];
        // Same dead path, but declared as a deny entry: must survive.
        ent.filesystem.deny = vec![dead_dir.to_string_lossy().to_string()];

        // Nest agent_home two levels inside the tempdir so the derived
        // channels dir (`agent_home.parent().parent()/channels`) stays
        // inside the tempdir too, rather than touching the real system /tmp.
        let agent_home = tmp.path().join("agents").join("dead-grant-test");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

        assert!(
            policy.fs_read.contains(&live_dir),
            "live read path must be kept: {:?}",
            policy.fs_read
        );
        assert!(
            !policy.fs_read.contains(&dead_dir),
            "dead read path must be dropped: {:?}",
            policy.fs_read
        );
        assert!(
            policy.fs_write.contains(&live_dir),
            "live write path must be kept: {:?}",
            policy.fs_write
        );
        assert!(
            !policy.fs_write.contains(&dead_dir),
            "dead write path must be dropped: {:?}",
            policy.fs_write
        );
        assert!(
            policy.fs_deny.contains(&dead_dir),
            "dead deny path must be KEPT verbatim (dropping would be fail-open): {:?}",
            policy.fs_deny
        );

        #[cfg(target_os = "macos")]
        {
            let sbpl = crate::sandbox::macos::build_sbpl_profile(&policy);
            let live_p = live_dir.to_string_lossy();
            let dead_p = dead_dir.to_string_lossy();
            assert!(
                sbpl.contains(&format!("(allow file-write* (subpath \"{live_p}\"))")),
                "SBPL must contain an allow-write subpath for the live path"
            );
            assert!(
                !sbpl.contains(&format!("(allow file-write* (subpath \"{dead_p}\"))")),
                "SBPL must NOT contain an allow-write subpath for the dropped dead path"
            );
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn from_entitlements_resolves_spawn_allowed_and_drops_unresolved() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let bin_path = make_fake_executable(tmp.path(), "fake-tool");

        let mut ent = minimal_entitlements();
        ent.processes.spawn.mode = SpawnMode::Allowlist;
        ent.processes.spawn.allowed = vec![
            bin_path.to_string_lossy().to_string(),
            "definitely-does-not-exist".to_string(),
        ];

        let agent_home = PathBuf::from("/tmp/agent_home_spawn");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

        assert_eq!(policy.spawn_mode, SpawnMode::Allowlist);
        let expected_canonical_bin =
            std::fs::canonicalize(&bin_path).expect("canonicalize fake tool");
        // Tempdirs may sit behind a symlinked ancestor (e.g. /var ->
        // /private/var on macOS), in which case BOTH the original and
        // canonical forms are kept. Assert the canonical form is present and
        // that no unrelated binary name leaked in.
        assert!(
            policy.spawn_allowed_paths.contains(&expected_canonical_bin),
            "spawn_allowed_paths must contain the canonical fake tool path: {:?}",
            policy.spawn_allowed_paths
        );
        assert!(
            policy
                .spawn_allowed_paths
                .iter()
                .all(|p| p.ends_with("fake-tool")),
            "no unrelated binary name should have leaked into spawn_allowed_paths: {:?}",
            policy.spawn_allowed_paths
        );
    }

    #[test]
    #[cfg(unix)]
    fn from_entitlements_resolves_symlinked_spawn_entry_to_canonical_prefix() {
        // Issue 17: an allowlist entry may be an absolute path to a symlink
        // (e.g. a shim/wrapper) rather than the real binary. The resolved
        // literal must be the CANONICAL target, and the derived prefix must
        // be computed from that canonical path — a `<pkg>/<version>/bin/tool`
        // layout should yield `<pkg>/<version>` as the prefix (grandparent,
        // since the parent is named `bin`), not the shim's own directory.
        let tmp = tempfile::tempdir().expect("tempdir");

        let real_bin_dir = tmp.path().join("pkg").join("1.0").join("bin");
        std::fs::create_dir_all(&real_bin_dir).expect("mkdir real bin dir");
        let real_tool = make_fake_executable(&real_bin_dir, "tool");

        let shim_dir = tmp.path().join("shim-bin");
        std::fs::create_dir_all(&shim_dir).expect("mkdir shim dir");
        let shim_tool = shim_dir.join("tool");
        std::os::unix::fs::symlink(&real_tool, &shim_tool).expect("symlink shim -> real tool");

        let mut ent = minimal_entitlements();
        ent.processes.spawn.mode = SpawnMode::Allowlist;
        ent.processes.spawn.allowed = vec![shim_tool.to_string_lossy().to_string()];

        let agent_home = tmp.path().join("agents").join("symlink-test");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

        let expected_canonical = std::fs::canonicalize(&real_tool).expect("canonicalize real tool");
        assert!(
            policy.spawn_allowed_paths.contains(&expected_canonical),
            "spawn_allowed_paths must contain the canonical target of the symlink: {:?}",
            policy.spawn_allowed_paths
        );

        let expected_prefix = std::fs::canonicalize(tmp.path().join("pkg").join("1.0"))
            .expect("canonicalize pkg/1.0");
        assert!(
            policy.spawn_allowed_prefixes.contains(&expected_prefix),
            "spawn_allowed_prefixes must contain the grandparent pkg/1.0 dir \
             (parent is `bin`): {:?}",
            policy.spawn_allowed_prefixes
        );
    }

    #[test]
    #[cfg(unix)]
    fn from_entitlements_spawn_prefix_is_immediate_parent_when_not_under_bin() {
        // When the binary's parent directory is NOT named `bin`, the prefix
        // must be that immediate parent itself (no grandparent hop, no
        // broad-root guard triggered) — deterministic, without needing to
        // fake a real filesystem root or /usr.
        let tmp = tempfile::tempdir().expect("tempdir");
        let just_a_dir = tmp.path().join("just-a-dir");
        std::fs::create_dir_all(&just_a_dir).expect("mkdir just-a-dir");
        let tool_path = make_fake_executable(&just_a_dir, "tool");

        let mut ent = minimal_entitlements();
        ent.processes.spawn.mode = SpawnMode::Allowlist;
        ent.processes.spawn.allowed = vec![tool_path.to_string_lossy().to_string()];

        let agent_home = tmp.path().join("agents").join("non-bin-prefix-test");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

        let expected_prefix = std::fs::canonicalize(&just_a_dir).expect("canonicalize just-a-dir");
        // Tempdirs may sit behind a symlinked ancestor (e.g. /var ->
        // /private/var on macOS), in which case BOTH the original and
        // canonical forms are kept. Assert the canonical form is present and
        // that no unrelated directory name leaked in.
        assert!(
            policy.spawn_allowed_prefixes.contains(&expected_prefix),
            "prefixes must contain the canonical just-a-dir path: {:?}",
            policy.spawn_allowed_prefixes
        );
        assert!(
            policy
                .spawn_allowed_prefixes
                .iter()
                .all(|p| p.ends_with("just-a-dir")),
            "no unrelated directory should have leaked into spawn_allowed_prefixes: {:?}",
            policy.spawn_allowed_prefixes
        );
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn from_entitlements_empty_spawn_allowlist_yields_empty_paths_and_prefixes() {
        let mut ent = minimal_entitlements();
        ent.processes.spawn.mode = SpawnMode::Allowlist;
        ent.processes.spawn.allowed = vec![];

        let agent_home = PathBuf::from("/tmp/agent_home_empty_spawn");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

        assert_eq!(policy.spawn_mode, SpawnMode::Allowlist);
        assert!(
            policy.spawn_allowed_paths.is_empty(),
            "empty allowlist must yield no resolved paths: {:?}",
            policy.spawn_allowed_paths
        );
        assert!(
            policy.spawn_allowed_prefixes.is_empty(),
            "empty allowlist must yield no derived prefixes: {:?}",
            policy.spawn_allowed_prefixes
        );
    }

    #[test]
    #[cfg(unix)]
    fn strict_mode_seeds_shell_into_spawn_allowed() {
        // Decision (i): in Strict mode the runtime itself guarantees the
        // bash TOOL stays functional by resolving the same `bash` binary
        // `tools/bash.rs` spawns (a PATH lookup) and auto-seeding its
        // canonical path into `spawn_allowed_paths` -- even when the
        // profile author declared an empty allowlist.
        let mut ent = minimal_entitlements();
        ent.processes.spawn.mode = SpawnMode::Strict;
        ent.processes.spawn.allowed = vec![];

        let agent_home = PathBuf::from("/tmp/agent_home_strict_shell_seed");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

        assert_eq!(policy.spawn_mode, SpawnMode::Strict);
        assert!(
            policy
                .spawn_allowed_paths
                .iter()
                .any(|p| p.ends_with("bash")),
            "strict mode must auto-seed a resolved bash path even with an \
             empty allowlist: {:?}",
            policy.spawn_allowed_paths
        );
        // On a normal macOS/unix host `bash` resolves via PATH to the
        // canonical system shell at /bin/bash.
        let canonical_bash = std::fs::canonicalize("/bin/bash");
        if let Ok(canonical_bash) = canonical_bash {
            assert!(
                policy.spawn_allowed_paths.contains(&canonical_bash),
                "expected the canonical /bin/bash to be seeded into \
                 spawn_allowed_paths: {:?}",
                policy.spawn_allowed_paths
            );
        }
    }

    #[test]
    #[cfg(not(target_os = "windows"))]
    fn fake_rustup_toolchain_bin_is_searched() {
        // Issue 17: `~/.cargo/bin/cargo` is a rustup PROXY that re-execs the
        // real binary under `<rustup_home>/toolchains/<toolchain>/bin/` at
        // runtime — that real exec path must be discoverable via a bare
        // `cargo` allowlist entry, without a real rustup install present.
        let tmp = tempfile::tempdir().expect("tempdir");
        let toolchain_bin = tmp.path().join("toolchains").join("tc1").join("bin");
        std::fs::create_dir_all(&toolchain_bin).expect("mkdir toolchain bin dir");
        let cargo_path = make_fake_executable(&toolchain_bin, "cargo");

        let rustup_home = tmp.path().to_path_buf();
        // SAFETY: set/cleared within this test; no other test in this
        // process reads RUSTUP_HOME concurrently in a way that would race
        // with this value (tests run single-threaded per-process here or
        // isolated via nextest).
        unsafe {
            std::env::set_var("RUSTUP_HOME", &rustup_home);
        }

        let mut ent = minimal_entitlements();
        ent.processes.spawn.mode = SpawnMode::Allowlist;
        ent.processes.spawn.allowed = vec!["cargo".to_string()];

        let agent_home = tmp.path().join("agents").join("rustup-test");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

        // SAFETY: paired with the set_var above; always run even if an
        // assertion below panics would be nicer, but this mirrors the
        // existing set_var/remove_var pattern used elsewhere in this repo
        // (see llm::tests).
        unsafe {
            std::env::remove_var("RUSTUP_HOME");
        }

        let expected_canonical =
            std::fs::canonicalize(&cargo_path).expect("canonicalize toolchain cargo");
        assert!(
            policy.spawn_allowed_paths.contains(&expected_canonical),
            "spawn_allowed_paths must contain the rustup toolchain's cargo: {:?}",
            policy.spawn_allowed_paths
        );
    }

    #[test]
    #[cfg(unix)]
    fn both_path_forms_kept_for_symlinked_ancestor() {
        // Issue 17: when an ANCESTOR directory of an allowlisted absolute
        // path is a symlink (not the file itself), Seatbelt's exec-path
        // check may observe either the original (symlink-form) path or the
        // canonicalized one depending on how the process is launched — both
        // forms must be granted.
        let tmp = tempfile::tempdir().expect("tempdir");

        let real_bin_dir = tmp.path().join("pkg").join("bin");
        std::fs::create_dir_all(&real_bin_dir).expect("mkdir real bin dir");
        let real_tool = make_fake_executable(&real_bin_dir, "tool");

        let link_pkg = tmp.path().join("link-pkg");
        std::os::unix::fs::symlink(tmp.path().join("pkg"), &link_pkg)
            .expect("symlink link-pkg -> pkg");

        let symlink_form_tool = tmp.path().join("link-pkg").join("bin").join("tool");

        let mut ent = minimal_entitlements();
        ent.processes.spawn.mode = SpawnMode::Allowlist;
        ent.processes.spawn.allowed = vec![symlink_form_tool.to_string_lossy().to_string()];

        let agent_home = tmp.path().join("agents").join("symlink-ancestor-test");
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);

        let expected_canonical = std::fs::canonicalize(&real_tool).expect("canonicalize real tool");
        // The symlink-form entry stored in `spawn_allowed_paths` is the
        // ORIGINAL entitlement literal, reconstructed verbatim from the
        // string via `Path::new` — never itself canonicalized (only the
        // resolved `canon` value is). `symlink_form_tool` was built the
        // same way (joined on `tmp.path()` as returned, before any
        // canonicalization), so it is the exact expected value — no need
        // to canonicalize `tmp.path()` here, since doing so would collapse
        // the `link-pkg` symlink hop this test specifically exercises.
        assert!(
            policy.spawn_allowed_paths.contains(&symlink_form_tool),
            "spawn_allowed_paths must contain the symlink-form path: {:?}",
            policy.spawn_allowed_paths
        );
        assert!(
            policy.spawn_allowed_paths.contains(&expected_canonical),
            "spawn_allowed_paths must contain the canonical form: {:?}",
            policy.spawn_allowed_paths
        );
    }

    #[test]
    fn loopback_ports_respect_off_mode() {
        // Off = user denied all outbound; the carve-out must not reopen it.
        let mut p_off = SandboxPolicy {
            net_allow_ports: Some(vec![]),
            ..Default::default()
        };
        p_off.allow_loopback_ports(&[54321]);
        assert!(p_off.net_allow_loopback_ports.is_empty());

        // Restricted: carve-out applies, deduplicated.
        let mut p_r = SandboxPolicy {
            net_allow_ports: Some(vec![80, 443, 8080, 8443]),
            ..Default::default()
        };
        p_r.allow_loopback_ports(&[54321]);
        p_r.allow_loopback_ports(&[54321]);
        assert_eq!(p_r.net_allow_loopback_ports, vec![54321]);

        // Unrestricted (None): (allow default) already covers it; no rule needed.
        let mut p_u = SandboxPolicy {
            net_allow_ports: None,
            ..Default::default()
        };
        p_u.allow_loopback_ports(&[54321]);
        assert!(p_u.net_allow_loopback_ports.is_empty());
    }

    #[test]
    fn connect_tcp_ports_proxy_only_is_loopback_only() {
        // ProxyOnly: empty general list + loopback ports → only the loopback ports
        // get ConnectTcp rules (general egress e.g. 443 is default-denied).
        let mut policy = SandboxPolicy::default();
        policy.net_allow_ports = Some(Vec::new());
        policy.net_allow_loopback_ports = vec![8088, 54321];
        assert_eq!(connect_tcp_ports(&policy), vec![8088u16, 54321]);
    }

    #[test]
    fn connect_tcp_ports_restricted_is_general_plus_loopback() {
        let mut policy = SandboxPolicy::default();
        policy.net_allow_ports = Some(vec![80, 443]);
        policy.net_allow_loopback_ports = vec![54321];
        assert_eq!(connect_tcp_ports(&policy), vec![80u16, 443, 54321]);
    }

    #[test]
    fn connect_tcp_ports_off_and_unrestricted_are_empty() {
        let mut off = SandboxPolicy::default();
        off.net_allow_ports = Some(Vec::new()); // Off: empty general + empty loopback
        assert!(connect_tcp_ports(&off).is_empty());
        let unr = SandboxPolicy::default(); // Unrestricted: net_allow_ports = None
        assert!(connect_tcp_ports(&unr).is_empty());
    }

    #[test]
    fn channel_index_subdir_granted_not_whole_index_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let mur_home = tmp.path();
        let agent_home = mur_home.join("agents").join("w1");
        std::fs::create_dir_all(&agent_home).unwrap();
        let ent = minimal_entitlements();
        let policy = SandboxPolicy::from_entitlements(&ent, &agent_home);
        assert!(
            policy.fs_write.contains(&mur_home.join("channels")),
            "pre-existing channels carve-out must remain"
        );
        assert!(
            policy
                .fs_write
                .contains(&mur_home.join("index").join("channels")),
            "channels read-model subdir must be granted alongside channels"
        );
        assert!(
            !policy.fs_write.contains(&mur_home.join("index")),
            "the whole index dir must NOT be granted — capabilities.json and \
             the lance stores also live there and must stay unwritable"
        );
        // The grant idiom creates the dir so Landlock rules stick.
        assert!(mur_home.join("index").join("channels").is_dir());
    }
}
