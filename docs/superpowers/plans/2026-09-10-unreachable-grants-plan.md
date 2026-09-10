# Implementation plan — unreachable grants

Execute with **`mur-executing-plans`** (in-context, task by task). Do not
delegate: the machine this was written on cannot give an agent access to the
volume under test, so a delegate cannot verify its own work.

Spec: `docs/superpowers/specs/2026-09-10-unreachable-grants-design.md` (5c3ac3b7).
Branch: `docs/unreachable-grants`.

## Goal

Make every permission surface tell the truth when the kernel accepted a
filesystem grant that the agent process still cannot use.

## Architecture

The agent runtime probes each of its own filesystem grants once, immediately
after the sandbox seals, and records the ones that answer EPERM into
`running.lock`. `mur agent perm list-paths`, `mur agent doctor`, and the Hub
all read that one record instead of guessing. The existing guidance string is
reused; the existing check that probes `/Volumes` from the CLI process is
deleted, because it asks the wrong question from the wrong process.

## Tech stack

Rust (edition 2024), `serde`, `tracing`. Crates touched: `mur-common`,
`mur-agent-runtime`, `mur-core`, and `mur-hub-gui` (workspace-EXCLUDED).

## Global Constraints

Every task implicitly includes all of these.

- Only EPERM (`raw_os_error() == Some(1)`) marks a grant unreachable. ENOENT is a dead grant and belongs to `doctor::dead_grants`. Every other errno is left alone.
- Classification must NOT depend on a `/Volumes/` path prefix. `~/Projects` may be a symlink onto an external volume while expanding to `/Users/...`; the existing `fs_policy::is_removable_volume_eperm` requires that prefix and is therefore the wrong helper here. Do not reuse it.
- The probe never blocks startup. An agent with unreachable grants must still come up, or nothing is left running to report that it is crippled.
- The new `SandboxRecord` field carries `#[serde(default)]`. A lock written before this change must still deserialise.
- Directory grants probe with `read_dir`; file grants probe with a metadata read. `read_dir` on a file returns ENOTDIR and would read as healthy.
- No hardcoded values — constants, config, or env vars (CLAUDE.md rule 1).
- Single source file ≤ 800 lines (CLAUDE.md rule 4).
- `mur-hub-gui` is workspace-excluded: `cargo build --workspace` does NOT compile it. Its check runs LAST, as its own task.

## Verification commands

Use these verbatim; the env vars are not optional on this repo.

```bash
# unit tests for a crate (nextest, not `cargo test` — plain cargo test has 7 false reds)
ORT_STRATEGY=download cargo nextest run -p mur-common
ORT_STRATEGY=download cargo nextest run -p mur-agent-runtime
ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" \
  RUST_MIN_STACK=33554432 cargo nextest run -p mur-core

# lint — --all-targets is required; without it cfg variants rot undetected
ORT_STRATEGY=download cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --check
```

Read the exit code. A grep over the output that "looks clean" is not a pass.

## File structure

| File | Change | Responsibility |
|---|---|---|
| `mur-common/src/agent.rs` | modify | `SandboxRecord.unreachable` field |
| `mur-common/src/removable_volume.rs` | modify | guidance string names the binary, not a nonexistent launching app |
| `mur-agent-runtime/src/sandbox/reach.rs` | **create** | the probe: classification + per-path probe + grant walk |
| `mur-agent-runtime/src/sandbox/mod.rs` | modify | `pub mod reach;` |
| `mur-agent-runtime/src/supervisor.rs` | modify | run the probe after seal, fill the lock field, warn once |
| `mur-core/src/cmd/agent/perm_view.rs` | modify | `GrantStatus::Unreachable` + its computation + its rendering |
| `mur-core/src/cmd/agent/doctor.rs` | modify | delete `probe_volumes`; read the lock instead |
| `mur-core/src/cmd/agent/lifecycle.rs` | modify | `create` prints the deferred-verification notice |
| `mur-hub-gui/…` | modify | banner (task 8, scoped there) |

---

## Task 1 — `SandboxRecord` carries unreachable grants

**Interfaces**
- Consumes: nothing.
- Produces: `mur_common::agent::SandboxRecord.unreachable: Vec<DroppedGrant>`. Every later task reads or writes this field.

Steps:

- [ ] Add the failing test to `mur-common/src/agent.rs` (in the existing `mod tests`):

```rust
#[test]
fn a_lock_written_before_this_field_existed_still_deserialises() {
    let legacy = r#"{"enforcing":true,"mode":"macos-sbpl","granted_digest":"abc","dropped":[]}"#;
    let rec: SandboxRecord = serde_json::from_str(legacy).expect("legacy lock must load");
    assert!(rec.unreachable.is_empty());
}
```

- [ ] Run `ORT_STRATEGY=download cargo nextest run -p mur-common` — it must fail to COMPILE (no field `unreachable`). That is the red.
- [ ] Add the field to `pub struct SandboxRecord` (`mur-common/src/agent.rs:1287`), directly after `dropped`:

```rust
    /// Grants the kernel accepted but the process cannot actually reach —
    /// e.g. macOS gating an external volume. Distinct from `dropped`, which
    /// never reached the kernel at all: widening that field to cover this
    /// would make the one unambiguous signal ambiguous.
    #[serde(default)]
    pub unreachable: Vec<DroppedGrant>,
```

- [ ] Fix every struct-literal construction site — the compiler lists them; they are `mur-agent-runtime/src/supervisor.rs:494` and `:513`, `mur-core/src/cmd/agent/perm_view.rs:474` and `:661`, and `mur-common/src/agent.rs:1963`. Add `unreachable: Vec::new(),` to each. Do NOT add `..Default::default()`.
- [ ] Run the same nextest command — green.
- [ ] Commit: `feat(lock): record grants the kernel took but the process cannot reach`

---

## Task 2 — the probe

**Interfaces**
- Consumes: `mur_common::agent::DroppedGrant` (existing).
- Produces:
  - `mur_agent_runtime::sandbox::reach::is_unreachable(err: &std::io::Error) -> bool`
  - `mur_agent_runtime::sandbox::reach::probe_path(path: &Path) -> std::io::Result<()>`
  - `mur_agent_runtime::sandbox::reach::probe_grants<F>(read: &[PathBuf], write: &[PathBuf], probe: F) -> Vec<DroppedGrant>` where `F: Fn(&Path) -> std::io::Result<()>`

Steps:

- [ ] Create `mur-agent-runtime/src/sandbox/reach.rs` with the tests FIRST (paste the whole file; the `mod tests` block at the bottom is the red):

```rust
//! Post-seal reachability probe.
//!
//! A grant can be installed in the kernel and still be unusable: macOS gates
//! access to external volumes per process, so the sandbox accepts the path
//! while the agent gets EPERM on every read. `dropped` cannot express that —
//! nothing was dropped — so this module answers the only question that
//! matters at that point: with the seal in force, can this process actually
//! reach what it was granted?
//!
//! The probe closure is a parameter so the walk is testable without touching
//! a real filesystem, mirroring `doctor::removable_volume_hint`.

use mur_common::agent::DroppedGrant;
use std::path::{Path, PathBuf};

/// EPERM is the only failure that means "granted but unreachable".
///
/// Deliberately NOT keyed on a `/Volumes/` prefix: a granted path may be a
/// symlink onto an external volume while expanding to something under
/// `/Users`, and prefix-matching would silently pass it as healthy.
pub fn is_unreachable(err: &std::io::Error) -> bool {
    err.raw_os_error() == Some(1)
}

/// Probe one granted path. Directories are enumerated (`read_dir`); files are
/// stat'd. `read_dir` on a file answers ENOTDIR, which would read as healthy.
pub fn probe_path(path: &Path) -> std::io::Result<()> {
    let md = std::fs::metadata(path)?;
    if md.is_dir() {
        std::fs::read_dir(path).map(|_| ())
    } else {
        Ok(())
    }
}

/// Walk the sealed read and write grants, returning the unreachable ones.
/// Never fails: this is diagnostic information, and an agent that cannot
/// reach its grants must still start so it can report that.
pub fn probe_grants<F>(read: &[PathBuf], write: &[PathBuf], probe: F) -> Vec<DroppedGrant>
where
    F: Fn(&Path) -> std::io::Result<()>,
{
    let mut out = Vec::new();
    for (verb, list) in [("read", read), ("write", write)] {
        for path in list {
            if let Err(e) = probe(path)
                && is_unreachable(&e)
            {
                out.push(DroppedGrant {
                    path: path.display().to_string(),
                    verb: verb.to_string(),
                    reason: mur_common::REMOVABLE_VOLUME_EPERM_HINT.to_string(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eperm() -> std::io::Error {
        std::io::Error::from_raw_os_error(1)
    }

    #[test]
    fn only_eperm_counts_as_unreachable() {
        assert!(is_unreachable(&eperm()));
        // ENOENT is a dead grant (doctor::dead_grants owns it), not this.
        assert!(!is_unreachable(&std::io::Error::from_raw_os_error(2)));
        // EACCES is an ordinary permission problem, not the volume gate.
        assert!(!is_unreachable(&std::io::Error::from_raw_os_error(13)));
    }

    #[test]
    fn an_unreachable_grant_is_recorded_with_its_verb() {
        let read = vec![PathBuf::from("/x/blocked")];
        let write = vec![];
        let out = probe_grants(&read, &write, |_p| Err(eperm()));
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].verb, "read");
        assert_eq!(out[0].path, "/x/blocked");
        assert_eq!(out[0].reason, mur_common::REMOVABLE_VOLUME_EPERM_HINT);
    }

    #[test]
    fn reachable_grants_produce_nothing() {
        let read = vec![PathBuf::from("/x/fine")];
        let write = vec![PathBuf::from("/y/fine")];
        assert!(probe_grants(&read, &write, |_p| Ok(())).is_empty());
    }

    #[test]
    fn a_non_eperm_failure_is_not_hijacked() {
        let read = vec![PathBuf::from("/x/gone")];
        let out = probe_grants(&read, &[], |_p| Err(std::io::Error::from_raw_os_error(2)));
        assert!(out.is_empty(), "ENOENT must stay with dead_grants: {out:?}");
    }

    #[test]
    fn a_path_outside_volumes_is_still_classified_by_errno() {
        // A symlinked home directory can point at an external volume while
        // expanding to /Users/... — prefix matching would miss it.
        let read = vec![PathBuf::from("/Users/someone/Projects/thing")];
        let out = probe_grants(&read, &[], |_p| Err(eperm()));
        assert_eq!(out.len(), 1, "classification must not depend on /Volumes/");
    }

    #[test]
    fn a_file_grant_is_probed_by_metadata_not_read_dir() {
        // Regression guard for the ENOTDIR trap: probe_path must not call
        // read_dir on a file. Uses this source file, which is always present.
        let this = Path::new(file!());
        if this.exists() {
            probe_path(this).expect("a readable file must probe clean");
        }
    }
}
```

- [ ] Add `pub mod reach;` to `mur-agent-runtime/src/sandbox/mod.rs`, next to `pub mod policy;`.
- [ ] Run `ORT_STRATEGY=download cargo nextest run -p mur-agent-runtime reach` — all six green.
- [ ] Commit: `feat(sandbox): probe whether sealed grants are actually reachable`

---

## Task 3 — run the probe at seal time

**Interfaces**
- Consumes: `reach::probe_grants`, `reach::probe_path` (Task 2); `SandboxRecord.unreachable` (Task 1).
- Produces:
  - `mur_agent_runtime::sandbox::SandboxStatus.fs_read: Vec<PathBuf>` and `.fs_write: Vec<PathBuf>` — the sealed grant lists, needed because `apply()` currently drops the policy.
  - `running.lock` whose `sandbox.unreachable` is populated; a single startup WARN when it is non-empty.

Steps:

- [ ] In `mur-agent-runtime/src/supervisor.rs`, in the `Ok(status)` arm that currently builds `SandboxRecord` at line 494, compute the probe result BEFORE the struct literal:

```rust
            // The seal is in force here, so what the probe observes is what
            // this process can actually do — which is the only authority on
            // the question. Diagnostic only: never fail the start.
            let unreachable = crate::sandbox::reach::probe_grants(
                &policy_fs_read,
                &policy_fs_write,
                crate::sandbox::reach::probe_path,
            );
            if !unreachable.is_empty() {
                tracing::warn!(
                    count = unreachable.len(),
                    paths = ?unreachable.iter().map(|g| &g.path).collect::<Vec<_>>(),
                    hint = mur_common::REMOVABLE_VOLUME_EPERM_HINT,
                    "granted paths are installed but unreachable by this process"
                );
            }
```

- [ ] `policy_fs_read` / `policy_fs_write` do not exist yet at that point. `sandbox::apply` consumes the policy internally, so change `sandbox::apply` (`mur-agent-runtime/src/sandbox/mod.rs:45`) to return the two grant lists alongside the status: add `pub fs_read: Vec<PathBuf>` and `pub fs_write: Vec<PathBuf>` to `SandboxStatus`, filled from `policy.fs_read.clone()` / `policy.fs_write.clone()` immediately before `apply_policy(&policy)` is called. Then read them off `status` in the supervisor.
- [ ] Set `unreachable,` in the `SandboxRecord` literal at line 494. Leave the `Err(e)` arm at line 513 as `unreachable: Vec::new()` — no policy was installed there, so nothing was granted to be unreachable.
- [ ] Run `ORT_STRATEGY=download cargo nextest run -p mur-agent-runtime` — green.
- [ ] Manual check on this machine (the failure this change exists for is live here):

```bash
mur agent restart qa
python3 -c "import json;print(json.load(open('/Users/david/.mur/agents/qa/running.lock'))['sandbox']['unreachable'])"
```

Expect a non-empty list naming the granted project paths. If it is empty, the probe is not running — do not proceed.

- [ ] Commit: `feat(supervisor): record unreachable grants at seal time`

---

## Task 4 — `perm list-paths` stops printing ✓ for what it cannot reach

**Interfaces**
- Consumes: `SandboxRecord.unreachable` (Task 1).
- Produces: `GrantStatus::Unreachable { reason: String }` — Task 8's Hub banner reads the same lock field, not this enum.

Steps:

- [ ] Add the failing test to `mur-core/src/cmd/agent/perm_view.rs` (in `mod tests`, beside `a_dropped_grant_is_marked_with_its_reason`):

```rust
    #[test]
    fn an_unreachable_grant_is_not_reported_as_effective() {
        let p = fs_profile(&["/x/blocked"], &[]);
        let mut rec = sealed(&p, vec![]);
        rec.unreachable.push(DroppedGrant {
            path: "/x/blocked".into(),
            verb: "read".into(),
            reason: mur_common::REMOVABLE_VOLUME_EPERM_HINT.into(),
        });
        let out = paths_picture("a", &p, Some(&lock_with(Some(rec))), None);
        assert!(!out.contains("✓ /x/blocked"), "must not claim effective: {out}");
        assert!(out.contains("✗ /x/blocked"), "must be marked failed: {out}");
        assert!(
            out.contains(mur_common::REMOVABLE_VOLUME_EPERM_HINT),
            "must carry the guidance: {out}"
        );
    }

    #[test]
    fn a_reachable_grant_is_still_effective() {
        let p = fs_profile(&["/x/fine"], &[]);
        let out = paths_picture("a", &p, Some(&lock_with(Some(sealed(&p, vec![])))), None);
        assert!(out.contains("✓ /x/fine"), "negative control: {out}");
    }
```

- [ ] Run `ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" RUST_MIN_STACK=33554432 cargo nextest run -p mur-core perm_view` — red.
- [ ] Add the variant to `pub enum GrantStatus` (`perm_view.rs:33`), after `Dropped`:

```rust
    /// The sandbox installed this grant, but the process cannot reach it.
    Unreachable {
        reason: String,
    },
```

- [ ] In `permissions_view` (`perm_view.rs:152-163`), check `unreachable` before falling through to `Effective`:

```rust
                    Some(sb) => {
                        let dropped = sb
                            .dropped
                            .iter()
                            .find(|d| d.verb == verb && d.path == expanded);
                        let unreachable = sb
                            .unreachable
                            .iter()
                            .find(|d| d.verb == verb && d.path == expanded);
                        match (dropped, unreachable) {
                            (Some(d), _) => GrantStatus::Dropped {
                                reason: d.reason.clone(),
                            },
                            (None, Some(u)) => GrantStatus::Unreachable {
                                reason: u.reason.clone(),
                            },
                            (None, None) => GrantStatus::Effective,
                        }
                    }
```

- [ ] In `paths_picture` (`perm_view.rs:393`), add the arm beside `Dropped`:

```rust
                GrantStatus::Unreachable { reason } => {
                    let _ = writeln!(o, "  ✗ {}\n      installed, but unreachable — {reason}", g.raw);
                }
```

- [ ] Run the same nextest command — green.
- [ ] Commit: `fix(perm): an unreachable grant no longer prints as effective`

---

## Task 5 — `doctor` reads the lock instead of probing the wrong path

**Interfaces**
- Consumes: `SandboxRecord.unreachable` (Task 1).
- Produces: nothing later tasks depend on. Deletes `probe_volumes` and the `removable_volume_hint(probe_volumes)` call.

Steps:

- [ ] Add the failing test to `mur-core/src/cmd/agent/doctor.rs` (in `mod tests`):

```rust
    #[test]
    fn doctor_no_longer_probes_volumes_from_the_cli_process() {
        // The deleted check asked whether the USER's shell could read
        // /Volumes. It always could; the agent is the process that cannot.
        let src = include_str!("doctor.rs");
        assert!(
            !src.contains("fn probe_volumes"),
            "probe_volumes must be gone: it probes the wrong path from the wrong process"
        );
    }
```

- [ ] Run `ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" RUST_MIN_STACK=33554432 cargo nextest run -p mur-core doctor` — red.
- [ ] Delete `fn probe_volumes` (`doctor.rs:133-136`) and the call site block at `doctor.rs:308-311`.
- [ ] Delete `pub fn removable_volume_hint` (`doctor.rs:120-129`) and its three tests (`removable_hint_shown_on_eperm`, `removable_hint_absent_when_readable`, `removable_hint_ignores_non_eperm_errors`). Verified: `doctor.rs` is its only caller in `mur-core`. The runtime's own classifier (`mur-agent-runtime/src/tools/fs_policy.rs:88 is_removable_volume_eperm`) is a separate function and stays untouched. The shared guidance CONSTANT in `mur-common` also stays — it is what this plan's other tasks print.
- [ ] In `cmd_doctor`, after the stale-agent loop, print unreachable grants from each agent's lock:

```rust
    // Read what the agents themselves recorded at seal time: the CLI process
    // has volume access, so it can never observe this by probing.
    for (agent_name, sb) in &unreachable_by_agent {
        eprintln!(
            "\nagent '{agent_name}' — {} granted path(s) installed but unreachable:",
            sb.len()
        );
        for g in sb {
            eprintln!("  ✗ {} ({})", g.path, g.verb);
        }
        eprintln!("{}", mur_common::REMOVABLE_VOLUME_EPERM_HINT);
    }
```

`unreachable_by_agent` is collected inside the EXISTING agent loop in
`cmd_doctor` (`doctor.rs:157-175`), which already binds `agent_name` and a
parsed `lock`. Add one line next to the existing `rows.push(...)`:

```rust
            if let Some(sb) = lock.sandbox.as_ref()
                && !sb.unreachable.is_empty()
            {
                unreachable_by_agent.push((agent_name.clone(), sb.unreachable.clone()));
            }
```

declaring `let mut unreachable_by_agent: Vec<(String, Vec<DroppedGrant>)> = Vec::new();`
beside `let mut rows`. Do not add a second enumeration of the agents directory.

- [ ] Run the same nextest command — green.
- [ ] Commit: `fix(doctor): report unreachable grants from the lock, not a CLI-side probe`

---

## Task 6 — the guidance string names the binary

**Interfaces**
- Consumes: nothing.
- Produces: revised `mur_common::REMOVABLE_VOLUME_EPERM_HINT`. Tasks 2, 4, 5 assert against the constant, not its text, so they keep passing.

Steps:

- [ ] Add the failing test to `mur-common/src/removable_volume.rs`:

```rust
#[cfg(test)]
mod tests {
    #[test]
    fn the_hint_names_the_binary_that_needs_the_grant() {
        // An agent started by launchd has no "app that launched it"; the
        // grant belongs to the runtime binary every per-agent symlink
        // resolves to.
        assert!(super::REMOVABLE_VOLUME_EPERM_HINT.contains("mur-agent-runtime"));
    }
}
```

- [ ] Run `ORT_STRATEGY=download cargo nextest run -p mur-common removable_volume` — red.
- [ ] Replace the constant's value with exactly this (one line, zh-TW, brand uppercase per CLAUDE.md rule 7):

```rust
pub const REMOVABLE_VOLUME_EPERM_HINT: &str = "macOS 擋住了這顆磁碟：請到 系統設定→隱私權與安全性→完全取用磁碟，加入 ~/.local/bin/mur-agent-runtime（所有 agent 都是它的 symlink；由 MUR Hub 啟動的 agent 則加入 MUR Hub），然後重啟該 agent";
```
- [ ] Run the same nextest command — green.
- [ ] Commit: `fix(hint): name mur-agent-runtime, which is what actually needs the grant`

---

## Task 7 — `create` says when volume access gets verified

**Interfaces**
- Consumes: nothing.
- Produces: one line of output. Nothing depends on it.

Steps:

- [ ] Add the failing test to `mur-core/src/cmd/agent/lifecycle.rs` (in `mod tests`; if the module has none, create it):

```rust
    #[test]
    fn create_notice_points_at_first_start() {
        assert!(super::CREATE_VOLUME_NOTICE.contains("mur agent start"));
    }
```

- [ ] Run `ORT_STRATEGY=download MUR_WEB_DIST="$HOME/Projects/mur-web/dist" RUST_MIN_STACK=33554432 cargo nextest run -p mur-core lifecycle` — red.
- [ ] Add the constant next to the other module constants in `lifecycle.rs` and print it at the end of `cmd_create`, after the existing success output. `cmd_create` does NOT start the agent and this task does not change that: making creation start a process is a larger behavioural change than this feature warrants (service installation, unexpected processes).
- [ ] Run the same nextest command — green.
- [ ] Commit: `feat(create): say that volume access is verified on first start`

---

## Task 8 — Hub banner (workspace-excluded; runs LAST)

**Interfaces**
- Consumes: `SandboxRecord.unreachable` (Task 1) via whatever lock-reading path the Hub already uses.
- Produces: nothing.

Steps:

- [ ] First, prove the earlier tasks did not break the Hub. `mur-common` type changes have broken this crate three times because `cargo build --workspace` does not compile it:

```bash
command grep -rn "SandboxRecord {" mur-hub-gui/src mur-gui-core/src
cd mur-hub-gui && ORT_STRATEGY=download cargo check --manifest-path Cargo.toml
```

Any struct literal found needs `unreachable: Vec::new(),`. Fix before continuing.

- [ ] The Hub reads locks through `mur-gui-core/src/discovery.rs` and `mur-gui-core/src/sidecar.rs` — verified, those are the only two. Add the unreachable list to the payload `discovery.rs` already sends the UI; do not add a second file read.
- [ ] Render a banner on the agent's detail view when the list is non-empty: the count, the paths, the guidance string, and a button opening `x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles`.
- [ ] The button navigates to the pane and nothing more — macOS does not allow toggling it programmatically. The copy must not imply otherwise.
- [ ] Build the Hub `.app` and look at it. `cargo tauri dev` will not do: it produces a null bundle id. Follow the recipe in the `gotcha_hub_local_app_build_recipe` memory.
- [ ] Commit: `feat(hub): banner when an agent cannot reach its granted paths`

---

## Done when

- [ ] `ORT_STRATEGY=download cargo clippy --workspace --all-targets -- -D warnings` exits 0
- [ ] `cargo fmt --check` exits 0
- [ ] `mur agent perm list-paths qa` on this machine prints ✗ with the guidance for the project paths, and ✓ for `~/.mur` paths
- [ ] `mur agent doctor` prints the same finding for every affected agent
- [ ] After the user grants access in System Settings and restarts the agent, both surfaces flip back to ✓ — the negative control that proves the probe reads reality and not a constant
