//! What a computed policy lets the agent exec, and whether its seal keeps the
//! signing key from the agent's own programs: the two facts the SSH setup
//! card shows and acts on (#2344). Both read the same policy the kernel
//! backends install, so the card cannot describe a different seal.

use super::*;

/// Standard macOS binary locations `Allowlist`/`None` re-allow for the shell
/// and coreutils. `Strict` skips them. Shared with the SBPL builder
/// (`sandbox::macos`), so the preview and the profile cannot disagree.
pub const MACOS_SYSTEM_EXEC_PATHS: &[&str] = &["/bin", "/usr/bin", "/usr/lib"];

/// Which kernel backend's rules to describe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExecBackend {
    /// SBPL: `process-exec*` clauses only; read/write never carry exec.
    Seatbelt,
    /// Landlock: `Execute` on `exec_grants()`, plus every read and write
    /// grant unless the mode is `Strict` (#2343).
    Landlock,
}

impl ExecBackend {
    /// The backend this build installs, if any.
    pub fn current() -> Option<Self> {
        if cfg!(target_os = "macos") {
            Some(Self::Seatbelt)
        } else if cfg!(target_os = "linux") {
            Some(Self::Landlock)
        } else {
            None
        }
    }
}

/// Everything a policy lets the agent exec on one backend. Empty for
/// `SpawnMode::Any`, which is unrestricted and says so via `unrestricted`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecReach {
    pub unrestricted: bool,
    /// Directories: any program beneath them may run.
    pub dirs: Vec<PathBuf>,
    /// Individual executables.
    pub files: Vec<PathBuf>,
    /// Landlock only: the read/write grants also carry `Execute`, so a
    /// program the agent writes into a writable grant can run.
    pub rw_exec: bool,
}

fn push(v: &mut Vec<PathBuf>, p: &Path) {
    if !v.iter().any(|x| x == p) {
        v.push(p.to_path_buf());
    }
}

impl SandboxPolicy {
    /// Mirrors `sandbox::macos::build_sbpl_profile`'s `process-exec*`
    /// clauses and `sandbox::linux`'s Execute rules. Keep in lockstep.
    pub fn exec_reach(&self, backend: ExecBackend) -> ExecReach {
        if self.spawn_mode == SpawnMode::Any {
            return ExecReach {
                unrestricted: true,
                ..ExecReach::default()
            };
        }
        let mut r = ExecReach::default();
        match backend {
            ExecBackend::Seatbelt => {
                if self.spawn_mode != SpawnMode::Strict {
                    for p in MACOS_SYSTEM_EXEC_PATHS {
                        push(&mut r.dirs, Path::new(p));
                    }
                }
                for p in self.fs_exec.iter().chain(&self.spawn_allowed_prefixes) {
                    push(&mut r.dirs, p);
                }
                for p in &self.spawn_allowed_paths {
                    push(&mut r.files, p);
                }
            }
            ExecBackend::Landlock => {
                // A literal in exec_grants is a file rule (so is each ELF
                // interpreter it adds); a prefix or fs_exec entry is a
                // directory rule. Classified by what the policy said each was.
                for p in self.exec_grants() {
                    if self.fs_exec.contains(&p) || self.spawn_allowed_prefixes.contains(&p) {
                        push(&mut r.dirs, &p);
                    } else {
                        push(&mut r.files, &p);
                    }
                }
                if self.landlock_read_write_exec() {
                    r.rw_exec = true;
                    for p in self.fs_read.iter().chain(&self.fs_write) {
                        push(&mut r.dirs, p);
                    }
                }
            }
        }
        r
    }

    /// Whether this backend's seal keeps the agent's own programs from
    /// reading its signing key (`identity.key`), so a HITL decision signed
    /// with that key can only have come from the runtime acting on a human
    /// answer. SBPL re-asserts a read deny on it after the own-home re-allow;
    /// Landlock cannot carve a file out of the own home it force-grants, so
    /// it never qualifies.
    pub fn signing_key_isolated(backend: Option<ExecBackend>) -> bool {
        backend == Some(ExecBackend::Seatbelt)
    }
}

#[cfg(test)]
#[path = "exec_reach_tests.rs"]
mod tests;
