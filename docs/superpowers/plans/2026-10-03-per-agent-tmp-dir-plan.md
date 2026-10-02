# Per-agent scratch directory — implementation plan

> Execute with **mur-executing-plans** (sequential, in-context). Track progress
> with the checkboxes in this file, not in memory. Spec:
> `docs/superpowers/specs/2026-10-03-per-agent-tmp-dir-design.md`.

**Goal:** Give every agent a write-granted, self-cleaning scratch directory
`<mur_home>/tmp/<agent>`, exported as `TMPDIR`, so scratch writes stop being
denied and stop withdrawing `bash`.

**Architecture:** One neutral helper (`agent_paths::agent_scratch_dir`) derives
the path; the kernel policy and the file-tool gate both call it, so the two
grants cannot drift. The bash and MCP spawn sites export it as
`TMPDIR`/`TMP`/`TEMP`; the system prompt names it; the supervisor prunes it
before seal.

**Tech stack:** Rust 2024, `mur-agent-runtime`, `mur-common` (config),
`mur-core` (uninstall, doctor). Landlock (Linux) / seatbelt (macOS).

## Global constraints (every task)

- Both grant sites change together or skip together. A grant at one site only is the bug being fixed.
- Grant only `<mur_home>/tmp/<agent>`, never `<mur_home>/tmp`.
- No hardcoded `~/.mur`; every path derives from `agent_home` / mur home.
- `agent_scratch_dir` returning `Err` ⇒ `error` log, no grants, no `TMPDIR`, no prompt line, startup continues.
- Directories `<mur_home>/tmp` and `<mur_home>/tmp/<agent>` are mode `0700`, also when pre-existing.
- Cleanup never follows symlinks.
- `/private/tmp` macOS baseline in `sandbox/macos.rs` is NOT touched.
- Every source file stays ≤ 800 lines (Mandatory Rule 5). `tools/bash.rs` is at 745: add at most ~15 lines there; anything larger goes into `agent_paths.rs`.
- Lint gate per task: `cargo clippy --all --all-targets --no-deps --locked -- -D warnings` and `cargo fmt --all -- --check`.

## File structure

| File | Change | Responsibility |
|---|---|---|
| `mur-agent-runtime/src/agent_paths.rs` | new | `agent_scratch_dir`, `AgentPathError`, `ensure_scratch_dir` (create + 0700), `scratch_env` |
| `mur-agent-runtime/src/agent_paths/cleanup.rs` | new | `prune_scratch(dir, retention) -> PruneReport` |
| `mur-agent-runtime/src/agent_paths/tests.rs` | new | tests #8, #8b, #10, #11 |
| `mur-agent-runtime/src/lib.rs` | modify | `pub mod agent_paths;` |
| `mur-agent-runtime/src/sandbox/policy/build.rs` | modify | kernel grant after artifacts block (~l.262) |
| `mur-agent-runtime/src/tools/fs_policy/mod.rs` | modify | tool-gate grant after artifacts block (~l.258) |
| `mur-agent-runtime/src/sandbox/policy/tests/fs_grants.rs` | modify | tests #1, #2 |
| `mur-agent-runtime/src/tools/fs_policy/tests.rs` | modify | test #3 |
| `mur-agent-runtime/src/tools/bash.rs` | modify | `scratch_dir: Option<PathBuf>` field; append `scratch_env` to `env` before `SpawnSpec` (~l.250) |
| `mur-agent-runtime/src/tools/registry.rs` | modify | populate `scratch_dir` at the three `BashTool` constructions (l.193, 230, 429) |
| `mur-agent-runtime/src/protocol/mcp_client.rs` | modify | `std_cmd.envs(scratch_env)` before `spawn_sandboxed` (~l.280) |
| `mur-agent-runtime/src/task_runner/system_prompt.rs` | modify | const → `output_locations_rule(Option<&Path>) -> String` |
| `mur-agent-runtime/src/task_runner/builder.rs` | modify | `scratch_dir: Option<PathBuf>` on the builder |
| `mur-agent-runtime/src/supervisor/seal.rs` | modify | `ensure_scratch_dir` + `prune_scratch` before `sandbox::apply` (~l.297) |
| `mur-common/src/config/scratch.rs` | new | `ScratchConfig { retention_days: u32 = 7, warn_size_mb: u64 = 2048 }` |
| `mur-common/src/config/mod.rs` | modify | `#[serde(default)] pub scratch: ScratchConfig` on `Config` |
| `mur-core/src/cmd/agent/install.rs` | modify | `cmd_uninstall` with `delete_data` removes `<mur_home>/tmp/<name>` |
| `mur-core/src/cmd/doctor/*` | modify | per-agent tmp size line + warning above `warn_size_mb` |
| `mur-agent-runtime/tests/scratch_seal.rs` | new | integration #5, #6, #7 |
| `docs/architecture/runtime-overview.md` | modify | macOS known-limitation paragraph (~l.363) |

## Task 1 — Helper and directory creation (#10, #11)

**Interfaces.** Consumes: nothing. Produces:

```rust
// agent_paths.rs
#[derive(Debug, thiserror::Error)]
pub enum AgentPathError {
    #[error("agent home {0} has no <mur_home>/agents ancestor")]
    NoMurHome(PathBuf),
    #[error("agent home {0} has no agent name component")]
    NoAgentName(PathBuf),
}
pub fn agent_scratch_dir(agent_home: &Path) -> Result<PathBuf, AgentPathError>;
/// create_dir_all, then chmod 0700 on `<mur_home>/tmp` and the agent dir.
pub fn ensure_scratch_dir(dir: &Path) -> std::io::Result<()>;
/// `[("TMPDIR", dir), ("TMP", dir), ("TEMP", dir)]`
pub fn scratch_env(dir: &Path) -> [(String, String); 3];
```

- [x] Write failing tests in `agent_paths/tests.rs`: `/m/agents/a` → `/m/tmp/a`; `/` → `Err(NoMurHome)`; `a` → `Err`; `scratch_env` yields all three keys.
- [x] Write failing #11: fresh dir ends `0700`; pre-created at `0755` ends `0700` (both parent `tmp` and agent dir). `#[cfg(unix)]`.
- [x] Run `cargo test -p mur-agent-runtime agent_paths` — watch fail.
- [x] Implement using `agent_home.parent().and_then(Path::parent)` and `file_name()`, same derivation as the artifacts blocks.
- [x] Run tests — pass. Lint gate. Commit `feat(runtime): agent_scratch_dir helper`.

## Task 2 — Both grants in one task (#1, #2, #3, #10 grant half)

**Interfaces.** Consumes `agent_scratch_dir`, `ensure_scratch_dir`. Produces: `policy.fs_write` and `for_file_tools(...).write` contain the scratch path.

- [x] Failing #1 in `fs_grants.rs`: build both from one temp `agent_home`; assert both contain `<home>/tmp/<agent>`, neither contains `<home>/tmp`.
- [x] Failing #2: `policy.dropped` (the `dropped_grants` set) excludes the scratch path.
- [x] Failing #3 in `fs_policy/tests.rs`: `check_write_entitlement` Ok for `tmp/<agent>/x`, `Err("path not write-entitled")` for `/tmp/x`.
- [x] Failing #10 grant half: `agent_home` without ancestors ⇒ neither set contains any `tmp` path.
- [x] Watch all four fail.
- [x] `build.rs`: after artifacts block, `match agent_scratch_dir(agent_home)`; `Ok` ⇒ `ensure_scratch_dir` then push; `Err(e)` ⇒ `tracing::error!(agent_home=%..., %e, "scratch dir not granted")`.
- [x] `fs_policy/mod.rs`: same match, push `to_string_lossy` form (entitlement `write` is `Vec<String>`, `mur-common/src/agent/entitlements.rs:134`; kernel `fs_write` is `Vec<PathBuf>`, `policy/mod.rs:108`, so build.rs pushes the `PathBuf` directly), no log (the kernel site already logged; avoid duplicate errors).
- [x] Acceptance: on helper `Err`, the `error!` fires at least once per agent start. It lives in `from_entitlements`, which seal always runs, so it does not depend on whether or when `registry.rs` calls the helper with `.ok()`. Assert with a `tracing` capture in the #10 grant test.
- [x] Tests pass. Lint gate. Commit `feat(runtime): grant per-agent scratch dir at kernel and tool gate`.

## Task 3 — Env export (#4)

**Interfaces.** Consumes `agent_scratch_dir`, `scratch_env`. Produces `BashTool.scratch_dir: Option<PathBuf>`.

- [x] Failing #4 (bash): with process `TMPDIR=/elsewhere`, a `BashTool` with `scratch_dir=Some(d)` runs `printf %s:%s:%s "$TMPDIR" "$TMP" "$TEMP"`; output is `d:d:d`.
- [x] Failing #4 (`scratch_dir=None`): no `TMPDIR` override is added (output equals the inherited value).
- [x] Failing #4 (MCP): unit-test the env built for the MCP `std_cmd` via a small extracted `fn mcp_child_env(...)` contains the three pairs.
- [x] Watch fail.
- [x] `bash.rs`: after vault env, `if let Some(d) = &self.scratch_dir { env.extend(scratch_env(d)); }` plus one `debug!` when the inherited `TMPDIR` differed. Later `.envs` entries win over inherited env.
- [x] `registry.rs`: set `scratch_dir: agent_scratch_dir(&agent_home).ok()` at all three constructions (tests that build `BashTool` directly get `None`).
- [x] `mcp_client.rs`: same `std_cmd.env` pairs before `spawn_sandboxed`.
- [x] Pass. Lint gate (check `bash.rs` line count ≤ 800). Commit.

## Task 4 — Prompt line (#10 prompt half)

**Interfaces.** Produces `pub(super) fn output_locations_rule` (same visibility as today's `pub(super) const OUTPUT_LOCATIONS_RULE`, `system_prompt.rs:8`; `fs_grants.rs:331` and `build.rs:240` reference it only in doc comments, so no visibility change is needed) `fn output_locations_rule(scratch: Option<&Path>) -> String`; `TaskRunner` gains `scratch_dir: Option<PathBuf>` set by the builder.

- [x] Failing test: `Some(p)` ⇒ output contains the exact line from the spec with `p` substituted, placed after the artifacts bullet, and does not contain "`/tmp` is not writable"; `None` ⇒ output equals today's rule byte-for-byte.
- [x] Update the doc comments at `fs_grants.rs:331` and `build.rs:240` to name `output_locations_rule` (comments only, no code reference).
- [x] Watch fail; implement; `assemble_system_prompt` calls `output_locations_rule(self.scratch_dir.as_deref())`.
- [x] Pass. Lint gate. Commit.

## Task 5 — Cleanup (#8, #8b) and config

**Interfaces.** Produces `ScratchConfig`, and:

```rust
pub struct PruneReport { pub removed: usize, pub kept: usize, pub errors: usize }
pub fn prune_scratch(dir: &Path, retention: Duration, now: SystemTime) -> PruneReport;
```

- [x] Failing #8: old file, fresh file, old symlink to an external file. Age is controlled by the injected `now` (e.g. `now = real_now + 30d`) plus std `File::set_modified` to push the *fresh* file's mtime forward to `now`; no `filetime` dependency. The symlink needs no mtime setting: its own `symlink_metadata` mtime is real-now, so it is old relative to the injected `now`. Expect old removed, fresh kept, link removed, target intact.
- [x] Failing #8b: old top-level dir with one fresh file deep inside ⇒ kept intact; all-old nested dir ⇒ removed; `dir` itself kept.
- [x] Failing: per-entry error increments `errors` and the pass continues.
- [x] Watch fail. Implement: top-level only; newest mtime via recursive walk with `symlink_metadata`; remove whole entry or nothing.
- [x] `ScratchConfig` with serde defaults; test that a config without `scratch:` parses to `7` / `2048`.
- [x] `seal.rs` order is fixed: (1) `ensure_scratch_dir` + `prune_scratch`, (2) `from_entitlements` (its idempotent `ensure_scratch_dir` + grant), (3) `sandbox::apply`. Prune always has a directory to scan and the grant always sees the path.
- [x] `seal.rs`: `ensure_scratch_dir` + `prune_scratch` with `retention_days`, `info!` the report. Skipped on helper `Err`.
- [x] Pass. Lint gate. Commit.

**As built.** `prune_scratch` lives in `agent_paths/prune.rs`. `from_entitlements` is called inside `sandbox::apply`, so `seal.rs` runs `prune_agent_scratch` just before `apply` and the fixed order holds. An entry whose age cannot be read (e.g. mode `000`) counts as an error and is kept, never removed blind. Config lives in `mur-common/src/config/scratch.rs` with named default constants. `tests/supervisor_shutdown.rs::sigterm_removes_running_lock_and_flushes_telemetry` fails identically before T2 (`73fad23c~1`); pre-existing, out of scope.

---

## Task 6 — Uninstall and doctor

- [x] Step 1 (checked 2026-10-03): `cmd_uninstall(name: &str, delete_data: bool)` already exists (`install.rs:195`). T6 changes behavior only: remove `<mur_home>/tmp/<name>` when `delete_data` is true. No signature change.

- [x] Failing test: `cmd_uninstall(name, delete_data=true)` removes `<mur_home>/tmp/<name>`; `delete_data=false` keeps it. Note: the spec says "together with artifacts", but `cmd_uninstall` today does not remove artifacts — scope this task to tmp only and report the artifacts gap rather than widening the change.
- [x] Failing test: `mur agent doctor <name>` prints the tmp size and warns above `warn_size_mb`.
- [x] Implement; pass; lint gate; commit.

**As built.** `cmd_uninstall(.., delete_data=true)` removes `<mur_home>/tmp/<name>` via `agent_paths::agent_scratch_dir` (same derivation as the grant); the `tmp` root is kept. Doctor's size check lives in `cmd/agent/scratch_check.rs` (symlinks not followed, unreadable entries count 0) and prints one line per running agent, warning above `scratch.warn_size_mb`. Like the rest of doctor, only agents with a `running.lock` get a line. Artifacts gap: `cmd_uninstall` still does not remove `~/.mur/artifacts/<name>`; left out of scope as planned.


---

## Task 7 — Integration tests (#5, #6, #7)

File `mur-agent-runtime/tests/scratch_seal.rs`, each test forks a sealed child.

- [x] #5 (both): sealed child runs `mktemp` and `touch "$TMPDIR/x"` ⇒ exit 0, file under own scratch dir.
- [x] #6 (both): agent A's sealed child writes `<home>/tmp/B/x` ⇒ EPERM (Linux) / seatbelt deny (macOS).
- [x] #7 **Linux only** (`#[cfg(target_os = "linux")]`, runs in Linux CI): sealed child writes `/tmp/x` ⇒ EPERM. Do not add a macOS variant; the baseline allows it by design.
- [ ] Run locally on macOS (#5, #6); confirm #7 in Linux CI output. Commit.

**As built.** `tests/scratch_seal.rs`, ctor re-exec like `sandbox_e2e.rs`. Fake MUR home lives under `CARGO_TARGET_TMPDIR` (not system temp: the macOS baseline write-exempts `/private/var/folders` + `/private/tmp`, which would make #6 vacuous). Child drops `tmp/a/.sealed` only after an enforcing seal, so #5 asserts the probe file only when actually sealed; unenforced = skip, or exit 4 under `MUR_TEST_REQUIRE_SANDBOX=1` (set in CI). Not yet observed sealed: from inside MUR, nested `sandbox_init` fails (EPERM) so both cases skip locally — #5 and #6 then passed sealed on the user's unsandboxed macOS terminal with `MUR_TEST_REQUIRE_SANDBOX=1` (after `0de2b35b`); #7 awaits Linux CI.

**Known gap (macOS `mktemp`).** BSD `mktemp` with no template (and `mktemp -t prefix`) ignores `TMPDIR` and creates under `/var/folders/.../T`; observed directly. A tool inside the agent that calls bare `mktemp` therefore writes outside its scratch dir. No failure (the baseline exempts `/private/var/folders`), but scratch isolation does not cover it. GNU `mktemp` on Linux honors `TMPDIR`. The test uses an explicit `"$TMPDIR/probe.XXXXXX"` template. Tracked with the T8 follow-up on tightening the macOS tmp exemptions.

## Task 8 — Docs

- [x] `runtime-overview.md` sandbox section: macOS kernel layer lets every agent write `/private/tmp` and `/private/var/folders`; `TMPDIR` sets the default, not isolation; tightening is a follow-up change. Also note BSD `mktemp` without a template ignores `TMPDIR` (see T7 Known gap).
- [x] Spec status was set to `Approved — implementation in progress` when the plan was approved (2026-10-03). After T9 passes, set it to `Implemented`. Commit.

**As built.** New `### Per-Agent Scratch Dir` section in `runtime-overview.md`, just before Per-Server MCP Egress: grants, env, cleanup knobs, the macOS `/private/tmp` + `/private/var/folders` exemption, and the BSD `mktemp` gap. `mur verify` adds no new stale claims. Spec status stays `in progress` until T9.

## Task 9 — Manual E2E (#9, human step, macOS)

- [x] Restart the `mur` agent, ask it to "save a scratch file".
- [x] Turn ledger shows the path under `tmp/mur`, no `path not write-entitled`, `bash` still available next call.

## Final verification

- [ ] `cargo test -p mur-agent-runtime -p mur-common -p mur-core` — 0 failures.
- [ ] Lint gate, full workspace.
- [ ] Linux CI green including #7.

## Self-review

- Spec coverage: grants → T2; helper/errors → T1, T2, T4; env → T3; prompt → T4; cleanup/config → T5; delete/doctor → T6; integration → T7; macOS limitation → T8; E2E → T9. Items #1–#11 + #8b all mapped.
- Type names checked across tasks: `agent_scratch_dir`, `ensure_scratch_dir`, `scratch_env`, `prune_scratch`, `PruneReport`, `ScratchConfig`, `output_locations_rule`.
- Open point: `cmd_uninstall` does not delete artifacts today (Task 6 note).
