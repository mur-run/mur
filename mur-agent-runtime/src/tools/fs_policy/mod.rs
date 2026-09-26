//! Shared call-time filesystem-entitlement gates (issue #591 PR2). `deny`
//! always wins; writes require a `write` grant, reads a `read` or `write`
//! grant. `read_file` and the project-instruction loader share the read gate.

use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};

use mur_common::agent::FilesystemEntitlement;

use crate::tools::ToolError;

/// Per-conversation working directory shared by the `bash` tool and the file
/// tools (`read_file`/`write_file`/`edit_file`), so a relative path resolves
/// against the same base no matter which tool the agent reached for.
///
/// Dogfood bug: `bash pwd` showed one directory while `read_file rel/path`
/// resolved against `agent_home`, because the two tools never shared a base.
/// The `bash` tool updates this only when it is given an explicit `cwd`
/// argument; a `cd` *inside* a spawned subprocess cannot be observed by the
/// parent and is deliberately NOT tracked (that's called out in both tools'
/// descriptions).
///
/// Keyed by turn, not held once per agent. Dogfood bug #2: one value per
/// process meant two murmur sessions on one agent took turns overwriting it,
/// so a session in `gateway/` silently started working in `mur/` the moment
/// another session spoke. Each turn gets its own slot
/// ([`Self::begin_turn`]), inherited from the turn it continues; a tool call
/// finds its slot through the `CURRENT_TASK_ID` scope every execute site
/// already sets — in-process and CLI-shim alike.
#[derive(Clone)]
pub struct SessionCwd {
    /// Where a conversation with no directory of its own starts.
    home: Arc<PathBuf>,
    turns: Arc<RwLock<TurnCwds>>,
}

/// Turn slots kept before the oldest is dropped. Well above the runner's
/// conversation cap, so the latest turn of every live conversation survives;
/// a conversation older than that falls back to the home, exactly as it does
/// after a restart.
const MAX_TURN_CWDS: usize = 1_024;

#[derive(Default)]
struct TurnCwds {
    by_turn: std::collections::HashMap<String, PathBuf>,
    /// Insertion order, for eviction past [`MAX_TURN_CWDS`].
    order: std::collections::VecDeque<String>,
    /// A tool called outside any turn (direct tool tests, tooling): kept apart
    /// so it can never leak into a conversation.
    unscoped: Option<PathBuf>,
}

impl SessionCwd {
    /// Create the cwd table; `home` (the agent home) is every new
    /// conversation's starting directory.
    pub fn new(home: PathBuf) -> Self {
        Self {
            home: Arc::new(home),
            turns: Arc::default(),
        }
    }

    /// Open turn `id`'s slot: the caller's entitled `requested` directory,
    /// else the directory of the turn it continues (`parent`), else the home.
    /// Never another conversation's.
    pub fn begin_turn(&self, id: &str, parent: Option<&str>, requested: Option<PathBuf>) {
        let dir = requested
            .or_else(|| parent.and_then(|p| self.lookup(p)))
            .unwrap_or_else(|| self.home.as_ref().clone());
        self.insert(id, dir);
    }

    /// Turn `id`'s directory, or the home when it has none.
    pub fn for_turn(&self, id: &str) -> PathBuf {
        self.lookup(id)
            .unwrap_or_else(|| self.home.as_ref().clone())
    }

    /// The calling tool's directory. Clones and releases the lock at once, so
    /// callers never hold a guard across `.await`.
    pub fn current(&self) -> PathBuf {
        match crate::tools::bash_jobs::current_task_id() {
            Some(id) => self.for_turn(&id),
            None => self
                .read()
                .unscoped
                .clone()
                .unwrap_or_else(|| self.home.as_ref().clone()),
        }
    }

    /// Move the calling tool's directory (`bash` with an explicit `cwd`).
    /// Only this conversation moves; later turns of it inherit the change.
    pub fn set(&self, dir: PathBuf) {
        match crate::tools::bash_jobs::current_task_id() {
            Some(id) => self.insert(&id, dir),
            None => self.write().unscoped = Some(dir),
        }
    }

    fn lookup(&self, id: &str) -> Option<PathBuf> {
        self.read().by_turn.get(id).cloned()
    }

    fn insert(&self, id: &str, dir: PathBuf) {
        let mut t = self.write();
        if t.by_turn.insert(id.to_string(), dir).is_none() {
            t.order.push_back(id.to_string());
        }
        while t.order.len() > MAX_TURN_CWDS {
            if let Some(old) = t.order.pop_front() {
                t.by_turn.remove(&old);
            }
        }
    }

    fn read(&self) -> std::sync::RwLockReadGuard<'_, TurnCwds> {
        self.turns.read().unwrap_or_else(|e| e.into_inner())
    }

    fn write(&self) -> std::sync::RwLockWriteGuard<'_, TurnCwds> {
        self.turns.write().unwrap_or_else(|e| e.into_inner())
    }
}

/// The accepted `path` forms, worded for tool schemas. Single source of truth
/// for every path-taking tool's parameter description — do NOT re-word it per
/// tool.
///
/// It lives beside `resolve_path` because it documents exactly what that
/// function accepts. A schema advertising fewer forms than the resolver
/// implements is not a cosmetic gap: it made the model expand `~` itself, and
/// since nothing tells it what `~` is, it invented a username and wrote to
/// `/Users/i/` and `/Users/lidj/` while the real home was `/Users/david`.
/// The system prompt's output-locations rule tells agents to write under
/// `~/.mur/artifacts/`, so hiding `~` here put two MUR-authored strings in
/// direct contradiction. Enforced by `tools::tests::path_taking_tools_advertise_tilde`.
pub(crate) const PATH_FORMS: &str = "absolute, `~`-relative (expanded to your real home — write `~` literally, \
     never guess a home path), or relative to the session cwd";

/// Resolve a tool-supplied path: expand a leading `~`/`~/` to the user's
/// home, keep absolute paths as-is, and join relative paths onto
/// `working_dir`. Entitlement checks run on the canonicalized result,
/// so expansion never widens what a grant covers.
pub(crate) fn resolve_path(working_dir: &Path, raw: &str) -> PathBuf {
    if let Some(home) = dirs::home_dir() {
        if raw == "~" {
            return home;
        }
        if let Some(rest) = raw.strip_prefix("~/") {
            return home.join(rest);
        }
    }
    if Path::new(raw).is_absolute() {
        PathBuf::from(raw)
    } else {
        working_dir.join(raw)
    }
}

/// Re-export of the canonical guidance string, which now lives in
/// `mur-common` so `mur agent doctor` and the runtime tools share one source
/// of truth (issue #1). Do NOT inline the wording here — keep it a re-export.
pub use mur_common::REMOVABLE_VOLUME_EPERM_HINT;

/// True when `err` is an EPERM ("Operation not permitted", raw `os error 1`)
/// against a path under `/Volumes/*`. This is the exact macOS Full-Disk-Access
/// failure mode on removable/external volumes — distinct from a plain
/// `PermissionDenied` (EACCES) so we don't hijack ordinary permission errors.
pub fn is_removable_volume_eperm(path: &Path, err: &std::io::Error) -> bool {
    let is_eperm = err.raw_os_error() == Some(1);
    is_eperm && path.starts_with("/Volumes/")
}

/// Format an I/O error message, appending [`REMOVABLE_VOLUME_EPERM_HINT`] when
/// the failure is the macOS Full-Disk-Access EPERM on a `/Volumes/*` path.
/// `base` (the resolution base) is preserved verbatim so existing "relative to
/// session cwd" diagnostics stay intact. Used by every file tool's error path.
pub fn format_io_error(verb: &str, path: &Path, base: &Path, err: &std::io::Error) -> String {
    let mut msg = format!(
        "cannot {verb} {}: {err} (relative to session cwd {})",
        path.display(),
        base.display()
    );
    if is_removable_volume_eperm(path, err) {
        msg.push_str("\n\n");
        msg.push_str(REMOVABLE_VOLUME_EPERM_HINT);
    }
    msg
}

/// Adapt a raw profile entitlement for the file tools: grant the agent its own
/// home, then carve the self-protected files back out.
///
/// **Why the write grant is here and not in the profile.** The sandbox builder
/// force-grants `agent_home` unconditionally ("runtime cannot function without
/// it" — [`crate::sandbox::policy::SandboxPolicy::from_entitlements`]), but the
/// file tools were handed `profile.entitlements.filesystem` verbatim. So the
/// kernel allowed a write the tool gate refused, and an agent whose profile did
/// not happen to list its own home could not write there at all:
/// `path not write-entitled: ~/.mur/agents/<name>/… (grant it via mur agent
/// perm allow-write)`. Observed 2026-09-13, where it pushed the agent to reach
/// for `/tmp` instead and trip the tool-withdrawal path.
///
/// **Why only `agent_home`.** The sandbox force-grants three more paths —
/// `<mur_home>/channels`, `<mur_home>/index/channels`, `open-items.jsonl` —
/// and those deliberately stay out. The two layers answer different questions:
/// the kernel policy bounds what this PROCESS may write (the runtime appends
/// signed channel events itself), while this gate bounds what the MODEL may
/// write through `write_file`/`edit_file`. Aligning the lists wholesale would
/// hand a prompt-injected agent a way to forge channel events and corrupt the
/// channels read-model. `open_item` does not route through this gate at all.
///
/// Issue #712: the agent's own `profile.yaml` and `identity.key` are appended
/// to the deny list, so the gate refuses them even under a grant covering the
/// whole agent dir — including the one added just above. `deny` is checked
/// before `write` in [`check_write_entitlement`], so the carve-out wins.
/// On Linux this gate is the enforcement point (Landlock cannot express
/// deny-within-allow); on macOS it fronts the SBPL kernel deny with a clear
/// error instead of a raw EPERM.
///
/// Issue #007: this list is consulted by the READ gate too (`deny` is one
/// list), which made the agent's own `profile.yaml` unreadable — while the
/// sibling profile next door stayed readable, so the rule bought no
/// confidentiality and only cost the agent the ability to answer "what am I
/// allowed to do?". Reads are now carved back via
/// [`self_protected_write_only`]: `identity.key` remains read-denied through
/// `LaunchChain::protects_read` (signing authority), `profile.yaml` does not.
pub(crate) fn for_file_tools(
    mut fs: FilesystemEntitlement,
    agent_home: &Path,
) -> FilesystemEntitlement {
    let home = agent_home.to_string_lossy().into_owned();
    if !fs.write.contains(&home) {
        fs.write.push(home);
    }
    // `<mur_home>/artifacts/<agent>`: the sandbox grants it (see
    // `from_entitlements`) because the system prompt's output-locations rule
    // sends every agent there for reports and scratch output. Unlike the other
    // runtime-owned grants this one is FOR the model, so it belongs in this
    // gate too — otherwise the kernel allows the write and `write_file`
    // refuses it, which is how an agent ended up probing `/tmp` instead.
    if let (Some(mur_home), Some(agent_name)) = (
        agent_home.parent().and_then(|p| p.parent()),
        agent_home.file_name(),
    ) {
        let mine = mur_home
            .join("artifacts")
            .join(agent_name)
            .to_string_lossy()
            .into_owned();
        if !fs.write.contains(&mine) {
            fs.write.push(mine);
        }
    }
    for f in crate::sandbox::policy::SELF_PROTECTED_AGENT_FILES {
        let p = agent_home.join(f).to_string_lossy().into_owned();
        if !fs.deny.contains(&p) {
            fs.deny.push(p);
        }
    }
    fs
}

/// True when `canonical` falls under any of `roots`.
///
/// Roots go through the SAME `~` expansion the sandbox builder applies
/// ([`crate::sandbox::policy::expand_entitlement_path`]) before
/// canonicalization. Without it the two layers disagreed about one grant:
/// the kernel policy expanded `~/...` and let the access through, while this
/// gate compared a literal `~/...` root (whose `canonicalize` always fails,
/// falling back to the un-expanded string) and answered "not entitled".
/// A `~`-written *deny* entry — the very form `detect_warnings` suggests for
/// `~/.ssh` — was silently inert here for the same reason.
///
/// Windows note: `canonicalize` returns a `\\?\` UNC path and succeeds only
/// for a path that exists, so a present root canonicalizes to a different
/// spelling than an absent one. Callers pass an already-canonical path, so a
/// live grant matches; a grant naming a missing path fails closed — and
/// `mur agent perm` rejects those up front (`reject_dead_grant`).
pub(crate) fn under_any(roots: &[String], canonical: &Path) -> bool {
    roots.iter().any(|r| {
        let expanded = crate::sandbox::policy::expand_entitlement_path(r);
        let root = std::fs::canonicalize(&expanded).unwrap_or(expanded);
        canonical.starts_with(&root)
    })
}

/// Allow-side membership test: `under_any`, plus one derived hop for a path
/// that lives in a git worktree of a granted checkout (issue #004).
///
/// ## Why this is a separate function, and why `deny` must never call it
///
/// Prefix matching cannot see that `~/work/repo-feat` and `~/work/repo` are the
/// same project, so a user who granted the repo they work in was refused the
/// moment the work moved into a worktree, and had to grant the same repo a
/// second time under a different path. That is the whole of #004.
///
/// The derivation is one-way ALLOW-side only. Applying it to `deny` would be
/// fail-open in the most dangerous direction: `deny ~/secrets` must keep
/// meaning exactly `~/secrets`, and "this path's main checkout is denied"
/// widening into "so is every worktree" is a rule the user never wrote. Deny
/// stays literal, and because `check_write_entitlement`/`check_entitlement`
/// evaluate `deny` FIRST and unconditionally, a denied path inside a derived
/// worktree grant is still refused.
///
/// The derived root is never written back to `profile.yaml`: an entitlement
/// the user did not type must not silently appear in the file they audit.
/// It is recomputed from git's on-disk metadata on every call, so removing the
/// worktree removes the access with no stale grant left behind.
pub(crate) fn under_any_or_worktree(roots: &[String], canonical: &Path) -> bool {
    if under_any(roots, canonical) {
        return true;
    }
    // Not directly granted — ask git whether this path is a worktree of
    // something that IS granted. Read-only, no subprocess (this gate is what
    // decides whether spawning is permitted in the first place).
    match mur_common::worktree::main_checkout_of(canonical) {
        Some(main) => {
            let main = std::fs::canonicalize(&main).unwrap_or(main);
            under_any(roots, &main)
        }
        None => false,
    }
}

/// The self-protected files whose deny is WRITE-only (issue #007).
///
/// `for_file_tools` pushes `SELF_PROTECTED_AGENT_FILES` onto `fs.deny`, and
/// `deny` is one list shared by both gates — so the #712 write rule silently
/// became a read rule too. This names the subset the READ gate must skip.
///
/// `identity.key` is deliberately absent: it stays read-denied, but through
/// `LaunchChain::protects_read`, which sits *before* the lists and cannot be
/// satisfied by any entitlement. That is the right layer for "reading this is
/// holding a credential"; the deny list is not, because a user-written grant
/// could otherwise be argued to override it.
fn self_protected_write_only(agent_home: &Path) -> Vec<PathBuf> {
    vec![agent_home.join("profile.yaml")]
}

/// Deny-list membership for the READ gate: `under_any`, minus the entries that
/// exist only to express a WRITE rule (issue #007).
///
/// Takes `agent_home` rather than filtering by file name so a user who
/// explicitly wrote `deny <some other agent>/profile.yaml` keeps that deny —
/// only THIS agent's own profile is carved back out.
pub(crate) fn under_any_read_deny(roots: &[String], canonical: &Path, agent_home: &Path) -> bool {
    let carved: Vec<PathBuf> = self_protected_write_only(agent_home)
        .into_iter()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect();
    if carved.iter().any(|p| p == canonical) {
        return false;
    }
    under_any(roots, canonical)
}

pub(crate) fn check_write_entitlement(
    fs: &FilesystemEntitlement,
    canonical: &Path,
    chain: &crate::sandbox::launch_chain::LaunchChain,
) -> Result<(), ToolError> {
    // Checked first and unconditionally: no entitlement can satisfy this, and
    // the kernel's bare EPERM reads the same as "not granted", so this is the
    // only layer that can say which of the two happened.
    if let Some(reason) = chain.protects_write(canonical) {
        return Err(ToolError::Execution(format!(
            "path is part of MUR's launch chain and can never be written: {} ({reason})",
            canonical.display()
        )));
    }
    if under_any(&fs.deny, canonical) {
        return Err(ToolError::Execution(format!(
            "path denied by entitlement: {}",
            canonical.display()
        )));
    }
    // `under_any_or_worktree` tries the literal grants first, then one derived
    // hop for a worktree of a granted checkout (#004).
    if under_any_or_worktree(&fs.write, canonical) {
        return Ok(());
    }
    Err(ToolError::Execution(format!(
        "path not write-entitled: {} (grant it via `mur agent perm allow-write`)",
        canonical.display()
    )))
}

/// Why a read was refused — a bare token, never a path or error text.
///
/// The project-instructions block renders refusals into the prompt, and must
/// not leak a path through an error string (spec §5.1), so this carries no
/// data and has no `Display`. The human-facing strings live in
/// [`check_read_entitlement`], which is a presentation of the same decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadRefusal {
    /// Part of MUR's launch chain; no entitlement can grant it.
    LaunchChain,
    /// Under an explicit `deny` root.
    DenyList,
    /// Outside every `read` and `write` root.
    NoGrant,
}

/// The one read decision: `Err` carries the refusal and, for the launch
/// chain, the reason the chain gave (used only by the string presentation).
fn decide_read(
    fs: &FilesystemEntitlement,
    canonical: &Path,
    chain: &crate::sandbox::launch_chain::LaunchChain,
) -> Result<(), (ReadRefusal, &'static str)> {
    if let Some(reason) = chain.protects_read(canonical) {
        return Err((ReadRefusal::LaunchChain, reason));
    }
    if under_any_read_deny(&fs.deny, canonical, chain.agent_self_home()) {
        return Err((ReadRefusal::DenyList, ""));
    }
    // `under_any_or_worktree` tries the literal grants first, then one derived
    // hop for a worktree of a granted checkout (#004).
    if under_any_or_worktree(&fs.read, canonical) || under_any_or_worktree(&fs.write, canonical) {
        return Ok(());
    }
    Err((ReadRefusal::NoGrant, ""))
}

/// Typed read gate: the same decision as [`check_read_entitlement`], for
/// callers that must not see error text (the project-instructions loader).
pub(crate) fn check_read_refusal(
    fs: &FilesystemEntitlement,
    canonical: &Path,
    chain: &crate::sandbox::launch_chain::LaunchChain,
) -> Result<(), ReadRefusal> {
    decide_read(fs, canonical, chain).map_err(|(r, _)| r)
}

/// Read-side twin of [`check_write_entitlement`], and the one gate for every
/// read of a user path the runtime makes on the model's behalf: `read_file`,
/// and the loader for a project's `AGENTS.md` / `CLAUDE.md`. Both go through
/// [`decide_read`] — this function and [`check_read_refusal`] are two
/// presentations of one decision, so they can never disagree about what is
/// readable.
///
/// Order matches the write gate: the launch chain first (no entitlement can
/// satisfy it — another agent's signing key is enough to forge its events),
/// then `deny`, then the grants. Write implies read-back, so both lists count.
pub(crate) fn check_read_entitlement(
    fs: &FilesystemEntitlement,
    canonical: &Path,
    chain: &crate::sandbox::launch_chain::LaunchChain,
) -> Result<(), ToolError> {
    decide_read(fs, canonical, chain).map_err(|(refusal, reason)| {
        let path = canonical.display();
        ToolError::Execution(match refusal {
            ReadRefusal::LaunchChain => format!(
                "path is part of MUR's launch chain and can never be read: {path} ({reason})"
            ),
            ReadRefusal::DenyList => format!("path denied by entitlement: {path}"),
            ReadRefusal::NoGrant => {
                format!("path not entitled: {path} (grant it via `mur agent perm allow-read`)")
            }
        })
    })
}

#[cfg(test)]
mod tests;
