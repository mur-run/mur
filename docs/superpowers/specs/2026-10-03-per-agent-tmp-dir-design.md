# Per-agent scratch directory (`<mur_home>/tmp/<agent>`)

Status: Approved — implementation in progress
Date: 2026-10-03

## Problem

No code path sets `TMPDIR` (`grep TMPDIR` finds nothing in `tools/bash.rs`,
`sandbox/child.rs`, `sandbox/launch_chain.rs`). Agents therefore reach for
`/tmp`, either through child processes (`cargo`, `mktemp`) or by choosing the
path themselves. In run 527479d60217, writing to `/tmp` was rejected, and the
bash tool was withdrawn for the rest of the turn.

The two enforcement layers handle `/tmp` differently:

| Platform | Tool gate (`write_file`/`edit_file`) | Kernel (bash children) |
|---|---|---|
| macOS | denies `/tmp` | **allows** `/private/tmp`, `/private/var/folders` (`macos.rs:63-65`, `MACOS_SYSTEM_WRITE_PATHS`) |
| Linux | denies `/tmp` | denies `/tmp` (`linux.rs:54-59` grants only `policy.fs_write`) |

## Decision

Give every agent one writable scratch directory, `<mur_home>/tmp/<agent>`.
Point both child processes (A: `TMPDIR`) and the model (B: a prompt line) at it.

### Options considered

- **A only (set `TMPDIR`)**: rejected. The model can still pick `/tmp` on its
  own and hit the tool gate.
- **B (TMPDIR + prompt + grant), per-agent dir**: chosen.
- **Shared `<mur_home>/tmp`**: rejected. It creates a cross-agent read/write
  and symlink attack surface, and cleanup cannot tell which agent owns a file.
- **Fleet-shared tmp area**: out of scope. The signed channel and artifacts
  are the sanctioned hand-off routes. Passing data through tmp would bypass
  the HITL gate. If a fleet area is ever needed, it gets its own design as
  `<mur_home>/tmp/fleet/<fleet>`.

### Naming

`agent_name = agent_home.file_name()` and `mur_home = agent_home.parent().parent()`,
which is the same derivation the artifacts grant uses. The on-disk directory
name is already the canonical name produced by `canonicalize_agent_name`, and
it is the exact string the spoof check compares against. No extra
canonicalization is needed here, and `Mur`/`mur` cannot become two directories.

## Implementation scope

### Two independent grant sites (both must change)

Investigation shows that the kernel policy and the tool gate are derived
**separately**, so the change must be made in both places:

1. **Kernel policy**: `sandbox/policy/build.rs`, `from_entitlements`. Add the
   grant immediately after the artifacts block (~`build.rs:253-262`) using the
   same create-before-grant idiom: `create_dir_all`, then push it to
   `fs_write`. Landlock skips rules for paths that do not exist at seal time.
2. **Tool gate**: `tools/fs_policy/mod.rs`, `for_file_tools`
   (~`mod.rs:245-258`). Push the same path into `fs.write`. The existing
   artifacts comment there records what happens without it: "the kernel allows
   the write and `write_file` refuses it, which is how an agent ended up
   probing `/tmp` instead."

Both sites call one shared helper so the derivation cannot drift:

```rust
// mur-agent-runtime/src/agent_paths.rs (new, crate-root, neutral module)
pub fn agent_scratch_dir(agent_home: &Path) -> Result<PathBuf, AgentPathError>;
```

- **Placement**: a new neutral module `agent_paths.rs`, not `sandbox/policy`,
  so `tools/fs_policy` does not gain a dependency on the sandbox builder.
- **Error cases**: `agent_home` has fewer than two ancestors, or no
  `file_name()`. The helper returns `Err` and never guesses a path.
- **Caller behavior on `Err`**: log at `error` level (agent home path and
  reason), skip the grant, and do **not** set `TMPDIR`. Startup continues,
  matching the existing artifacts behavior, but the failure is visible in the
  log instead of silent. Both sites must skip together; a grant at one site
  only is exactly the bug this design fixes.
- **Directory creation**: `create_dir_all`, then set mode `0700` on
  `<mur_home>/tmp/<agent>` (also when it already exists). `<mur_home>/tmp`
  itself is created `0700` if missing. Unix permissions are identical on
  macOS and Linux.

Verification item 1 remains as the regression guard regardless.

Grant scope is the agent's own subdirectory only, never `<mur_home>/tmp`
itself.

- **Linux**: entry in `fs_write`, which maps to `AccessFs::from_all(abi)`
  (`linux.rs:55-59`). This covers create, delete, and rename, as needed for
  atomic writes.
- **macOS**: the same entry becomes `(allow file-write* (subpath …))`.
- **Launch chain**: the directory holds no secrets and no binaries, so
  `partition_write_grants` is expected to keep it. Item 2 asserts that it does
  not appear in `dropped_grants`.

### Environment

The runtime sets `TMPDIR`, `TMP`, and `TEMP` on every agent-sandboxed child
`Command`, rather than relying on the agent's shell. An `unset` by the model
affects only that one command. The agent-side spawn sites are:

- `tools/bash_jobs.rs` (`Command::new("bash")`, the path `cargo`, `mktemp`,
  and every model shell command take; a direct `cargo` run is a bash child).
- `protocol/mcp_client.rs:281` via `sandbox::child::spawn_sandboxed` (MCP
  servers inherit the agent's sandbox, so they need the matching env).

Supervisor-side spawns (`oauth/`, `cli_spawn.rs`, `hooks/`,
`supervisor/identity.rs`) are not under the agent seal and are out of scope.

A user-set `TMPDIR` is **overridden**, with one debug log line. The sandbox
grants are derived statically. Honoring an arbitrary user value would mean
granting dynamically, possibly to `~` or a secrets location. Overriding keeps
three things identical: the `$TMPDIR` the model sees, the location children
write to, and the location the sandbox permits.

### `/private/tmp` in `policy/mod.rs` read list

Unchanged. It stays read-only at the policy level. Making it writable would
reopen the shared area this design rejects.

### Prompt line

Added once to the existing "Output locations" block
(`OUTPUT_LOCATIONS_RULE`, `task_runner/system_prompt.rs:8`), directly after the
artifacts rule. The rule becomes a template filled at prompt build time; when
`agent_scratch_dir` returned `Err`, the line is omitted rather than pointing
at a path that is not granted. `{tmp_dir}` is filled by the runtime with the resolved path;
`~/.mur` is never hardcoded.

```text
- Scratch files (temp output, intermediate data) go in `{tmp_dir}` — this is also `$TMPDIR`. Never use `/tmp`: it is outside your write entitlement and write_file/edit_file will reject it.
```

The wording deliberately does **not** claim "`/tmp` is not writable", because
that is false for bash children on macOS. A rule the model can catch being
wrong is a rule it will stop following. The line separates the two
directories' purposes: artifacts are for output to keep, tmp is for output
that can be discarded.

## Cleanup

| When | Behavior |
|---|---|
| Sandbox seal | `create_dir_all`; **never** emptied, so an agent can resume its scratch files after a restart |
| Runtime start, before seal, by the supervisor | Delete entries whose mtime is older than `tmp_retention` (config value, default 7 days). This is the only automatic cleanup point, and the agent cannot alter it |
| `mur agent delete <name>` | Remove `<mur_home>/tmp/<canonical>` together with artifacts |
| Manual | No new command. `mur agent doctor` reports per-agent tmp size and warns above a configured threshold |
| Size cap | None in v1. Neither seatbelt nor Landlock supports quotas, and a `cargo` target dir would trip any naive cap |

### Cleanup algorithm

1. Iterate the **top-level entries** of `<mur_home>/tmp/<agent>` only. The
   directory itself is always kept.
2. An entry's age is the **newest mtime anywhere in its tree** (walked with
   `symlink_metadata`, never following links). A directory mtime alone is not
   used: writes deep inside an active `cargo` target dir do not update the
   top-level directory's mtime, so it would look stale while in use.
3. If that newest mtime is older than `tmp_retention`, remove the entry
   recursively (`remove_dir_all` for a real directory, `remove_file` for a
   file or symlink). Otherwise keep the whole entry untouched; no partial
   pruning inside a live tree.
4. Errors on one entry are logged at `warn` and do not stop the pass.

Cleanup never follows symlinks. It unlinks the link itself, so an agent
cannot plant a link that makes the supervisor delete files elsewhere.

"Empty on every start" was rejected because it can destroy long-running work
and offers no v1 benefit.

## Known limitation (macOS)

On macOS, the kernel layer lets **every** agent write `/private/tmp` and
`/private/var/folders` (`MACOS_SYSTEM_WRITE_PATHS`, re-allowed at
`macos.rs:170-174`). `TMPDIR` controls only the *default* destination. It is
not kernel-level isolation: a compromised agent's bash can still write
`/tmp`. The tool gate still blocks `write_file`/`edit_file` there.

Tightening this baseline is deferred to a separate change. dyld, unix sockets
(`macos.rs:147-148`), and system tools may depend on it, so it needs its own
dependency investigation. This design is purely additive. That follow-up is
subtractive and carries a different risk.

The limitation is documented in `docs/architecture/runtime-overview.md`, in
the sandbox paragraph (currently around line 363, next to the seatbelt
inheritance note), as part of this change. The README is not changed: it
makes no isolation claim about `/tmp`.

## Verification

| # | Claim | Level | Platform | Method | Expected |
|---|---|---|---|---|---|
| 1 | Both grants present | unit | both | Build policy and file-tool entitlement; `policy.fs_write` and `fs.write` both contain `<home>/tmp/<agent>`, neither contains `<home>/tmp` | pass |
| 2 | Not dropped by launch chain | unit | both | `dropped_grants` excludes the path | pass |
| 3 | Tool gate | unit | both | `check_write_entitlement` on `tmp/<agent>/x` and `/tmp/x` | Ok / `path not write-entitled` |
| 4 | Env override | unit | both | Inspect bash `Command` env, including with a pre-set user `TMPDIR` | `TMPDIR`/`TMP`/`TEMP` all = tmp dir; same on the MCP `spawn_sandboxed` path |
| 5 | Writes land | integration | both | In a sealed child: `mktemp`, `touch "$TMPDIR/x"` | exit 0, file under own dir |
| 6 | Cross-agent denied | integration | both | Agent A's sandbox writes `<home>/tmp/B/x` | EPERM (Linux) / seatbelt deny (macOS) |
| 7 | Kernel blocks `/tmp` | integration | **Linux only** | Sealed child writes `/tmp/x` | EPERM |
| 8 | Cleanup | unit | both | Old file, fresh file, old symlink to an external file | old deleted, fresh kept, link removed, target intact |
| 8b | Cleanup, live tree | unit | both | Old top-level dir containing one fresh file deep inside; plus an all-old nested dir | live dir kept intact, all-old dir removed, `<agent>` dir itself kept |
| 10 | Helper error path | unit | both | `agent_home` without enough ancestors | `Err`; neither grant contains a tmp path; no `TMPDIR` set; prompt line omitted; `error` log emitted |
| 11 | Directory mode | unit | both | Create fresh, and pre-create at `0755` then start | both end at `0700` |
| 9 | Original symptom gone | manual E2E | macOS | Re-run a 527479d60217-style task ("save a scratch file") | ledger path under `tmp/<agent>`, no denial, bash not withdrawn |

Item 7 is Linux-only because the macOS kernel allows `/tmp` by design (see
Known limitation). Item 9 is the only check that proves the model's behavior
changed.

## Out of scope

- Tightening macOS `MACOS_SYSTEM_WRITE_PATHS` (separate change).
- Fleet-shared tmp area.
- Hard size quotas.
