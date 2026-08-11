# Launch-Chain Protection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make MUR's own launch chain — the files deciding what starts next and with what authority — structurally unwritable from inside any agent sandbox, and unreadable where reading is equally fatal.

**Architecture:** A new `sandbox/launch_chain.rs` module owns the protected set as a *predicate* over paths, derived from `mur_home` / `bin_dir` / `$HOME` rather than enumerated. Four enforcement layers consume it: the cross-OS tool gate (legible errors, closed under agents created after seal), macOS SBPL (three-tier last-match-wins ordering), Linux Landlock (fail-closed grant drop, because Landlock cannot express deny-within-allow), and `mur agent perm` (refuse before it reaches the profile). `runtime-doctor` reports grants the set now neutralises; nothing is silently rewritten.

**Tech Stack:** Rust edition 2024, `cargo nextest`, macOS SBPL (TinyScheme), Linux Landlock ABI V4 via the `landlock` crate.

## Global Constraints

- **Depends on PR #922.** `mur_agent_runtime::sandbox::policy::expand_entitlement_path` is introduced there. Branch from `main` only after #922 merges, or from its branch.
- **`policy.rs` is 1649 lines — already over the CLAUDE.md 800-line limit.** Do not add to it. New logic goes in `sandbox/launch_chain.rs`.
- **No hardcoded values.** `bin_dir` is `MUR_AGENT_BIN_DIR` when set, else `~/.local/bin`. `mur_home` is derived as `agent_home.parent().parent()`, the existing idiom at `policy.rs:279`.
- **Never widen an existing install silently.** No task rewrites a user's `profile.yaml`.
- **Every protection test carries a negative control.** A test that passes because the code path never ran is indistinguishable from one that passes because the protection works.
- **Test runner is `cargo nextest`, not `cargo test`.** `RUST_MIN_STACK` is already set in `.cargo/config.toml`.
- **Export `CARGO_TARGET_DIR=<main checkout>/target` for every cargo command** if working in a worktree. Each worktree otherwise grows its own multi-gigabyte target directory; the machine this was written on had 18 GiB free against 51 GiB of existing build artifacts. Dependency artifacts are shared, so only workspace crates rebuild.
- Brand name in user-facing strings is uppercase **MUR**.

---

## File Structure

| File | Responsibility |
|---|---|
| `mur-agent-runtime/src/sandbox/launch_chain.rs` **(new)** | The protected set: construction from an `agent_home`, `protects_write`/`protects_read` predicates, `deny_paths()` for SBPL, `is_overbroad_grant_root()`. Owns every rule; no other file decides what is protected. |
| `mur-agent-runtime/src/sandbox/mod.rs` | Register the module. |
| `mur-agent-runtime/src/tools/fs_policy.rs` | Call `protects_write` before the entitlement lists. |
| `mur-agent-runtime/src/tools/read_file.rs` | Call `protects_read` before the entitlement lists. |
| `mur-agent-runtime/src/sandbox/policy.rs` | Carry a `LaunchChain` and a `dropped_grants` field on `SandboxPolicy`. Construction only — no rules. |
| `mur-agent-runtime/src/sandbox/macos.rs` | Emit the three-tier deny/re-allow/deny ordering. |
| `mur-agent-runtime/src/sandbox/linux.rs` | Drop write grants that contain a protected path; record them. |
| `mur-core/src/cmd/agent/perm.rs` | Refuse protected and overbroad paths at grant time. |
| `mur-core/src/cmd/agent/doctor.rs` | Report neutralised grants. |

---

### Task 1: The protected set

**Files:**
- Create: `mur-agent-runtime/src/sandbox/launch_chain.rs`
- Modify: `mur-agent-runtime/src/sandbox/mod.rs:3` (add `pub mod launch_chain;`)

**Interfaces:**
- Consumes: `mur_agent_runtime::sandbox::policy::expand_entitlement_path` (from #922).
- Produces:
  - `pub struct LaunchChain { … }` (`Clone`, `Debug`)
  - `pub fn LaunchChain::new(agent_home: &Path) -> LaunchChain`
  - `pub fn LaunchChain::protects_write(&self, path: &Path) -> Option<&'static str>` — `Some(reason)` when protected
  - `pub fn LaunchChain::protects_read(&self, path: &Path) -> Option<&'static str>`
  - `pub fn LaunchChain::deny_paths(&self) -> Vec<PathBuf>`
  - `pub fn LaunchChain::agent_self_home(&self) -> &Path`
  - `pub fn is_overbroad_grant_root(path: &Path, home: &Path) -> bool`

Returning `Option<&'static str>` rather than `bool` is deliberate: layer 1's whole reason to exist is a legible error, and the reason string is what makes it legible.

- [ ] **Step 1: Write the failing tests**

```rust
// at the bottom of mur-agent-runtime/src/sandbox/launch_chain.rs
#[cfg(test)]
mod tests {
    use super::*;

    /// `<tmp>/agents/mur` as agent_home, so mur_home is `<tmp>`.
    fn chain(tmp: &Path) -> LaunchChain {
        LaunchChain::for_test(
            &tmp.join("agents").join("mur"),
            &tmp.join("bin"),
            &tmp.join("home"),
        )
    }

    #[test]
    fn sibling_agent_files_are_write_protected_but_own_are_left_to_self_protect() {
        let tmp = tempfile::tempdir().unwrap();
        let c = chain(tmp.path());
        let agents = tmp.path().join("agents");

        assert!(c.protects_write(&agents.join("pm/profile.yaml")).is_some());
        assert!(c.protects_write(&agents.join("pm/identity.key")).is_some());
        assert!(c.protects_write(&agents.join("pm/anything/else")).is_some());

        // Negative control: the agent's own home stays writable. Without this,
        // a predicate that returned Some() for everything would still pass.
        assert!(c.protects_write(&agents.join("mur/running.lock")).is_none());
        assert!(c.protects_write(&agents.join("mur/skills/x.yaml")).is_none());
    }

    #[test]
    fn protects_agents_created_after_the_policy_was_built() {
        // The regression a path list cannot catch: this directory does not
        // exist, and never existed when any list would have been built.
        let tmp = tempfile::tempdir().unwrap();
        let c = chain(tmp.path());
        let unborn = tmp.path().join("agents/not-created-yet/profile.yaml");
        assert!(!unborn.exists());
        assert!(c.protects_write(&unborn).is_some());
    }

    #[test]
    fn only_murs_own_launch_artifacts_in_bin_dir_are_protected() {
        let tmp = tempfile::tempdir().unwrap();
        let c = chain(tmp.path());
        let bin = tmp.path().join("bin");

        assert!(c.protects_write(&bin.join("mur-agent-runtime")).is_some());
        assert!(c.protects_write(&bin.join("mur_agent_pm")).is_some());

        // Negative control: an agent installing a tool for itself still works.
        assert!(c.protects_write(&bin.join("ripgrep")).is_none());
        assert!(c.protects_write(&bin.join("murmur-notes")).is_none());
    }

    #[test]
    fn sibling_identity_key_is_read_protected_and_nothing_else_is() {
        let tmp = tempfile::tempdir().unwrap();
        let c = chain(tmp.path());
        let agents = tmp.path().join("agents");

        assert!(c.protects_read(&agents.join("pm/identity.key")).is_some());

        // Negative controls: reads are otherwise untouched by this module.
        assert!(c.protects_read(&agents.join("pm/profile.yaml")).is_none());
        assert!(c.protects_read(&agents.join("mur/identity.key")).is_none());
        assert!(c.protects_read(&tmp.path().join("skills/x.yaml")).is_none());
    }

    #[test]
    fn overbroad_roots_are_rejected_and_normal_project_dirs_are_not() {
        let home = PathBuf::from("/Users/someone");
        assert!(is_overbroad_grant_root(Path::new("/"), &home));
        assert!(is_overbroad_grant_root(&home, &home));
        assert!(is_overbroad_grant_root(Path::new("/Users"), &home));
        assert!(is_overbroad_grant_root(Path::new("/usr"), &home));
        assert!(is_overbroad_grant_root(Path::new("/opt/homebrew"), &home));
        assert!(is_overbroad_grant_root(Path::new("/Volumes/Disk"), &home));

        // Negative controls: real grants people legitimately make.
        assert!(!is_overbroad_grant_root(&home.join("Projects/app"), &home));
        assert!(!is_overbroad_grant_root(
            Path::new("/Volumes/Disk/Projects/app"),
            &home
        ));
    }
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo nextest run -p mur-agent-runtime -E 'test(launch_chain)'`
Expected: FAIL — `cannot find type LaunchChain in this scope`.

- [ ] **Step 3: Write the implementation**

```rust
//! MUR's own launch chain: the files that decide what starts next and with
//! what authority.
//!
//! A sandbox that can be edited from inside it is not a boundary, it is a
//! delay. The set guarded here is deliberately NOT "dangerous paths" — that
//! set is open-ended (`.zshenv`, autostart, git hooks, cron) and unwinnable.
//! It is the closed set MUR owns: what triggers a start, what gets exec'd,
//! what entitlements the started process carries, and what identity it signs
//! with. Every member is derivable from `mur_home`, `bin_dir` and `$HOME`.
//!
//! This is a predicate, not a path list, on purpose: a list built at seal
//! time cannot cover `<mur_home>/agents/<name>` for a name that did not exist
//! yet, and creating that directory is exactly the escape.

use std::path::{Path, PathBuf};

/// Written by MUR itself under `bin_dir`; exec'd before any sandbox applies.
const RUNTIME_BINARY: &str = "mur-agent-runtime";
/// BusyBox-style per-agent symlinks to `RUNTIME_BINARY`.
const AGENT_SYMLINK_PREFIX: &str = "mur_agent_";

#[derive(Clone, Debug)]
pub struct LaunchChain {
    mur_home: PathBuf,
    agent_home: PathBuf,
    bin_dir: PathBuf,
    autostart: Vec<PathBuf>,
}

impl LaunchChain {
    /// Derive the protected set for the agent rooted at `agent_home`.
    pub fn new(agent_home: &Path) -> Self {
        let home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("/tmp"));
        let bin_dir = std::env::var_os("MUR_AGENT_BIN_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/bin"));
        Self::build(agent_home, &bin_dir, &home)
    }

    /// Construct with explicit roots. Tests use this; `new` is the real path.
    pub fn for_test(agent_home: &Path, bin_dir: &Path, home: &Path) -> Self {
        Self::build(agent_home, bin_dir, home)
    }

    fn build(agent_home: &Path, bin_dir: &Path, home: &Path) -> Self {
        // `<mur_home>/agents/<name>` — the same derivation as policy.rs:279.
        let mur_home = agent_home
            .parent()
            .and_then(|p| p.parent())
            .map(Path::to_path_buf)
            .unwrap_or_else(|| agent_home.to_path_buf());
        Self {
            mur_home,
            agent_home: agent_home.to_path_buf(),
            bin_dir: bin_dir.to_path_buf(),
            autostart: autostart_dirs(home),
        }
    }

    pub fn agent_self_home(&self) -> &Path {
        &self.agent_home
    }

    /// Why `path` may never be written, or `None` if it is not in the set.
    pub fn protects_write(&self, path: &Path) -> Option<&'static str> {
        let agents = self.mur_home.join("agents");
        if path.starts_with(&agents) && !path.starts_with(&self.agent_home) {
            return Some(
                "another agent's directory — its profile.yaml is that agent's \
                 entitlements and its identity.key is that agent's signing authority",
            );
        }
        if self.is_launch_artifact(path) {
            return Some(
                "MUR's runtime binary or a per-agent symlink — exec'd before \
                 the sandbox applies, so replacing it escapes every sandbox",
            );
        }
        if self.autostart.iter().any(|d| path.starts_with(d)) {
            return Some("an OS autostart directory — entries here run outside any sandbox");
        }
        None
    }

    /// Why `path` may never be read. Only sibling signing keys: whoever reads
    /// one can forge that agent's channel events with no write at all, and
    /// verify-on-fold accepts them because the key on disk is untouched.
    pub fn protects_read(&self, path: &Path) -> Option<&'static str> {
        let agents = self.mur_home.join("agents");
        if path.starts_with(&agents)
            && !path.starts_with(&self.agent_home)
            && path.file_name().is_some_and(|n| n == "identity.key")
        {
            return Some(
                "another agent's signing key — reading it is enough to forge \
                 that agent's signed channel events",
            );
        }
        None
    }

    /// Concrete paths for backends that need a list rather than a predicate
    /// (SBPL). `<mur_home>/agents` is included as a whole; the caller is
    /// responsible for re-allowing `agent_self_home()` after it.
    pub fn deny_paths(&self) -> Vec<PathBuf> {
        let mut out = vec![self.mur_home.join("agents")];
        out.push(self.bin_dir.join(RUNTIME_BINARY));
        out.extend(self.existing_agent_symlinks());
        out.extend(self.autostart.iter().cloned());
        out
    }

    fn is_launch_artifact(&self, path: &Path) -> bool {
        if path.parent() != Some(self.bin_dir.as_path()) {
            return false;
        }
        path.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n == RUNTIME_BINARY || n.starts_with(AGENT_SYMLINK_PREFIX))
    }

    /// Symlinks present now. One created later is not listed, which is
    /// acceptable: a new symlink only matters if something starts it, and
    /// that needs a profile (denied) or an autostart entry (denied) or a human.
    fn existing_agent_symlinks(&self) -> Vec<PathBuf> {
        let Ok(entries) = std::fs::read_dir(&self.bin_dir) else {
            return Vec::new();
        };
        entries
            .filter_map(Result::ok)
            .filter(|e| {
                e.file_name()
                    .to_str()
                    .is_some_and(|n| n.starts_with(AGENT_SYMLINK_PREFIX))
            })
            .map(|e| e.path())
            .collect()
    }
}

#[cfg(target_os = "macos")]
fn autostart_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join("Library/LaunchAgents"),
        home.join("Library/LaunchDaemons"),
    ]
}

#[cfg(target_os = "linux")]
fn autostart_dirs(home: &Path) -> Vec<PathBuf> {
    vec![
        home.join(".config/systemd/user"),
        home.join(".config/autostart"),
    ]
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn autostart_dirs(_home: &Path) -> Vec<PathBuf> {
    Vec::new()
}

/// A grant root so broad that granting it is equivalent to no sandbox.
///
/// Unifies the judgement previously duplicated in `access.rs::is_overbroad_root`
/// (cwd consent) and `policy.rs::is_guarded_prefix` (spawn prefixes). Those two
/// disagreed: one knew about `/usr` and `/opt`, the other about depth. This is
/// the union.
pub fn is_overbroad_grant_root(path: &Path, home: &Path) -> bool {
    if path == Path::new("/")
        || path == home
        || path == Path::new("/usr")
        || path == Path::new("/opt")
        || path == Path::new("/opt/homebrew")
    {
        return true;
    }
    if let Ok(rest) = path.strip_prefix("/Volumes") {
        return rest.components().count() <= 1;
    }
    path.components()
        .filter(|c| matches!(c, std::path::Component::Normal(_)))
        .count()
        < 2
}
```

Register it:

```rust
// mur-agent-runtime/src/sandbox/mod.rs — beside `pub mod policy;`
pub mod launch_chain;
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo nextest run -p mur-agent-runtime -E 'test(launch_chain)'`
Expected: PASS, 5 tests.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/launch_chain.rs mur-agent-runtime/src/sandbox/mod.rs
git commit -m "feat(sandbox): derive MUR's protected launch chain

A predicate, not a path list: a list built at seal time cannot cover
<mur_home>/agents/<name> for a name that does not exist yet, and creating
that directory is exactly the escape."
```

---

### Task 2: Tool gate

**Files:**
- Modify: `mur-agent-runtime/src/tools/fs_policy.rs:115` (`check_write_entitlement`)
- Modify: `mur-agent-runtime/src/tools/read_file.rs:37` (`check_entitlement`)
- Modify: `mur-agent-runtime/src/tools/write_file.rs:65`, `mur-agent-runtime/src/tools/edit_file.rs:67` (call sites)
- Modify: `mur-agent-runtime/src/supervisor_runner.rs:275-289` (build the chain once, pass to all three tools)
- Test: `fs_policy.rs` and `read_file.rs` `mod tests`

**Interfaces:**
- Consumes: `LaunchChain::protects_write`, `LaunchChain::protects_read` from Task 1.
- Produces:
  - `check_write_entitlement(fs: &FilesystemEntitlement, canonical: &Path, chain: &LaunchChain)` — **signature change**
  - `ReadFileTool::new(cwd, fs, chain)`, `WriteFileTool::new(cwd, fs, chain)`, `EditFileTool::new(cwd, fs, chain)` — **signature change**, each stores `chain`

- [ ] **Step 1: Write the failing test**

```rust
// mur-agent-runtime/src/tools/fs_policy.rs, in mod tests
#[test]
fn launch_chain_beats_an_explicit_write_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    let chain = crate::sandbox::launch_chain::LaunchChain::for_test(
        &agents.join("mur"),
        &tmp.path().join("bin"),
        &tmp.path().join("home"),
    );

    // The most permissive grant a user could write.
    let fs = FilesystemEntitlement {
        write: vec![tmp.path().to_string_lossy().into_owned()],
        ..Default::default()
    };

    let err = check_write_entitlement(&fs, &agents.join("pm/profile.yaml"), &chain)
        .expect_err("a sibling profile must be refused even under a grant covering it");
    let msg = format!("{err:?}");
    assert!(msg.contains("entitlements"), "error must explain why: {msg}");

    // Negative control: the same grant still works for a path outside the set,
    // so the refusal above is the launch chain and not a broken check.
    check_write_entitlement(&fs, &tmp.path().join("skills/x.yaml"), &chain)
        .expect("unprotected path under the same grant must still be allowed");
}
```

```rust
// mur-agent-runtime/src/tools/read_file.rs, in mod tests
#[tokio::test]
async fn sibling_identity_key_is_refused_even_under_a_read_grant() {
    let tmp = tempfile::tempdir().unwrap();
    let agents = tmp.path().join("agents");
    std::fs::create_dir_all(agents.join("pm")).unwrap();
    std::fs::write(agents.join("pm/identity.key"), b"SECRET").unwrap();
    std::fs::write(agents.join("pm/profile.yaml"), b"name: pm\n").unwrap();

    let chain = crate::sandbox::launch_chain::LaunchChain::for_test(
        &agents.join("mur"),
        &tmp.path().join("bin"),
        &tmp.path().join("home"),
    );
    let fs = fs_ent(&[&tmp.path().to_string_lossy()], &[], &[]);
    let tool = ReadFileTool::new(SessionCwd::new(tmp.path().to_path_buf()), fs, chain);

    let err = tool
        .check_entitlement(&agents.join("pm/identity.key"))
        .expect_err("a sibling signing key must be refused under any read grant");
    assert!(format!("{err:?}").contains("forge"), "error must say why");

    // Negative control: the same grant still reads a neighbouring file, so the
    // refusal is the key rule and not a broken read path.
    tool.check_entitlement(&agents.join("pm/profile.yaml"))
        .expect("sibling profile.yaml is not read-protected");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-agent-runtime -E 'test(launch_chain_beats)'`
Expected: FAIL — `check_write_entitlement` takes 2 arguments, not 3.

- [ ] **Step 3: Write the implementation**

```rust
// mur-agent-runtime/src/tools/fs_policy.rs
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
    let under = |roots: &[String]| {
        roots.iter().any(|r| {
            let root = std::fs::canonicalize(r).unwrap_or_else(|_| PathBuf::from(r));
            canonical.starts_with(&root)
        })
    };
    if under(&fs.deny) {
        return Err(ToolError::Execution(format!(
            "path denied by entitlement: {}",
            canonical.display()
        )));
    }
    if under(&fs.write) {
        return Ok(());
    }
    Err(ToolError::Execution(format!(
        "path not write-entitled: {} (grant it via `mur agent perm allow-write`)",
        canonical.display()
    )))
}
```

`read_file.rs`'s `check_entitlement` is a method on `ReadFileTool`, so store the chain on the struct beside `self.fs` and add at the top of the method:

```rust
if let Some(reason) = self.chain.protects_read(canonical) {
    return Err(ToolError::Execution(format!(
        "path is part of MUR's launch chain and can never be read: {} ({reason})",
        canonical.display()
    )));
}
```

Thread the chain through the three call sites. Build it once beside the existing `self_protected` call:

```rust
// mur-agent-runtime/src/supervisor_runner.rs:275
let tool_fs = crate::tools::fs_policy::self_protected(
    profile.inner.entitlements.filesystem.clone(),
    agent_home,
);
// Issue #712 protects this agent's own profile/key. The launch chain protects
// every OTHER agent's, plus the binary and the autostart entries that start them.
let chain = crate::sandbox::launch_chain::LaunchChain::new(agent_home);
```

then pass `chain.clone()` as the third argument to `ReadFileTool::new`, `WriteFileTool::new` and `EditFileTool::new` (lines 280, 284, 288), and add `chain` to the `check_write_entitlement` calls at `write_file.rs:65` and `edit_file.rs:67`.

- [ ] **Step 4: Run the whole runtime suite**

Run: `cargo nextest run -p mur-agent-runtime`
Expected: PASS. The suite was 837 before Task 1; expect 837 + the new tests, 0 failures.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/tools/
git commit -m "feat(sandbox): enforce the launch chain at the tool gate

Cross-OS, closed under agents created after seal, and the only layer that
can tell the agent which of 'not granted' and 'never grantable' it hit."
```

---

### Task 3: macOS SBPL ordering

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/policy.rs` (add `launch_chain: LaunchChain` to `SandboxPolicy`, set in `from_entitlements`)
- Modify: `mur-agent-runtime/src/sandbox/macos.rs:160-192` (`build_sbpl_profile`)
- Test: `macos.rs` `mod tests`

**Interfaces:**
- Consumes: `LaunchChain::deny_paths`, `LaunchChain::agent_self_home`.
- Produces: `SandboxPolicy.launch_chain` — read by Tasks 3 and 4.

- [ ] **Step 1: Write the failing test**

```rust
// mur-agent-runtime/src/sandbox/macos.rs, in mod tests
#[test]
fn agents_deny_precedes_the_self_reallow_which_precedes_the_self_file_denies() {
    let policy = policy_with_launch_chain("/data/.mur/agents/mur");
    let sbpl = build_sbpl_profile(&policy);

    let agents_deny = sbpl
        .find(r#"(deny file-write* (subpath "/data/.mur/agents"))"#)
        .expect("agents deny missing");
    let self_reallow = sbpl
        .rfind(r#"(allow file-write* (subpath "/data/.mur/agents/mur"))"#)
        .expect("self re-allow missing");
    let self_profile_deny = sbpl
        .find(r#"(deny file-write* (subpath "/data/.mur/agents/mur/profile.yaml"))"#)
        .expect("self profile deny missing");

    // SBPL is last-match-wins, so the ordering IS the mechanism. Asserting the
    // lines merely exist would pass on a profile that grants everything.
    assert!(
        agents_deny < self_reallow,
        "self re-allow must come after the agents deny or the agent cannot write its own home"
    );
    assert!(
        self_reallow < self_profile_deny,
        "self profile deny must come after the re-allow or self-protection is undone"
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-agent-runtime -E 'test(agents_deny_precedes)'`
Expected: FAIL — the agents deny is not emitted.

- [ ] **Step 3: Write the implementation**

In `build_sbpl_profile`, after the existing `fs_deny` loop:

```rust
    // Launch chain, in three tiers. SBPL is last-match-wins, so ordering is
    // the mechanism: deny the whole agents tree, re-allow this agent's own
    // home, then let the pre-existing self-protection denies land last.
    for path in policy.launch_chain.deny_paths() {
        let p = sbpl_escape(&path.to_string_lossy());
        lines.push(format!("(deny file-write* (subpath \"{p}\"))"));
    }
    let own = sbpl_escape(&policy.launch_chain.agent_self_home().to_string_lossy());
    lines.push(format!("(allow file-write* (subpath \"{own}\"))"));
    for f in crate::sandbox::policy::SELF_PROTECTED_AGENT_FILES {
        let p = sbpl_escape(
            &policy
                .launch_chain
                .agent_self_home()
                .join(f)
                .to_string_lossy(),
        );
        lines.push(format!("(deny file-read* (subpath \"{p}\"))"));
        lines.push(format!("(deny file-write* (subpath \"{p}\"))"));
    }
```

- [ ] **Step 4: Run the whole runtime suite**

Run: `cargo nextest run -p mur-agent-runtime`
Expected: PASS. Watch `own_profile_and_identity_key_always_denied` (`policy.rs:805`) and the existing SBPL ordering tests around `macos.rs:361` — they encode the old ordering and must still hold.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/
git commit -m "feat(sandbox): three-tier launch-chain ordering in SBPL

Deny the agents tree, re-allow this agent's own home, then the existing
self-protection denies. Last-match-wins means the order is the mechanism."
```

---

### Task 4: Linux fail-closed

**Files:**
- Modify: `mur-agent-runtime/src/sandbox/policy.rs` (add `pub dropped_grants: Vec<PathBuf>` to `SandboxPolicy`)
- Modify: `mur-agent-runtime/src/sandbox/linux.rs:33-39`
- Test: `linux.rs` `mod tests`

**Interfaces:**
- Consumes: `SandboxPolicy.launch_chain`, `SandboxPolicy.fs_write`.
- Produces: `SandboxPolicy.dropped_grants` — read by Task 6's reporting.

Landlock is a pure allow-list (`path_beneath_rules`); there is no deny rule, so Task 3's carve-out is not expressible here. The only closed option is to drop the offending grant and say so.

- [ ] **Step 1: Write the failing test**

```rust
// mur-agent-runtime/src/sandbox/linux.rs, in mod tests
#[test]
fn a_write_grant_containing_a_protected_path_is_dropped_not_carved() {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents/mur");
    std::fs::create_dir_all(&agent_home).unwrap();
    let skills = mur_home.join("skills");
    std::fs::create_dir_all(&skills).unwrap();

    let (kept, dropped) = partition_write_grants(
        &[mur_home.to_path_buf(), skills.clone()],
        &LaunchChain::for_test(&agent_home, &mur_home.join("bin"), &mur_home.join("home")),
    );

    // `<mur_home>` contains `<mur_home>/agents`, and Landlock cannot carve it out.
    assert_eq!(dropped, vec![mur_home.to_path_buf()]);
    // Negative control: a grant that contains nothing protected survives intact.
    assert_eq!(kept, vec![skills]);
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-agent-runtime -E 'test(a_write_grant_containing)'`
Expected: FAIL — `partition_write_grants` not found.

- [ ] **Step 3: Write the implementation**

```rust
// mur-agent-runtime/src/sandbox/linux.rs
/// Split write grants into those Landlock can safely install and those it
/// cannot. Landlock has no deny rule, so a grant that contains a protected
/// path cannot be carved — it is dropped whole. Fail-closed on purpose: the
/// alternative is installing a rule that hands over the launch chain.
pub(crate) fn partition_write_grants(
    grants: &[PathBuf],
    chain: &LaunchChain,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    grants.iter().cloned().partition(|g| {
        !chain
            .deny_paths()
            .iter()
            .any(|p| p.starts_with(g) || g.starts_with(p))
    })
}
```

and in `apply_linux`:

```rust
    // FS read+write paths (superset of read).
    let (writable, _dropped) = partition_write_grants(&policy.fs_write, &policy.launch_chain);
    if !writable.is_empty() {
        let write_rules = path_beneath_rules(writable.iter(), AccessFs::from_all(abi));
        created = created
            .add_rules(write_rules)
            .context("add fs_write rules")?;
    }
```

Populate `SandboxPolicy.dropped_grants` in `from_entitlements` using the same helper so the field is correct on every platform, and `_dropped` here is only a local recomputation.

- [ ] **Step 4: Run the runtime suite**

Run: `cargo nextest run -p mur-agent-runtime`
Expected: PASS. On macOS the Linux test still compiles and runs — `partition_write_grants` is not `#[cfg]`-gated; only `apply_linux` is.

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/sandbox/
git commit -m "feat(sandbox): fail closed on Linux where Landlock cannot carve

Landlock is a pure allow-list, so the macOS deny/re-allow carve-out has no
equivalent. A grant containing a protected path is dropped whole and recorded."
```

---

### Task 5: Refuse at grant time

**Files:**
- Modify: `mur-core/src/cmd/agent/perm.rs` (extend the `reject_dead_grant` neighbourhood added by #922)
- Modify: `mur-core/src/cmd/agent/cli/access.rs:46` (delegate `is_overbroad_root`)
- Test: `mur-core/tests/agent_perm.rs`

**Interfaces:**
- Consumes: `LaunchChain::protects_write`, `LaunchChain::protects_read`, `is_overbroad_grant_root`.
- Produces: nothing downstream.

`policy.rs::is_guarded_prefix` is deliberately **not** folded in. It governs spawn-prefix resolution, and changing its semantics risks breaking binary resolution for every agent; it is listed as a follow-up, not done here.

- [ ] **Step 1: Write the failing test**

```rust
// mur-core/tests/agent_perm.rs
#[test]
fn allow_write_refuses_the_launch_chain_and_overbroad_roots() {
    let mur_home = TempDir::new().unwrap();
    let bin_dir = TempDir::new().unwrap();
    mur_create(mur_home.path(), bin_dir.path(), "agent_x");

    let sibling = mur_home.path().join("agents/other");
    std::fs::create_dir_all(&sibling).unwrap();
    let sibling_s = sibling.to_string_lossy().into_owned();

    let out = run(
        mur_home.path(),
        &["agent", "perm", "allow-write", "agent_x", &sibling_s],
    );
    assert!(!out.status.success(), "sibling agent dir must be refused");
    let err = String::from_utf8_lossy(&out.stderr).into_owned();
    assert!(err.contains("launch chain"), "error must name the rule: {err}");

    let out = run(mur_home.path(), &["agent", "perm", "allow-write", "agent_x", "/"]);
    assert!(!out.status.success(), "root must be refused");

    // Negative control: an ordinary existing dir under the same mur_home is
    // still grantable, so the refusals above are the new rules and not a
    // blanket failure.
    let ok_dir = mur_home.path().join("artifacts");
    std::fs::create_dir_all(&ok_dir).unwrap();
    let ok_s = ok_dir.to_string_lossy().into_owned();
    let out = run(
        mur_home.path(),
        &["agent", "perm", "allow-write", "agent_x", &ok_s],
    );
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core -E 'test(allow_write_refuses_the_launch_chain)'`
Expected: FAIL — the grant succeeds.

- [ ] **Step 3: Write the implementation**

```rust
// mur-core/src/cmd/agent/perm.rs
/// Refuse a grant that no sandbox would honour, before it reaches the profile.
///
/// Distinct from `reject_dead_grant`, which refuses a path that does not exist
/// yet. This one refuses paths that must never be granted at all.
fn reject_ungrantable(name: &str, path_arg: &str, write: bool) -> Result<()> {
    use mur_agent_runtime::sandbox::launch_chain::{LaunchChain, is_overbroad_grant_root};

    let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(path_arg);
    let agent_home = super::resolve_mur_home()?.join("agents").join(name);
    let chain = LaunchChain::new(&agent_home);

    let hit = if write {
        chain.protects_write(&p)
    } else {
        chain.protects_read(&p)
    };
    if let Some(reason) = hit {
        anyhow::bail!(
            "{} is part of MUR's launch chain and can never be granted: {reason}",
            p.display()
        );
    }

    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("/"));
    if is_overbroad_grant_root(&p, &home) {
        anyhow::bail!(
            "{} is too broad to grant — it covers the whole machine, the whole \
             home directory, or a volume root. Grant the specific project dir instead.",
            p.display()
        );
    }
    Ok(())
}
```

Call it first in both `cmd_perm_allow_read` (`write = false`) and `cmd_perm_allow_write` (`write = true`), before `reject_dead_grant`.

Then collapse the duplicate in `access.rs`:

```rust
// mur-core/src/cmd/agent/cli/access.rs — replace the body of is_overbroad_root
fn is_overbroad_root(p: &Path, home: &Path) -> bool {
    mur_agent_runtime::sandbox::launch_chain::is_overbroad_grant_root(p, home)
}
```

- [ ] **Step 4: Run the perm tests**

Run: `cargo nextest run -p mur-core -E 'binary(agent_perm) or test(overbroad)'`
Expected: PASS. `access.rs`'s existing `overbroad_blocks_shallow_and_home` (line 177) now exercises the shared helper and must still pass.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/perm.rs mur-core/src/cmd/agent/cli/access.rs mur-core/tests/agent_perm.rs
git commit -m "feat(agent): refuse ungrantable paths at the perm boundary

Launch-chain members and overbroad roots never reach the profile. Folds the
duplicated overbroad-root judgement onto one shared helper."
```

---

### Task 6: Report what the protected set neutralised

**Files:**
- Modify: `mur-core/src/cmd/agent/doctor.rs` (extend the `dead_grants` neighbourhood added by #922)
- Test: `doctor.rs` `mod tests`

**Interfaces:**
- Consumes: `LaunchChain::protects_write`, `LaunchChain::protects_read`.
- Produces: nothing downstream.

No task rewrites a profile. An upgrade that silently edits entitlements is the same class of surprise this whole spec exists to remove.

- [ ] **Step 1: Write the failing test**

```rust
// mur-core/src/cmd/agent/doctor.rs, in mod tests
#[test]
fn neutralised_grants_reports_all_three_real_world_escapes() {
    let tmp = tempfile::TempDir::new().unwrap();
    let mur_home = tmp.path();
    let agent_home = mur_home.join("agents/mur");
    let bin = mur_home.join("bin");
    let home = mur_home.join("home");
    let chain = mur_agent_runtime::sandbox::launch_chain::LaunchChain::for_test(
        &agent_home, &bin, &home,
    );

    let fs = mur_common::agent::FilesystemEntitlement {
        write: vec![
            mur_home.join("agents").to_string_lossy().into_owned(),
            bin.join("mur-agent-runtime").to_string_lossy().into_owned(),
            mur_home.join("skills").to_string_lossy().into_owned(),
        ],
        ..Default::default()
    };

    let found = neutralised_grants(&fs, &chain);
    assert_eq!(found.len(), 2, "got {found:?}");
    // Negative control: the legitimate authoring grant is not reported, so the
    // finding count reflects the rules rather than "everything is flagged".
    assert!(!found.iter().any(|(p, _)| p.ends_with("skills")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo nextest run -p mur-core -E 'test(neutralised_grants)'`
Expected: FAIL — `neutralised_grants` not found.

- [ ] **Step 3: Write the implementation**

```rust
// mur-core/src/cmd/agent/doctor.rs
/// Grants the launch chain now makes inert. Reported, never removed: an
/// upgrade that rewrites a user's entitlements is exactly the surprise this
/// work exists to remove.
pub fn neutralised_grants(
    fs: &mur_common::agent::FilesystemEntitlement,
    chain: &mur_agent_runtime::sandbox::launch_chain::LaunchChain,
) -> Vec<(PathBuf, &'static str)> {
    let mut out = Vec::new();
    for raw in &fs.write {
        let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(raw);
        if let Some(reason) = chain.protects_write(&p) {
            out.push((p, reason));
        }
    }
    for raw in &fs.read {
        let p = mur_agent_runtime::sandbox::policy::expand_entitlement_path(raw);
        if let Some(reason) = chain.protects_read(&p) {
            out.push((p, reason));
        }
    }
    out
}
```

Print it in `cmd_doctor`'s text branch, beside the existing dead-grant and concierge-gap output. `cmd_doctor` already resolves `mur_home`; build the chain per agent row:

```rust
        for r in &rows {
            let chain = mur_agent_runtime::sandbox::launch_chain::LaunchChain::new(
                &mur_home.join("agents").join(&r.name),
            );
            let neutralised = super::load_profile_for_edit(&r.name)
                .map(|(_p, prof)| neutralised_grants(&prof.entitlements.filesystem, &chain))
                .unwrap_or_default();
            for (p, reason) in neutralised {
                println!("  {}: grant has NO EFFECT: {}", r.name, p.display());
                println!("    {reason}");
                println!(
                    "    remove with: mur agent perm deny-path {} {}",
                    r.name,
                    p.display()
                );
            }
        }
```

- [ ] **Step 4: Run the doctor tests**

Run: `cargo nextest run -p mur-core -E 'test(neutralised) or test(dead_grants) or test(missing_authoring)'`
Expected: PASS.

- [ ] **Step 5: Full gate and commit**

```bash
cargo fmt
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo fmt --manifest-path mur-agent-gui/src-tauri/Cargo.toml
cargo nextest run -p mur-agent-runtime -p mur-core
cargo clippy -p mur-common -p mur-agent-runtime -p mur-core --all-targets -- -D warnings

git add mur-core/src/cmd/agent/doctor.rs
git commit -m "feat(agent): report grants the launch chain neutralised

Reported, never removed. An upgrade that rewrites a user's entitlements is
the surprise this work exists to remove."
```

---

## Documentation

Fold into Task 6's commit, not a separate task — the surface is user-visible the moment Task 5 lands.

- `README.md` — extend the "A grant that never reached the kernel" bullet added by #922 with one sentence: some paths can never be granted, and `runtime-doctor` names them.
- `mur-server` `docs-content/troubleshooting.md` — the "MUR can't create a skill, workflow or fleet" entry already warns against granting `~/.mur` and `~/.mur/agents`; change "do not" to "cannot", since it is now enforced. **Separate repo, separate PR, do not auto-merge** — merging deploys to the public app.mur.run.

## Out of scope

- **`agent_create`** (Spec 2). Task 5 makes agent-authored agent creation impossible; nothing restores it until Spec 2 ships. Expect a window where an agent genuinely cannot create an agent.
- **Runtime binary attestation.** Verifying `mur-agent-runtime`'s Developer ID before exec catches a swapped binary however it was swapped. Noted in the spec, not scheduled.
- **`is_guarded_prefix` convergence.** Task 5 folds `is_overbroad_root` onto the shared helper but leaves the spawn-prefix path alone; changing it risks binary resolution for every agent.
