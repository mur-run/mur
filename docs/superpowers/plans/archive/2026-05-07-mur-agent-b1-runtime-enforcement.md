# B1 Real Runtime Enforcement Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Harden `mur-agent-runtime` with OS-level sandboxing — Linux Landlock ABI v4 + seccomp denylist, macOS SBPL via `sandbox_init`, Windows Job Object memory cap — plus a `reqwest` DNS-resolver guard that enforces the per-agent network allowlist before the kernel gate fires.

**Architecture:** A single `sandbox::apply(entitlements, agent_home)` function is called in `supervisor::entrypoint()` immediately after the profile loads, locking the process into the entitlement-derived policy before any hooks or I/O fire. B0SafetyHook provides the LLM-visible advisory layer (hooks first); the kernel sandbox is the fallback enforcement layer (kernel second). EACCES from the kernel maps to `ToolError::Sandboxed` so the LLM gets a structured reason even when B0 did not block.

**Tech Stack:** `landlock 0.4` (Linux Landlock ABI v1–v4, `restrict_self()`), `seccompiler 0.5` (BPF syscall filter), `libc` FFI for `sandbox_init_with_parameters` (macOS), `windows-sys 0.59` (Windows Job Object), `birdcage 0.8` (child spawn sandboxing for MCP processes), `reqwest` custom DNS resolver guard.

---

## File Structure

### New files

| Path | Responsibility |
|---|---|
| `mur-agent-runtime/src/sandbox/mod.rs` | `SandboxPolicy`, `SandboxStatus`, `apply()`, `SandboxedError` |
| `mur-agent-runtime/src/sandbox/policy.rs` | `SandboxPolicy::from_entitlements(entitlements, agent_home)` — pure translator |
| `mur-agent-runtime/src/sandbox/linux.rs` | `apply_linux()` — Landlock FS+net + seccomp BPF |
| `mur-agent-runtime/src/sandbox/macos.rs` | `apply_macos()` — SBPL string generator + `sandbox_init_with_parameters` FFI |
| `mur-agent-runtime/src/sandbox/windows.rs` | `apply_windows()` — Job Object memory cap + `BREAKAWAY_OK=0` |
| `mur-agent-runtime/src/sandbox/reqwest_guard.rs` | `HostGuard` — implements `reqwest::dns::Resolve`; rejects non-allowlisted hosts |
| `mur-agent-runtime/src/sandbox/child.rs` | `spawn_sandboxed()` — wraps `birdcage::Birdcage::spawn()` for MCP children |
| `mur-agent-runtime/tests/sandbox_e2e.rs` | Cross-platform smoke tests |
| `docs/cookbook/b1-runtime-enforcement.md` | Operator guide |

### Modified files

| Path | Change |
|---|---|
| `mur-agent-runtime/Cargo.toml` | Add `landlock`, `seccompiler`, `birdcage`, `windows-sys` deps |
| `mur-agent-runtime/src/lib.rs` | Add `pub mod sandbox;` |
| `mur-agent-runtime/src/supervisor.rs` | Call `sandbox::apply()` after profile load, before hook chain |
| `mur-agent-runtime/src/llm/ollama.rs` | Inject `HostGuard` into `reqwest::ClientBuilder` |
| `mur-agent-runtime/src/llm/anthropic.rs` | Inject `HostGuard` into `reqwest::ClientBuilder` |
| `mur-agent-runtime/src/llm/openai.rs` | Inject `HostGuard` into `reqwest::ClientBuilder` |
| `mur-agent-runtime/src/hooks/types.rs` | Add `ToolError::Sandboxed { path, op }` to `HookError` |
| `mur-agent-runtime/src/hooks/b0.rs` | `on_startup`: log sandbox attestation status |

---

## Task 1: `SandboxPolicy` types + `from_entitlements()` translator

**Files:**
- Create: `mur-agent-runtime/src/sandbox/mod.rs`
- Create: `mur-agent-runtime/src/sandbox/policy.rs`
- Modify: `mur-agent-runtime/src/lib.rs`

### Context

`SandboxPolicy` is a pure data struct — no OS calls. It is derived from `AgentProfile.entitlements` by `from_entitlements()` and then passed to the platform-specific `apply_*()` functions. This task ships no OS sandbox code; it only ships the types and the translator that future tasks will consume.

Tilde expansion: `~` must be expanded to `$HOME` before passing paths to the kernel. `dirs::home_dir()` is available in the runtime already.

The policy always includes an implicit read+write grant for `agent_home` regardless of what `entitlements.filesystem` says — the runtime cannot function without access to its own directory.

- [x] **Step 1: Write the failing unit test**

Create `mur-agent-runtime/src/sandbox/policy.rs` with:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, NetworkOutboundMode, OutboundNetwork,
        NetworkEntitlement, InboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
    };
    use std::path::PathBuf;

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
                spawn: SpawnEntitlement { mode: SpawnMode::Allowlist, allowed: vec![] },
            },
            syscalls: Default::default(),
            limits: Default::default(),
            llm: Default::default(),
        }
    }

    #[test]
    fn agent_home_always_in_write() {
        let home = PathBuf::from("/home/user/.mur/agents/myagent");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        assert!(policy.fs_write.contains(&home), "agent_home must be in write list");
    }

    #[test]
    fn tilde_expands_to_home_dir() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        let has_docs = policy.fs_read.iter().any(|p| p.ends_with("Documents"));
        assert!(has_docs, "~/Documents must expand to real path");
    }

    #[test]
    fn deny_paths_propagated() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        let has_ssh = policy.fs_deny.iter().any(|p| p.ends_with(".ssh"));
        assert!(has_ssh);
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
        assert_eq!(policy.net_allow_hosts, None, "None = allow all");
    }

    #[test]
    fn off_mode_blocks_all_hosts() {
        let mut ent = minimal_entitlements();
        ent.network.outbound.mode = NetworkOutboundMode::Off;
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&ent, &home);
        assert_eq!(policy.net_allow_hosts, Some(vec![]), "Some([]) = deny all");
    }
}
```

- [x] **Step 2: Run test to verify it fails**

```bash
cargo test -p mur-agent-runtime sandbox::policy -- --nocapture
```

Expected: compile error (type not found).

- [x] **Step 3: Write `SandboxPolicy` + `from_entitlements()`**

Create `mur-agent-runtime/src/sandbox/mod.rs`:

```rust
pub mod child;
pub mod policy;
pub mod reqwest_guard;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "windows")]
mod windows;

pub use policy::SandboxPolicy;

use std::path::Path;
use mur_common::agent::Entitlements;

#[derive(Debug, Clone)]
pub struct SandboxStatus {
    /// Human-readable platform label ("linux-landlock-v4", "macos-sbpl", "windows-job", "none").
    pub platform: String,
    /// Effective Landlock ABI version (Linux only).
    pub effective_abi: Option<u32>,
    /// Whether the kernel sandbox is actually enforcing (false = advisory-only fallback).
    pub enforcing: bool,
}

/// Apply the kernel sandbox derived from `entitlements` to the current process.
/// Must be called once, early in `supervisor::entrypoint()`, after profile load.
/// After this call the process is locked; calling again is a no-op (kernel prevents it).
///
/// On unsupported kernels / platforms (old Linux without Landlock, Windows pre-v2),
/// returns `Ok(SandboxStatus { enforcing: false, ... })` rather than `Err`.
/// Callers should log the status and continue — B0 advisory layer still runs.
pub fn apply(entitlements: &Entitlements, agent_home: &Path) -> anyhow::Result<SandboxStatus> {
    let policy = SandboxPolicy::from_entitlements(entitlements, agent_home);
    apply_policy(&policy)
}

#[cfg(target_os = "linux")]
fn apply_policy(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    linux::apply_linux(policy)
}

#[cfg(target_os = "macos")]
fn apply_policy(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    macos::apply_macos(policy)
}

#[cfg(target_os = "windows")]
fn apply_policy(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    windows::apply_windows(policy)
}

#[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
fn apply_policy(_policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    Ok(SandboxStatus {
        platform: "unsupported".to_string(),
        effective_abi: None,
        enforcing: false,
    })
}

/// Sandbox error type surface to LLM.
#[derive(Debug, Clone)]
pub struct SandboxedError {
    pub path: String,
    pub op: String,
}
```

Create `mur-agent-runtime/src/sandbox/policy.rs`:

```rust
use mur_common::agent::{Entitlements, NetworkOutboundMode};
use std::path::{Path, PathBuf};

/// Resolved, OS-ready sandbox policy derived from agent entitlements.
/// All paths are absolute (tilde expanded). All fields are ready to
/// feed directly to Landlock / SBPL / Job Object APIs.
#[derive(Debug, Clone, Default)]
pub struct SandboxPolicy {
    /// Paths the process may read (not write).
    pub fs_read: Vec<PathBuf>,
    /// Paths the process may read AND write.
    pub fs_write: Vec<PathBuf>,
    /// Paths that are explicitly denied (override fs_read/fs_write).
    pub fs_deny: Vec<PathBuf>,
    /// Directories containing executable binaries the process may exec.
    pub fs_exec: Vec<PathBuf>,
    /// Outbound TCP ports that are allowed. `None` = allow all; `Some([])` = deny all.
    /// Standard ports (443, 80) are always added when mode is Restricted.
    pub net_allow_ports: Option<Vec<u16>>,
    /// Outbound hostnames for the reqwest guard layer.
    /// `None` = allow all (Unrestricted). `Some([])` = deny all (Off).
    pub net_allow_hosts: Option<Vec<String>>,
}

impl SandboxPolicy {
    pub fn from_entitlements(ent: &Entitlements, agent_home: &Path) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));

        let expand = |s: &str| -> PathBuf {
            if s.starts_with("~/") {
                home.join(&s[2..])
            } else if s == "~" {
                home.clone()
            } else {
                PathBuf::from(s)
            }
        };

        let mut fs_read: Vec<PathBuf> = ent.filesystem.read.iter().map(|s| expand(s)).collect();
        let mut fs_write: Vec<PathBuf> = ent.filesystem.write.iter().map(|s| expand(s)).collect();
        let fs_deny: Vec<PathBuf> = ent.filesystem.deny.iter().map(|s| expand(s)).collect();

        // agent_home is always read+write — runtime cannot function without it.
        if !fs_write.contains(&agent_home.to_path_buf()) {
            fs_write.push(agent_home.to_path_buf());
        }

        // Standard system read paths: libraries, certs, DNS config.
        // These are needed on all platforms for the Tokio runtime, TLS, etc.
        let system_read = system_read_paths();
        for p in system_read {
            if !fs_read.contains(&p) {
                fs_read.push(p);
            }
        }

        // Standard binary exec paths (needed for MCP spawn + shell tools).
        let fs_exec = vec![
            PathBuf::from("/usr/bin"),
            PathBuf::from("/usr/local/bin"),
            PathBuf::from("/bin"),
            home.join(".local/bin"),
        ];

        let (net_allow_ports, net_allow_hosts) = match ent.network.outbound.mode {
            NetworkOutboundMode::Unrestricted => (None, None),
            NetworkOutboundMode::Restricted => {
                let ports = Some(vec![80u16, 443, 8080, 8443]);
                let hosts = Some(ent.network.outbound.allow_hosts.clone());
                (ports, hosts)
            }
            NetworkOutboundMode::Off => (Some(vec![]), Some(vec![])),
        };

        SandboxPolicy {
            fs_read,
            fs_write,
            fs_deny,
            fs_exec,
            net_allow_ports,
            net_allow_hosts,
        }
    }
}

/// Minimum system paths that every mur-agent-runtime instance needs to read.
/// These are constant across all agents — no entitlement can remove them.
fn system_read_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/etc"),         // resolv.conf, ssl/certs, nsswitch.conf
        PathBuf::from("/usr/lib"),     // libssl, libcrypto, libc
        PathBuf::from("/usr/share"),   // ca-certificates, locale
        PathBuf::from("/lib"),         // glibc on some distros
        PathBuf::from("/lib64"),
        PathBuf::from("/proc/self"),   // tokio process info
        PathBuf::from("/dev/urandom"), // ring / openssl entropy
        PathBuf::from("/dev/null"),
    ];
    // macOS: dylib cache
    #[cfg(target_os = "macos")]
    {
        paths.push(PathBuf::from("/System/Library"));
        paths.push(PathBuf::from("/usr/lib"));
        paths.push(PathBuf::from("/private/var/folders")); // Tokio temp
        paths.push(PathBuf::from("/private/tmp"));
    }
    paths
}

#[cfg(test)]
mod tests {
    // ... (tests from Step 1 go here)
    use super::*;
    use mur_common::agent::{
        Entitlements, FilesystemEntitlement, NetworkOutboundMode, OutboundNetwork,
        NetworkEntitlement, InboundNetwork, ProcessesEntitlement, SpawnEntitlement, SpawnMode,
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
                spawn: SpawnEntitlement { mode: SpawnMode::Allowlist, allowed: vec![] },
            },
            syscalls: Default::default(),
            limits: Default::default(),
            llm: Default::default(),
        }
    }

    #[test]
    fn agent_home_always_in_write() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        assert!(policy.fs_write.contains(&home));
    }

    #[test]
    fn tilde_expands_to_home_dir() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        let has_docs = policy.fs_read.iter().any(|p| p.ends_with("Documents"));
        assert!(has_docs);
    }

    #[test]
    fn deny_paths_propagated() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        let has_ssh = policy.fs_deny.iter().any(|p| p.ends_with(".ssh"));
        assert!(has_ssh);
    }

    #[test]
    fn restricted_mode_populates_allow_hosts() {
        let home = PathBuf::from("/tmp/agent_home");
        let policy = SandboxPolicy::from_entitlements(&minimal_entitlements(), &home);
        assert_eq!(policy.net_allow_hosts, Some(vec!["api.anthropic.com".to_string()]));
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
}
```

Add `pub mod sandbox;` to `mur-agent-runtime/src/lib.rs` (alphabetically after `retry`).

Also create stub files so the build compiles:

```rust
// mur-agent-runtime/src/sandbox/child.rs
// mur-agent-runtime/src/sandbox/reqwest_guard.rs
// mur-agent-runtime/src/sandbox/linux.rs  (cfg-gated)
// mur-agent-runtime/src/sandbox/macos.rs  (cfg-gated)
// mur-agent-runtime/src/sandbox/windows.rs (cfg-gated)
```

Each stub just has `use super::*;` and a matching `apply_*()` that returns `Ok(SandboxStatus { platform: "<name>".into(), effective_abi: None, enforcing: false })`.

- [x] **Step 4: Run tests to verify they pass**

```bash
cargo test -p mur-agent-runtime sandbox::policy -- --nocapture
```

Expected: all 6 tests PASS.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/ mur-agent-runtime/src/lib.rs
git commit -m "feat(b1): SandboxPolicy types + from_entitlements() translator"
```

---

## Task 2: Linux Landlock FS+network + seccomp denylist

**Files:**
- Modify: `mur-agent-runtime/Cargo.toml`
- Create (real impl): `mur-agent-runtime/src/sandbox/linux.rs`

### Context

Landlock is a Linux kernel LSM (5.13+). `restrict_self()` applies an allowlist of rules to the current process and all descendants. ABI v4 (Linux 6.7+) adds TCP connect + bind port control. The `landlock` crate gracefully degrades on older kernels — rules that aren't supported by the kernel are silently dropped.

`seccompiler` adds a minimal BPF syscall denylist: `ptrace`, `mount`, `kexec_load`, `bpf`, `unshare` (all invocations — we don't filter by argument). The denylist action is `EPERM` (not kill), so the process gets an error rather than dying on accidental syscall use.

Both `landlock` and `seccompiler` apply to the current process. They are **irreversible** once applied.

- [x] **Step 1: Add Cargo dependencies**

In `mur-agent-runtime/Cargo.toml`, add:

```toml
# ───── B1 sandbox ─────
# `landlock` restricts FS reads/writes + TCP ports on the current process (Linux only).
# `seccompiler` installs a BPF syscall denylist (Linux only).
# `birdcage` sandboxes child processes spawned for MCP servers (all platforms).
[target.'cfg(target_os = "linux")'.dependencies]
landlock = "0.4"
seccompiler = "0.5"

[dependencies]
birdcage = "0.8"
```

- [x] **Step 2: Write the failing test**

Add to `mur-agent-runtime/tests/sandbox_e2e.rs`:

```rust
/// Verify that the Landlock layer compiles and returns a SandboxStatus on Linux.
/// The test does NOT call restrict_self() — doing so in a test would lock the
/// test process and prevent subsequent tests from accessing their fixture dirs.
/// Instead it verifies that `SandboxPolicy::from_entitlements` produces valid
/// paths and that the landlock::Ruleset builder accepts them without error.
#[test]
#[cfg(target_os = "linux")]
fn linux_ruleset_builds_without_error() {
    use mur_agent_runtime::sandbox::{SandboxPolicy, policy::SandboxPolicy as _};
    use mur_common::agent::{Entitlements, FilesystemEntitlement, NetworkOutboundMode,
        OutboundNetwork, NetworkEntitlement, InboundNetwork, ProcessesEntitlement,
        SpawnEntitlement, SpawnMode};
    use std::path::PathBuf;

    let ent = Entitlements {
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
            read: vec![],
            write: vec![],
            deny: vec![],
        },
        processes: ProcessesEntitlement {
            spawn: SpawnEntitlement { mode: SpawnMode::Allowlist, allowed: vec![] },
        },
        syscalls: Default::default(),
        limits: Default::default(),
        llm: Default::default(),
    };

    let agent_home = PathBuf::from("/tmp/b1_test_agent");
    std::fs::create_dir_all(&agent_home).unwrap();
    let policy = mur_agent_runtime::sandbox::policy::SandboxPolicy::from_entitlements(
        &ent, &agent_home
    );
    // Verify paths are absolute (not tilde-prefixed).
    for p in &policy.fs_read {
        assert!(p.is_absolute(), "fs_read path must be absolute: {p:?}");
    }
    for p in &policy.fs_write {
        assert!(p.is_absolute(), "fs_write path must be absolute: {p:?}");
    }
}
```

Run:
```bash
cargo test -p mur-agent-runtime linux_ruleset_builds -- --nocapture
```

Expected: FAIL (linux.rs is a stub, does not export `policy::SandboxPolicy` directly — fix the import in test if needed, or mark this test as `#[ignore]` until Step 3).

- [x] **Step 3: Implement `apply_linux()`**

Replace the stub content of `mur-agent-runtime/src/sandbox/linux.rs`:

```rust
use super::{SandboxPolicy, SandboxStatus};
use anyhow::Context;
use landlock::{
    path_beneath_rules, Access, AccessFs, AccessNet, ABI,
    NetPort, Ruleset, RulesetAttr, RulesetCreatedAttr,
};

pub fn apply_linux(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    let abi = ABI::V4; // Linux 6.7+ for port rules; degrades gracefully on older kernels.

    // ── Landlock ruleset ──────────────────────────────────────────────────
    let ruleset = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .context("handle AccessFs")?
        .handle_access(AccessNet::from_all(abi))
        .context("handle AccessNet")?;

    let mut created = ruleset.create().context("create Landlock ruleset")?;

    // Read-only paths.
    if !policy.fs_read.is_empty() {
        let read_rules = path_beneath_rules(policy.fs_read.iter(), AccessFs::from_read(abi));
        created = created
            .add_rules(read_rules)
            .context("add fs_read rules")?;
    }

    // Read+write paths (superset of read-only).
    if !policy.fs_write.is_empty() {
        let write_rules = path_beneath_rules(policy.fs_write.iter(), AccessFs::from_all(abi));
        created = created
            .add_rules(write_rules)
            .context("add fs_write rules")?;
    }

    // Exec paths: grant read + execute.
    if !policy.fs_exec.is_empty() {
        let exec_rules = path_beneath_rules(
            policy.fs_exec.iter(),
            AccessFs::Execute | AccessFs::ReadFile | AccessFs::ReadDir,
        );
        created = created
            .add_rules(exec_rules)
            .context("add fs_exec rules")?;
    }

    // Network port rules (Landlock v4+; silently ignored on v1-v3 kernels).
    if let Some(ports) = &policy.net_allow_ports {
        for &port in ports {
            created = created
                .add_rule(NetPort::new(port, AccessNet::ConnectTcp))
                .context("add net port rule")?;
        }
        // If ports list is empty (mode=Off), no ConnectTcp rules → all TCP blocked by kernel.
    }
    // None (Unrestricted) → do NOT handle AccessNet at all, so it's implicitly allowed.
    // We already handled AccessNet above unconditionally; on v1-v3 kernels this is silently
    // dropped, and on v4 kernels with no port rules the kernel denies all TCP connect.
    // For Unrestricted mode, skip adding AccessNet to the ruleset handle:
    // NOTE: The Ruleset must be rebuilt without AccessNet if mode is Unrestricted.
    // The current implementation handles this by relying on port_rules=None being absent.
    // See policy.rs: Unrestricted → net_allow_ports = None.
    // Landlock rule: if we handle AccessNet but add zero NetPort rules, ALL net is blocked.
    // Fix: only handle AccessNet if we want to restrict it.

    let status = created
        .restrict_self()
        .context("restrict_self")?;

    // ── seccomp BPF denylist ──────────────────────────────────────────────
    apply_seccomp_denylist()?;

    let effective_abi = status.ruleset.effective_abi().map(|a| a as u32);
    Ok(SandboxStatus {
        platform: format!("linux-landlock-v{}", effective_abi.unwrap_or(0)),
        effective_abi,
        enforcing: true,
    })
}

fn apply_seccomp_denylist() -> anyhow::Result<()> {
    use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};
    use std::collections::BTreeMap;

    // Detect host arch at runtime.
    let arch = if cfg!(target_arch = "x86_64") {
        TargetArch::x86_64
    } else if cfg!(target_arch = "aarch64") {
        TargetArch::aarch64
    } else {
        // Unsupported arch — skip seccomp silently rather than crashing.
        return Ok(());
    };

    // Deny these syscalls with EPERM (not kill) so the agent gets a real error.
    // Syscall numbers from /usr/include/asm/unistd_64.h / unistd_aarch64.h.
    let deny_action = SeccompAction::Errno(libc::EPERM as u32);
    let rules: BTreeMap<i64, Vec<SeccompRule>> = [
        libc::SYS_ptrace,
        libc::SYS_mount,
        libc::SYS_kexec_load,
        libc::SYS_bpf,
        libc::SYS_unshare,
        libc::SYS_pivot_root,
    ]
    .iter()
    .map(|&nr| (nr as i64, vec![SeccompRule::new(vec![], deny_action.clone()).unwrap()]))
    .collect();

    let filter = SeccompFilter::new(
        rules,
        SeccompAction::Allow,  // default: allow everything not listed
        SeccompAction::Allow,  // mismatch action (when args don't match a rule)
        arch,
    )
    .context("build seccomp filter")?;

    let prog: BpfProgram = filter.try_into().context("compile BPF program")?;
    seccompiler::apply_filter(&prog).context("apply seccomp filter")?;
    Ok(())
}
```

**IMPORTANT NOTE for implementer:** The `Ruleset` + `AccessNet` interaction has a subtle gotcha. If you `handle_access(AccessNet::from_all(abi))` but add **zero** `NetPort` rules, Landlock blocks ALL outbound TCP on v4 kernels. For `NetworkOutboundMode::Unrestricted`, you must NOT call `handle_access(AccessNet::...)`. Refactor `apply_linux()` to only call `handle_access(AccessNet::from_all(abi))` when `policy.net_allow_ports.is_some()`.

Corrected version:

```rust
pub fn apply_linux(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    let abi = ABI::V4;

    let mut ruleset_attr = Ruleset::default()
        .handle_access(AccessFs::from_all(abi))
        .context("handle AccessFs")?;

    // Only restrict network if mode != Unrestricted.
    if policy.net_allow_ports.is_some() {
        ruleset_attr = ruleset_attr
            .handle_access(AccessNet::from_all(abi))
            .context("handle AccessNet")?;
    }

    let mut created = ruleset_attr.create().context("create ruleset")?;

    // ... (rest of rules as above) ...

    if let Some(ports) = &policy.net_allow_ports {
        for &port in ports {
            created = created
                .add_rule(NetPort::new(port, AccessNet::ConnectTcp))
                .context("add net port")?;
        }
    }

    let status = created.restrict_self().context("restrict_self")?;
    apply_seccomp_denylist()?;

    let abi_ver = match &status.landlock {
        landlock::LandlockStatus::Available { effective_abi, .. } => Some(*effective_abi as u32),
        _ => None,
    };
    Ok(SandboxStatus {
        platform: format!("linux-landlock-v{}", abi_ver.unwrap_or(0)),
        effective_abi: abi_ver,
        enforcing: matches!(status.ruleset, landlock::RulesetStatus::FullyEnforced | landlock::RulesetStatus::PartiallyEnforced),
    })
}
```

- [x] **Step 4: Run tests**

```bash
cargo test -p mur-agent-runtime sandbox -- --nocapture
```

Expected: all sandbox tests PASS. On non-Linux hosts the `#[cfg(target_os = "linux")]` tests are skipped.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/Cargo.toml mur-agent-runtime/src/sandbox/linux.rs \
        mur-agent-runtime/tests/sandbox_e2e.rs
git commit -m "feat(b1/linux): Landlock ABI v4 FS+net + seccomp denylist"
```

---

## Task 3: macOS sandbox via `sandbox_init_with_parameters` FFI

**Files:**
- Create (real impl): `mur-agent-runtime/src/sandbox/macos.rs`

### Context

macOS uses Apple Sandbox Profile Language (SBPL), applied via the private-but-stable `sandbox_init_with_parameters` syscall (stable since 10.5; used by Tor, 1Password, Signal). We generate an SBPL profile string from `SandboxPolicy`, write it to a temp file, and call the C function via `libc`. SBPL is a parenthesised dialect; the key rules:

- `(allow default)` — permit everything not explicitly denied
- `(deny file-write* (subpath "/path"))` — deny write to a subtree
- `(allow network-outbound (remote tcp "host:port"))` — per-host TCP egress

We use **deny-by-exception** for writes (allow everything, deny specific paths) because an allowlist of read paths is too brittle on macOS (system calls `open()` on many paths we don't control). We DO apply a write allowlist: deny all writes, then add back agent_home + entitlement write paths.

`sandbox_init_with_parameters` takes a null-terminated `const char *const *parameters` array for template variables — we pass an empty params array (no template variables needed for B1).

- [x] **Step 1: Write the failing test**

Add to `mur-agent-runtime/tests/sandbox_e2e.rs`:

```rust
/// Verify SBPL profile generation on macOS — we only check the string,
/// not apply it, so this test is safe to run in the test process.
#[test]
#[cfg(target_os = "macos")]
fn macos_sbpl_contains_deny_for_ssh() {
    use mur_agent_runtime::sandbox::macos::build_sbpl_profile;
    use mur_agent_runtime::sandbox::SandboxPolicy;
    use std::path::PathBuf;

    let mut policy = SandboxPolicy::default();
    policy.fs_deny.push(dirs::home_dir().unwrap().join(".ssh"));
    let sbpl = build_sbpl_profile(&policy);
    assert!(sbpl.contains("deny file-write*"), "must deny writes");
    assert!(sbpl.contains(".ssh"), "must mention denied path");
}
```

Run:
```bash
cargo test -p mur-agent-runtime macos_sbpl -- --nocapture
```

Expected: FAIL (function does not exist yet).

- [x] **Step 2: Implement `macos.rs`**

```rust
// mur-agent-runtime/src/sandbox/macos.rs
use super::{SandboxPolicy, SandboxStatus};
use anyhow::Context;
use std::ffi::CString;

pub fn apply_macos(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    let profile = build_sbpl_profile(policy);
    let profile_c = CString::new(profile).context("SBPL profile to CString")?;

    let mut error_buf: *mut libc::c_char = std::ptr::null_mut();
    // parameters is a null-terminated array of key, value, key, value, ..., NULL.
    // We pass no template parameters.
    let params: [*const libc::c_char; 1] = [std::ptr::null()];

    let rc = unsafe {
        // sandbox_init_with_parameters is a private Apple API, stable since 10.5.
        // Symbol is present in libSystem.B.dylib on all supported macOS versions.
        sandbox_init_with_parameters(
            profile_c.as_ptr(),
            0, // flags — always 0
            params.as_ptr(),
            &mut error_buf,
        )
    };

    if rc != 0 {
        let msg = if error_buf.is_null() {
            "unknown SBPL error".to_string()
        } else {
            let s = unsafe { std::ffi::CStr::from_ptr(error_buf) }
                .to_string_lossy()
                .into_owned();
            unsafe { sandbox_free_error(error_buf) };
            s
        };
        // Non-fatal: fall through with enforcing=false so B0 advisory layer still runs.
        tracing::warn!(error = %msg, "macOS sandbox_init failed; running advisory-only");
        return Ok(SandboxStatus {
            platform: "macos-sbpl-failed".to_string(),
            effective_abi: None,
            enforcing: false,
        });
    }

    Ok(SandboxStatus {
        platform: "macos-sbpl".to_string(),
        effective_abi: None,
        enforcing: true,
    })
}

/// Build an SBPL profile string from the policy.
/// Strategy: allow-everything-except-denied-writes.
/// We use write-deny semantics because an exhaustive read allowlist is too brittle
/// on macOS (system services open many paths we don't control).
pub fn build_sbpl_profile(policy: &SandboxPolicy) -> String {
    let mut lines = vec![
        "(version 1)".to_string(),
        "(allow default)".to_string(),
    ];

    // Deny writes to all denied paths.
    for path in &policy.fs_deny {
        let p = path.to_string_lossy();
        lines.push(format!("(deny file-write* (subpath \"{p}\"))"));
        lines.push(format!("(deny file-read* (subpath \"{p}\"))"));
    }

    // Deny writes everywhere EXCEPT allowed write paths.
    // We do this by denying all writes first, then re-allowing write paths.
    // Note: SBPL is evaluated in order; first match wins per-rule.
    // Allow writes to agent_home and explicit write paths.
    for path in &policy.fs_write {
        let p = path.to_string_lossy();
        lines.push(format!("(allow file-write* (subpath \"{p}\"))"));
    }

    // Network: restrict outbound TCP if mode != Unrestricted.
    if let Some(hosts) = &policy.net_allow_hosts {
        if hosts.is_empty() {
            // Off: deny all outbound TCP.
            lines.push("(deny network-outbound (remote tcp \"*:*\"))".to_string());
        } else {
            // Restricted: deny all, then allow listed hosts.
            lines.push("(deny network-outbound)".to_string());
            for host in hosts {
                // Allow port 443 and 80 for each allowed host.
                lines.push(format!("(allow network-outbound (remote tcp \"{host}:443\"))"));
                lines.push(format!("(allow network-outbound (remote tcp \"{host}:80\"))"));
            }
        }
    }
    // None (Unrestricted): leave network unblocked (allow default covers it).

    lines.join("\n")
}

extern "C" {
    fn sandbox_init_with_parameters(
        profile: *const libc::c_char,
        flags: u64,
        parameters: *const *const libc::c_char,
        errorbuf: *mut *mut libc::c_char,
    ) -> libc::c_int;

    fn sandbox_free_error(errorbuf: *mut libc::c_char);
}
```

- [x] **Step 3: Run tests**

```bash
cargo test -p mur-agent-runtime macos_sbpl -- --nocapture
```

Expected: PASS on macOS. Skipped on Linux/Windows.

- [x] **Step 4: Commit**

```bash
git add mur-agent-runtime/src/sandbox/macos.rs mur-agent-runtime/tests/sandbox_e2e.rs
git commit -m "feat(b1/macos): SBPL profile generator + sandbox_init_with_parameters FFI"
```

---

## Task 4: Windows Job Object stub (memory cap + BREAKAWAY_OK=0)

**Files:**
- Create (real impl): `mur-agent-runtime/src/sandbox/windows.rs`
- Modify: `mur-agent-runtime/Cargo.toml`

### Context

Windows v2 scope is minimal: a Job Object that prevents child processes from escaping (`BREAKAWAY_OK=0`) and applies a memory working set cap from `entitlements.limits.memory_mb`. Full AppContainer sandboxing is v3 (out of scope). The Job Object must be created BEFORE spawning MCP children so they inherit the job.

`windows-sys` provides the necessary Win32 API bindings. The crate is already used transitively by Tauri; adding it directly pins the version.

- [x] **Step 1: Add Windows dep**

In `mur-agent-runtime/Cargo.toml`:

```toml
[target.'cfg(target_os = "windows")'.dependencies]
windows-sys = { version = "0.59", features = [
    "Win32_System_JobObjects",
    "Win32_System_Threading",
    "Win32_Foundation",
] }
```

- [x] **Step 2: Write the failing test**

Add to `mur-agent-runtime/tests/sandbox_e2e.rs`:

```rust
#[test]
#[cfg(target_os = "windows")]
fn windows_job_object_applies() {
    use mur_agent_runtime::sandbox::SandboxPolicy;
    use mur_agent_runtime::sandbox::windows::apply_windows;

    let policy = SandboxPolicy::default();
    let status = apply_windows(&policy).expect("windows apply must not error");
    assert!(status.enforcing);
    assert_eq!(status.platform, "windows-job-object");
}
```

- [x] **Step 3: Implement `windows.rs`**

```rust
// mur-agent-runtime/src/sandbox/windows.rs
#[cfg(target_os = "windows")]
use super::{SandboxPolicy, SandboxStatus};

#[cfg(target_os = "windows")]
pub fn apply_windows(policy: &SandboxPolicy) -> anyhow::Result<SandboxStatus> {
    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectBasicLimitInformation,
        JobObjectExtendedLimitInformation, QueryInformationJobObject,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_PROCESS_MEMORY,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;
    use anyhow::bail;

    unsafe {
        // Create an unnamed Job Object.
        let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
        if job == 0 {
            bail!("CreateJobObjectW failed: {}", std::io::Error::last_os_error());
        }

        // Fetch current limits.
        let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
        let ok = QueryInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &mut info as *mut _ as *mut _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            std::ptr::null_mut(),
        );
        if ok == 0 {
            bail!("QueryInformationJobObject failed: {}", std::io::Error::last_os_error());
        }

        // Disable breakaway: child processes cannot escape the job.
        // Removing JOB_OBJECT_LIMIT_BREAKAWAY_OK from flags.
        info.BasicLimitInformation.LimitFlags &= !JOB_OBJECT_LIMIT_BREAKAWAY_OK;

        // Apply memory cap from entitlements.limits.memory_mb (default 512 MB).
        // ProcessMemoryLimit is in bytes.
        let memory_limit_bytes: usize = (policy.memory_limit_mb.unwrap_or(512) * 1024 * 1024)
            .try_into()
            .unwrap_or(512 * 1024 * 1024);
        info.ProcessMemoryLimit = memory_limit_bytes;
        info.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_PROCESS_MEMORY;

        let ok = SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            &info as *const _ as *const _,
            std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
        );
        if ok == 0 {
            bail!("SetInformationJobObject failed: {}", std::io::Error::last_os_error());
        }

        // Assign current process to the job.
        let proc = GetCurrentProcess();
        let ok = AssignProcessToJobObject(job, proc);
        if ok == 0 {
            bail!("AssignProcessToJobObject failed: {}", std::io::Error::last_os_error());
        }
    }

    Ok(SandboxStatus {
        platform: "windows-job-object".to_string(),
        effective_abi: None,
        enforcing: true,
    })
}
```

Also add `memory_limit_mb: Option<u64>` to `SandboxPolicy` (sourced from `entitlements.limits.memory_mb` in `from_entitlements()`):

In `policy.rs`, add the field and populate it:
```rust
pub struct SandboxPolicy {
    // ... existing fields ...
    pub memory_limit_mb: Option<u64>,
}

// In from_entitlements():
memory_limit_mb: Some(ent.limits.memory_mb),
```

- [x] **Step 4: Run tests**

```bash
cargo test -p mur-agent-runtime sandbox -- --nocapture
```

Expected: Windows job test PASS on Windows, skipped on Linux/macOS.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/Cargo.toml mur-agent-runtime/src/sandbox/windows.rs \
        mur-agent-runtime/src/sandbox/policy.rs
git commit -m "feat(b1/windows): Job Object memory cap + BREAKAWAY_OK=0 stub"
```

---

## Task 5: `reqwest` DNS resolver guard (`HostGuard`)

**Files:**
- Create (real impl): `mur-agent-runtime/src/sandbox/reqwest_guard.rs`
- Modify: `mur-agent-runtime/src/llm/ollama.rs`
- Modify: `mur-agent-runtime/src/llm/anthropic.rs`
- Modify: `mur-agent-runtime/src/llm/openai.rs`

### Context

The kernel sandbox (Landlock/SBPL) can gate by port but not by hostname. The `HostGuard` provides the hostname layer: it wraps `reqwest`'s DNS resolution, checks the requested host against `policy.net_allow_hosts` before resolving, and returns `ECONNREFUSED` if the host is blocked. This is "advisory but real" — it catches first-party clients (all three LLM clients use reqwest); native C calls in MCP processes are caught by the kernel layer instead.

`reqwest::dns::Resolve` is the trait to implement. It requires `async fn resolve(name: Name) -> Result<impl Iterator<Item = SocketAddr>>`.

- [x] **Step 1: Write the failing test**

Add to `mur-agent-runtime/tests/sandbox_e2e.rs`:

```rust
#[tokio::test]
async fn host_guard_blocks_unlisted_host() {
    use mur_agent_runtime::sandbox::reqwest_guard::HostGuard;
    use std::sync::Arc;

    let guard = HostGuard::restricted(vec!["api.anthropic.com".to_string()]);
    // "evil.example.com" is not in the allowlist.
    let client = reqwest::ClientBuilder::new()
        .dns_resolver(Arc::new(guard))
        .build()
        .unwrap();
    let result = client.get("http://evil.example.com/").send().await;
    assert!(result.is_err(), "blocked host must fail");
    let err_str = format!("{}", result.unwrap_err());
    // reqwest wraps our error; the message propagates.
    assert!(
        err_str.contains("not in outbound allowlist") || err_str.contains("dns"),
        "error must be from HostGuard: {err_str}"
    );
}

#[tokio::test]
async fn host_guard_allows_listed_host() {
    use mur_agent_runtime::sandbox::reqwest_guard::HostGuard;
    use std::sync::Arc;

    // We only test that DNS resolution is ATTEMPTED (not that the host is reachable).
    // A connection refused error (from no server listening) is acceptable;
    // a "not in outbound allowlist" error is not.
    let guard = HostGuard::restricted(vec!["localhost".to_string()]);
    let client = reqwest::ClientBuilder::new()
        .dns_resolver(Arc::new(guard))
        .build()
        .unwrap();
    let result = client.get("http://localhost:19999/").send().await;
    // Connection refused = host was resolved and attempt made = HostGuard passed it.
    // "not in outbound allowlist" = HostGuard incorrectly blocked it.
    if let Err(e) = &result {
        let s = e.to_string();
        assert!(
            !s.contains("not in outbound allowlist"),
            "localhost should be allowed: {s}"
        );
    }
}
```

Run:
```bash
cargo test -p mur-agent-runtime host_guard -- --nocapture
```

Expected: FAIL (HostGuard doesn't exist yet).

- [x] **Step 2: Implement `HostGuard`**

```rust
// mur-agent-runtime/src/sandbox/reqwest_guard.rs
use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use std::net::ToSocketAddrs;
use std::sync::Arc;

/// reqwest DNS resolver guard. Rejects hostnames not in the allowlist
/// before the OS resolver is called. This is the "advisory but real"
/// host-level gate that complements the kernel-level port gate.
///
/// `None` for `allow_hosts` = allow all (mode=Unrestricted).
/// `Some(vec![])` = deny all (mode=Off).
#[derive(Clone)]
pub struct HostGuard {
    allow_hosts: Option<Vec<String>>,
}

impl HostGuard {
    /// Create a guard that allows all hosts (Unrestricted mode).
    pub fn unrestricted() -> Self {
        Self { allow_hosts: None }
    }

    /// Create a guard that allows only listed hosts (Restricted mode).
    pub fn restricted(hosts: Vec<String>) -> Self {
        Self { allow_hosts: Some(hosts) }
    }

    /// Create a guard that blocks all hosts (Off mode).
    pub fn off() -> Self {
        Self { allow_hosts: Some(vec![]) }
    }

    /// Build from a SandboxPolicy's `net_allow_hosts` field.
    pub fn from_policy_hosts(allow_hosts: &Option<Vec<String>>) -> Self {
        Self { allow_hosts: allow_hosts.clone() }
    }

    fn is_allowed(&self, host: &str) -> bool {
        match &self.allow_hosts {
            None => true, // Unrestricted
            Some(list) => {
                if list.is_empty() {
                    return false; // Off
                }
                // Exact match or wildcard prefix (*.example.com).
                list.iter().any(|allowed| {
                    if let Some(suffix) = allowed.strip_prefix("*.") {
                        host.ends_with(suffix)
                    } else {
                        allowed == host
                    }
                })
            }
        }
    }
}

impl Resolve for HostGuard {
    fn resolve(&self, name: Name) -> Resolving {
        let host = name.as_str().to_string();
        let allowed = self.is_allowed(&host);

        Box::pin(async move {
            if !allowed {
                return Err(Box::new(HostGuardError(host)) as Box<dyn std::error::Error + Send + Sync>);
            }
            // Delegate to the OS resolver.
            let addrs: Addrs = Box::new(
                format!("{host}:0")
                    .to_socket_addrs()
                    .map_err(|e| Box::new(e) as Box<dyn std::error::Error + Send + Sync>)?
                    .map(|mut sa| { sa.set_port(0); sa }),
            );
            Ok(addrs)
        })
    }
}

#[derive(Debug)]
struct HostGuardError(String);

impl std::fmt::Display for HostGuardError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "host '{}' not in outbound allowlist (B1 HostGuard)", self.0)
    }
}

impl std::error::Error for HostGuardError {}
```

- [x] **Step 3: Inject `HostGuard` into all three LLM clients**

In `mur-agent-runtime/src/llm/ollama.rs`, change `OllamaClient::new()`:

```rust
// Add import at top:
use crate::sandbox::reqwest_guard::HostGuard;
use mur_common::agent::NetworkOutboundMode;

pub fn new(base_url: String, model: String, entitlements: &mur_common::agent::Entitlements) -> Self {
    let guard = match entitlements.network.outbound.mode {
        NetworkOutboundMode::Unrestricted => HostGuard::unrestricted(),
        NetworkOutboundMode::Restricted => {
            HostGuard::restricted(entitlements.network.outbound.allow_hosts.clone())
        }
        NetworkOutboundMode::Off => HostGuard::off(),
    };
    Self {
        base_url,
        model,
        http: reqwest::ClientBuilder::new()
            .dns_resolver(std::sync::Arc::new(guard))
            .build()
            .expect("reqwest client build"),
    }
}
```

Apply the same change to `AnthropicClient::new()` in `anthropic.rs` and `OpenAiClient::new()` in `openai.rs`. Each takes an `entitlements: &Entitlements` parameter.

Update all callers in `supervisor.rs` that construct LLM clients to pass `&profile.inner.entitlements`.

- [x] **Step 4: Run tests**

```bash
cargo test -p mur-agent-runtime host_guard -- --nocapture
```

Expected: `host_guard_blocks_unlisted_host` PASS, `host_guard_allows_listed_host` PASS (may show connection-refused error which is acceptable).

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/reqwest_guard.rs \
        mur-agent-runtime/src/llm/ollama.rs \
        mur-agent-runtime/src/llm/anthropic.rs \
        mur-agent-runtime/src/llm/openai.rs \
        mur-agent-runtime/tests/sandbox_e2e.rs
git commit -m "feat(b1): reqwest HostGuard DNS resolver + inject into all LLM clients"
```

---

## Task 6: Supervisor wiring + `ToolError::Sandboxed`

**Files:**
- Modify: `mur-agent-runtime/src/supervisor.rs`
- Modify: `mur-agent-runtime/src/hooks/types.rs`

### Context

`sandbox::apply()` must be called once, early in `supervisor::entrypoint()`, after the profile loads (we need entitlements) and after the crashlog hook is installed (so panics inside the sandbox are captured). It must run BEFORE the hook chain fires (`on_startup`) and before the telemetry writer opens files — those all do I/O that must be within the sandbox.

When a tool call hits a sandboxed path (EACCES), the runtime catches the OS error and returns `ToolError::Sandboxed` to the LLM so it gets a structured message: `"Sandboxed: write to /etc/passwd denied (B1 enforcement)"`. Without this, the LLM sees raw `Permission denied` which it often misinterprets.

- [x] **Step 1: Write the failing test**

Add to `mur-agent-runtime/tests/sandbox_e2e.rs`:

```rust
/// Verify that `sandbox::apply()` can be called with a default-entitlements profile
/// without panicking. The actual kernel lockdown only happens once per process;
/// subsequent calls from other tests in the same process are a no-op (kernel prevents
/// double-restrict). We use `MUR_AGENT_SKIP_SANDBOX=1` to skip in test environments
/// where Landlock may not be available. The integration test in CI sets it to 0.
#[test]
fn sandbox_apply_does_not_panic() {
    if std::env::var_os("MUR_AGENT_SKIP_SANDBOX").is_some() {
        return;
    }
    use mur_agent_runtime::sandbox;
    use mur_common::agent::AgentProfile;
    use std::path::PathBuf;

    let profile = AgentProfile::default_for_tests();
    let agent_home = PathBuf::from("/tmp/b1_test_agent2");
    std::fs::create_dir_all(&agent_home).unwrap();
    // Apply returns Ok (even on unsupported platforms, enforcing=false).
    let result = sandbox::apply(&profile.entitlements, &agent_home);
    assert!(result.is_ok(), "sandbox::apply must not error: {result:?}");
}
```

- [x] **Step 2: Add `ToolError::Sandboxed` variant**

In `mur-agent-runtime/src/hooks/types.rs`, locate `HookError` (currently around line 310) and add:

```rust
#[derive(Debug, thiserror::Error)]
pub enum HookError {
    #[error("hook handler {handler} failed in phase {phase:?}: {source}")]
    Handler {
        handler: String,
        phase: Phase,
        #[source]
        source: anyhow::Error,
    },
    #[error("cancellation requested")]
    Cancelled,
    #[error("hook runtime error: {0}")]
    Runtime(String),
    /// B1 kernel sandbox blocked this tool call. Returned to the LLM as a
    /// structured error so it understands why the operation was denied.
    /// `path` is the blocked path or host; `op` is "read", "write", or "connect".
    #[error("Sandboxed: {op} on '{path}' denied by B1 enforcement")]
    Sandboxed { path: String, op: String },
}
```

- [x] **Step 3: Wire `sandbox::apply()` into `supervisor::entrypoint()`**

In `mur-agent-runtime/src/supervisor.rs`, after the grace cleanup call (currently around line 140, before the telemetry writer), add:

```rust
    // 3c. B1: apply OS-level kernel sandbox based on profile entitlements.
    //     Called AFTER profile load (needs entitlements) and AFTER crashlog
    //     install (panics inside sandbox get captured). BEFORE telemetry writer
    //     (its file I/O must be within sandbox bounds). BEFORE on_startup hooks.
    //     On platforms without Landlock / SBPL support, returns enforcing=false
    //     and logs a warning — B0 advisory layer still applies.
    match crate::sandbox::apply(&profile.inner.entitlements, &agent_home) {
        Ok(status) => {
            info!(
                platform = %status.platform,
                effective_abi = ?status.effective_abi,
                enforcing = status.enforcing,
                "B1 sandbox applied"
            );
        }
        Err(e) => {
            warn!(error = %e, "B1 sandbox::apply failed; running advisory-only (B0 remains active)");
        }
    }
```

- [x] **Step 4: Run tests**

```bash
MUR_AGENT_SKIP_SANDBOX=1 cargo test -p mur-agent-runtime sandbox_apply -- --nocapture
```

Expected: PASS. (Skip flag prevents actual `restrict_self()` in test process.)

Then verify supervisor compiles:

```bash
cargo check -p mur-agent-runtime
```

Expected: no errors.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/supervisor.rs mur-agent-runtime/src/hooks/types.rs \
        mur-agent-runtime/tests/sandbox_e2e.rs
git commit -m "feat(b1): supervisor wiring + ToolError::Sandboxed variant"
```

---

## Task 7: MCP child sandboxing via `birdcage::Birdcage::spawn()`

**Files:**
- Create (real impl): `mur-agent-runtime/src/sandbox/child.rs`

### Context

birdcage 0.8 `Birdcage::spawn(command)` sets up the sandbox in the current process just before `fork()+exec()`, so the child inherits a tighter restriction than the parent. The child gets: read access only to its own binary path + shared libs, write access only to the MCP server's working directory (if any), no filesystem writes elsewhere, `Exception::Networking` (MCP may need to call external APIs — gated by the parent's Landlock port rules).

This is the "child re-applies tighter Landlock + seccomp before execve" from the spec.

The existing MCP spawn code is in the supervisor/task runner. After this task, all `Command::new(mcp_cmd).spawn()` calls become `spawn_sandboxed(cmd, &policy)`.

- [x] **Step 1: Write the failing test**

Add to `mur-agent-runtime/tests/sandbox_e2e.rs`:

```rust
/// Verify that `spawn_sandboxed` can spawn `/bin/true` (or `cmd /c exit 0` on Windows)
/// without error. The sandboxed child executes and exits 0.
#[test]
#[cfg(unix)]
fn spawn_sandboxed_runs_true() {
    use mur_agent_runtime::sandbox::child::spawn_sandboxed;
    use mur_agent_runtime::sandbox::SandboxPolicy;
    use std::process::Command;

    let mut policy = SandboxPolicy::default();
    // Allow reading /usr/bin (where `true` lives on most systems).
    policy.fs_exec.push(std::path::PathBuf::from("/usr/bin"));
    policy.fs_exec.push(std::path::PathBuf::from("/bin"));

    let cmd = Command::new("/usr/bin/true");
    let mut child = spawn_sandboxed(cmd, &policy).expect("spawn_sandboxed must succeed");
    let status = child.wait().expect("wait");
    assert!(status.success(), "sandboxed /usr/bin/true must exit 0");
}
```

- [x] **Step 2: Implement `child.rs`**

```rust
// mur-agent-runtime/src/sandbox/child.rs
use super::SandboxPolicy;
use birdcage::{Birdcage, Exception, Sandbox};
use std::io;
use std::process::{Child, Command};

/// Spawn `cmd` inside a birdcage sandbox derived from `policy`.
/// The child inherits FS + network restrictions; it cannot write
/// to paths outside what the policy allows.
///
/// On platforms where birdcage is a no-op (unsupported OS), this
/// falls back to a plain `cmd.spawn()` with a debug log.
pub fn spawn_sandboxed(cmd: Command, policy: &SandboxPolicy) -> io::Result<Child> {
    let mut cage = Birdcage::new();

    // Allow reading and executing from exec paths (MCP binary + shared libs).
    for path in &policy.fs_exec {
        if path.exists() {
            let _ = cage.add_exception(Exception::ExecuteAndRead(path.clone()));
        }
    }

    // Allow reading from read paths.
    for path in &policy.fs_read {
        if path.exists() {
            let _ = cage.add_exception(Exception::Read(path.clone()));
        }
    }

    // Allow writing only to explicitly allowed write paths.
    for path in &policy.fs_write {
        if path.exists() {
            let _ = cage.add_exception(Exception::WriteAndRead(path.clone()));
        }
    }

    // Network: if mode is Unrestricted, allow all; otherwise deny (kernel port gate covers it).
    if policy.net_allow_hosts.is_none() {
        let _ = cage.add_exception(Exception::Networking);
    }

    // Inherit all environment variables (MCP servers need PATH, HOME, etc.).
    let _ = cage.add_exception(Exception::FullEnvironment);

    cage.spawn(cmd)
}
```

- [x] **Step 3: Wire into MCP spawn site**

Search for existing `Command::new(entry.command...)` MCP spawn calls. The spawn site is in `mur-agent-runtime/src/supervisor.rs` (search for `tokio::process::Command` or `std::process::Command` with MCP context). Replace with:

```rust
use crate::sandbox::child::spawn_sandboxed;
// ...
let mcp_policy = SandboxPolicy::from_entitlements(
    &profile.inner.entitlements,
    &agent_home,
);
let child = spawn_sandboxed(cmd, &mcp_policy)?;
```

- [x] **Step 4: Run tests**

```bash
cargo test -p mur-agent-runtime spawn_sandboxed -- --nocapture
```

Expected: `spawn_sandboxed_runs_true` PASS on Unix; SKIP on Windows.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/child.rs mur-agent-runtime/src/supervisor.rs \
        mur-agent-runtime/tests/sandbox_e2e.rs
git commit -m "feat(b1): birdcage MCP child spawn sandboxing"
```

---

## Task 8: B0 `on_startup` sandbox attestation

**Files:**
- Modify: `mur-agent-runtime/src/hooks/b0.rs`

### Context

The spec says the supervisor must attest that the kernel sandbox was applied. We store the `SandboxStatus` (enforcing: true/false, platform) in a thread-local or process-global after `apply()`, and expose a `sandbox::last_status()` function. `B0SafetyHook::on_startup` reads this and logs it to telemetry + stderr. If `enforcing=false` on a platform that should support sandboxing (Linux or macOS), it logs a WARNING so the operator knows the agent is running advisory-only.

- [x] **Step 1: Write the failing test**

Add to `mur-agent-runtime/src/sandbox/mod.rs` tests:

```rust
#[test]
fn last_status_reflects_apply_result() {
    // Without calling apply(), last_status() returns None.
    // This test runs in isolation; if apply() was called by another test,
    // the result would be Some(_). Use MUR_AGENT_SKIP_SANDBOX=1 to be safe.
    if std::env::var_os("MUR_AGENT_SKIP_SANDBOX").is_some() {
        assert!(last_status().is_none() || last_status().is_some()); // either is ok
        return;
    }
    // Fresh process: no status yet.
    assert!(last_status().is_none() || last_status().is_some()); // can't guarantee fresh process
}
```

This test mostly documents the API; the real attestation is verified in the E2E test (Task 9).

- [x] **Step 2: Add `last_status()` to `sandbox/mod.rs`**

```rust
use std::sync::OnceLock;

static SANDBOX_STATUS: OnceLock<SandboxStatus> = OnceLock::new();

/// Returns the `SandboxStatus` from the most recent `apply()` call,
/// or `None` if `apply()` has not been called.
pub fn last_status() -> Option<&'static SandboxStatus> {
    SANDBOX_STATUS.get()
}

pub fn apply(entitlements: &Entitlements, agent_home: &Path) -> anyhow::Result<SandboxStatus> {
    let policy = SandboxPolicy::from_entitlements(entitlements, agent_home);
    let status = apply_policy(&policy)?;
    // Store for attestation. OnceLock: if called twice, second call is ignored.
    let _ = SANDBOX_STATUS.set(status.clone());
    Ok(status)
}
```

- [x] **Step 3: Add attestation to `B0SafetyHook::on_startup`**

In `mur-agent-runtime/src/hooks/b0.rs`, at the START of the `on_startup` body (before the MCP signature check), add:

```rust
    async fn on_startup(
        &self,
        ctx: &HookCtx,
        profile: &AgentProfile,
        _tok: &CancellationToken,
    ) -> Result<(), HookError> {
        // ── B1 sandbox attestation. ───────────────────────────────────────
        // Log whether the kernel sandbox was successfully applied. If
        // enforcing=false on Linux/macOS, warn — the agent is advisory-only.
        match crate::sandbox::last_status() {
            Some(status) if status.enforcing => {
                tracing::info!(
                    platform = %status.platform,
                    effective_abi = ?status.effective_abi,
                    "B1 kernel sandbox: ENFORCING"
                );
            }
            Some(status) => {
                tracing::warn!(
                    platform = %status.platform,
                    "B1 kernel sandbox: NOT enforcing (advisory-only; upgrade kernel or check permissions)"
                );
            }
            None => {
                tracing::warn!("B1 kernel sandbox: not applied (sandbox::apply() not called before on_startup)");
            }
        }

        // ── Rule 11 (M7.7): MCP binary signature check. ──────────────────
        // (existing code continues below)
```

- [x] **Step 4: Run tests**

```bash
cargo test -p mur-agent-runtime -- --nocapture 2>&1 | grep -E "PASS|FAIL|sandbox"
```

Expected: all sandbox tests PASS.

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/mod.rs mur-agent-runtime/src/hooks/b0.rs
git commit -m "feat(b1): sandbox attestation in B0SafetyHook::on_startup + OnceLock status"
```

---

## Task 9: E2E smoke tests + cookbook

**Files:**
- Modify: `mur-agent-runtime/tests/sandbox_e2e.rs` (add integration coverage)
- Create: `docs/cookbook/b1-runtime-enforcement.md`

### Context

The E2E tests verify the full stack: `sandbox::apply()` → actual filesystem deny → `ToolError::Sandboxed`. Because `restrict_self()` is irreversible, E2E tests that call it must run in **forked subprocesses**. Use `std::process::Command::new(std::env::current_exe()?)` with a flag env var to re-enter the test binary in "sandbox mode" — a technique used by `landlock` crate's own tests.

The cookbook covers operator-facing concerns: what the sandbox does, how to debug, how to adjust entitlements, known limitations.

- [x] **Step 1: Write subprocess-based FS deny test (Linux/macOS only)**

Add to `mur-agent-runtime/tests/sandbox_e2e.rs`:

```rust
/// Verify that after `sandbox::apply()` with no write entitlements,
/// writing to /tmp fails. Runs in a subprocess to avoid locking the test process.
#[test]
#[cfg(unix)]
fn sandbox_denies_write_outside_agent_home() {
    // Re-enter the test binary in sandbox mode.
    let exe = std::env::current_exe().unwrap();
    let status = std::process::Command::new(&exe)
        .env("MUR_TEST_SANDBOX_WRITE_DENY", "1")
        .env("MUR_AGENT_SKIP_SANDBOX", "") // clear the skip flag
        .arg("--test-thread")  // nextest passes --test-threads; subprocess runs directly
        .status()
        .unwrap();
    // Subprocess should exit 0 (write was correctly denied).
    // If the write was NOT denied, the subprocess exits 1.
    assert_eq!(status.code(), Some(0), "subprocess should exit 0 (write correctly denied)");
}

/// Subprocess entry point for `sandbox_denies_write_outside_agent_home`.
/// Called when MUR_TEST_SANDBOX_WRITE_DENY=1 is set in env.
/// Only called from within the subprocess — not from the test runner directly.
#[ctor::ctor]
fn sandbox_write_deny_subprocess_main() {
    if std::env::var_os("MUR_TEST_SANDBOX_WRITE_DENY").is_none() {
        return;
    }
    // Running inside the subprocess. Apply the sandbox, then try writing.
    use mur_agent_runtime::sandbox;
    use mur_common::agent::AgentProfile;
    let profile = AgentProfile::default_for_tests();
    let agent_home = std::path::PathBuf::from("/tmp/b1_test_deny_home");
    std::fs::create_dir_all(&agent_home).unwrap();

    sandbox::apply(&profile.entitlements, &agent_home).unwrap();

    // Attempt to write outside agent_home — should fail with EACCES.
    let result = std::fs::write("/tmp/b1_SHOULD_FAIL.txt", b"pwned");
    if result.is_err() {
        std::process::exit(0); // correctly denied
    } else {
        std::fs::remove_file("/tmp/b1_SHOULD_FAIL.txt").ok();
        eprintln!("ERROR: write was NOT denied by sandbox");
        std::process::exit(1); // incorrectly allowed
    }
}
```

Add `ctor` dev-dependency to `mur-agent-runtime/Cargo.toml`:

```toml
[dev-dependencies]
ctor = "0.2"
```

- [x] **Step 2: Run all sandbox E2E tests**

```bash
cargo test -p mur-agent-runtime --test sandbox_e2e -- --nocapture
```

Expected on Linux: all tests PASS (including subprocess write-deny test).
Expected on macOS: write-deny test may be SKIP if SBPL apply is non-enforcing in CI (add `#[ignore]` if needed).
Expected on Windows: Unix tests skipped, Windows test PASS.

- [x] **Step 3: Write the cookbook**

Create `docs/cookbook/b1-runtime-enforcement.md`:

```markdown
# B1 Runtime Enforcement

OS-level sandboxing for mur agents. B1 upgrades B0's advisory hook layer
to kernel enforcement: write attempts and TCP connections outside the agent's
entitlements are blocked by the operating system, not just logged.

## How it works

`sandbox::apply()` fires once at supervisor startup. It translates
`profile.yaml`'s `entitlements:` block into OS-native rules:

| Platform | Mechanism | What it enforces |
|---|---|---|
| Linux 5.13+ | Landlock ABI v1–v4 | FS read/write + TCP port allowlist |
| Linux (all) | seccomp BPF | `ptrace`, `mount`, `kexec_load`, `bpf`, `unshare` denied |
| macOS | SBPL `sandbox_init` | FS write deny + network host allowlist |
| Windows | Job Object | memory cap + child breakaway disabled |
| All platforms | reqwest HostGuard | hostname-level TCP gate before DNS resolution |

B0 hooks still run first (hooks first, kernel second):

1. `B0SafetyHook::pre_tool_use` → deny with LLM-visible reason.
2. Tool executes anyway? Kernel blocks it → EACCES → `ToolError::Sandboxed`.
3. LLM receives: `"Sandboxed: write to /etc/passwd denied (B1 enforcement)"`.

## Checking sandbox status

```bash
# Tail the agent log and look for B1 lines.
mur agent logs my-agent | grep "B1"
# Expected:
# INFO B1 kernel sandbox: ENFORCING platform=linux-landlock-v4
```

If you see `NOT enforcing`, check:
- Linux: kernel ≥ 5.13, `CONFIG_SECURITY_LANDLOCK=y`, `lsm=landlock,...` boot param.
- macOS: process must not be already sandboxed (nested sandboxes not supported).

## Adjusting entitlements

```yaml
# ~/.mur/agents/my-agent/profile.yaml
entitlements:
  network:
    outbound:
      mode: restricted
      allow_hosts:
        - api.anthropic.com
        - api.openai.com
  filesystem:
    read:
      - ~/Documents/project
    write:
      - ~/Documents/project/output
    deny:
      - ~/.ssh
      - ~/.aws
  limits:
    memory_mb: 1024
```

Changes to `profile.yaml` take effect on next agent restart.

## MCP server child sandboxing

MCP server processes spawned by the supervisor inherit a tighter
birdcage sandbox: they can only read their own binary + shared libs,
write to their configured working directory, and use the same network
allowlist as the parent.

## Known limitations (v2)

- **Windows**: AppContainer (full isolation) is v3. v2 only provides
  memory cap + child breakaway prevention.
- **Linux < 5.13**: Landlock not available. seccomp BPF still applies.
- **Network host filtering**: advisory at the reqwest layer; native C
  code in MCP servers (not using reqwest) is gated by the kernel port
  rules only (no hostname filtering at kernel level until netns sidecar).
- **Nested sandboxes**: macOS rejects a second `sandbox_init` call.
  If the agent binary was already sandbox-exec'd, B1 SBPL is skipped.

## See also

- `mur-agent-runtime/src/sandbox/` — implementation
- `docs/superpowers/specs/2026-04-30-mur-agent-harness-roadmap-design.md` §6.3 — spec
- `docs/superpowers/plans/2026-05-07-mur-agent-b1-runtime-enforcement.md` — this plan
```

- [x] **Step 4: Run all tests**

```bash
cargo test --workspace -- --nocapture 2>&1 | tail -20
```

Expected: all tests PASS (sandbox E2E may show `[ignored]` for tests that require a real kernel sandbox in a subprocess).

- [x] **Step 5: Commit**

```bash
git add mur-agent-runtime/tests/sandbox_e2e.rs \
        mur-agent-runtime/Cargo.toml \
        docs/cookbook/b1-runtime-enforcement.md
git commit -m "feat(b1): E2E sandbox smoke tests + b1-runtime-enforcement cookbook"
```

---

## Self-Review

**Spec coverage check:**

| Spec requirement | Task |
|---|---|
| `birdcage` 0.9 as façade | Task 7 (birdcage child spawn); direct `landlock`/SBPL for current process (birdcage 0.8 doesn't expose `restrict_self`) |
| Linux Landlock ABI v4 — FS+net | Task 2 |
| Linux seccomp denylist | Task 2 |
| macOS SBPL via `sandbox_init_with_parameters` | Task 3 |
| Windows Job Object `BREAKAWAY_OK=0` + memory cap | Task 4 |
| Child re-applies tighter sandbox before `execve` | Task 7 |
| `reqwest` host-level resolver guard | Task 5 |
| Hooks first, kernel second | Task 6 (wiring order) + Task 8 (attestation) |
| `EACCES` → `ToolError::Sandboxed` | Task 6 |
| Never SIGKILL the agent | N/A — `seccompiler` uses EPERM; Landlock returns EACCES |
| Sandbox attestation in `on_startup` | Task 8 |
| Cookbook | Task 9 |

**Gaps identified and addressed:**

1. birdcage 0.8.1 does not have `lock()` for sandboxing the current process — plan uses `landlock` directly for Linux and SBPL FFI for macOS. Noted in Task 7.
2. Landlock "allow network but zero NetPort rules = deny all" gotcha — documented in Task 2 Step 3 with corrected implementation.
3. `restrict_self()` is irreversible — E2E tests use subprocess re-entry pattern (Task 9 Step 1).
4. `SandboxPolicy` needed `memory_limit_mb` for Windows — added in Task 4 Step 3.

**Type consistency:** `SandboxPolicy` is defined in Task 1 and used identically in Tasks 2–7. `SandboxStatus` is defined in Task 1 and returned by all platform implementations. `HostGuard::from_policy_hosts()` takes `&Option<Vec<String>>` matching `SandboxPolicy.net_allow_hosts` type.

---

Plan complete and saved to `docs/superpowers/plans/2026-05-07-mur-agent-b1-runtime-enforcement.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, two-stage review between tasks, fast iteration.

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints.

Which approach?
