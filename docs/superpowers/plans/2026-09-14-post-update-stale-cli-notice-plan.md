# Post-update stale interactive CLI notice — implementation plan

**Goal:** After a successful Unix `mur update`, tell the updater terminal and any other interactive `mur`/`murmur` processes still executing the replaced binary that they must be reopened before starting jobs.

**Architecture:** Add a process-snapshot seam to `mur-core/src/update/mod.rs`. A pure selector identifies interactive sessions tied to the replaced executable; a thin best-effort `sysinfo` adapter prints the updater warning and sorted peer-session block. Invoke that adapter after the Unix swap/post-upgrade path completes.

**Tech stack:** Rust, `sysinfo` 0.30 (already a `mur-core` dependency), existing `cargo test -p mur-core` test harness.

## Global constraints

- The notice runs only after a successful Unix binary swap.
- Process enumeration is best-effort: any enumeration or executable-path failure is silent and never fails `mur update`.
- The updater process is always warned separately, but must not appear in the peer-session list.
- Peer sessions qualify only when their executable is the replaced executable and their basename is `mur` or `murmur`; agents, daemons, runtimes, MCP servers, and gateways are excluded.
- Do not infer or print a stale process version.
- Preserve deterministic PID ordering in output.

## File structure

| File | Responsibility |
|---|---|
| `mur-core/src/update/mod.rs` | Snapshot type, pure session selector and formatter, `sysinfo` adapter, call after Unix post-upgrade; unit tests. |
| `docs/superpowers/specs/2026-09-14-post-update-stale-cli-notice-design.md` | Approved behavior contract (already committed). |

## Task 1 — Select stale interactive sessions (TDD)

**Interfaces**

- **Consumes:** `std::path::{Path, PathBuf}` and process data supplied by tests.
- **Produces:** `InteractiveProcess { pid: u32, name: String, exe: PathBuf }` plus `stale_interactive_sessions(processes: impl IntoIterator<Item = InteractiveProcess>, updater_pid: u32, replaced_executable: &Path) -> Vec<InteractiveProcess>`.

- [ ] Add a test `stale_sessions_exclude_updater_and_noninteractive_binaries` with snapshots for: updater `mur` PID 7 at `/opt/homebrew/bin/mur`; peer `mur` PID 42 at that path; peer `murmur` PID 11 at that path; `murmurd`, `mur-agent-runtime`, and `mur-mcp-server` at that path; and a `mur` at another path. Assert only PIDs 11 and 42 return, ordered by PID.
- [ ] Run `cargo test -p mur-core stale_sessions_exclude_updater_and_noninteractive_binaries`; observe failure because the type/function do not exist.
- [ ] Add the minimal private snapshot type and pure selector. Compare paths exactly, normalize `.exe` only in the basename predicate, exclude the updater PID, and sort by PID.
- [ ] Re-run the same test; observe pass.
- [ ] Commit with `test(update): cover stale interactive session selection`.

## Task 2 — Print and invoke the best-effort notice (TDD)

**Interfaces**

- **Consumes:** `stale_interactive_sessions`, `sysinfo::System::processes()`, `std::process::id()`, and the successful Unix update target path.
- **Produces:** `warn_stale_interactive_sessions(replaced_executable: &Path)`; no return value and no propagated error.

- [ ] Add a pure formatting test `stale_session_notice_names_terminal_and_sorted_peers`. Supply `mur` PID 42 and `murmur` PID 11 and assert the exact two-part output: mandatory current-terminal warning plus peer heading and PID-ordered bullet lines. Add a second assertion that an empty peer list omits the peer heading.
- [ ] Run `cargo test -p mur-core stale_session_notice_names_terminal_and_sorted_peers`; observe failure because the formatter does not exist.
- [ ] Add a pure `format_stale_interactive_session_notice(&[InteractiveProcess]) -> String`, then a thin `warn_stale_interactive_sessions` adapter that snapshots only processes with an executable path and prints non-empty formatter output. Do not let adapter errors escape.
- [ ] In the Unix update branch, call the adapter after `resign::post_upgrade` succeeds, passing `target`; retain the target before temporary-directory cleanup.
- [ ] Re-run the focused test; observe pass.
- [ ] Run `cargo test -p mur-core update::` and then `cargo test -p mur-core`; observe all pass.
- [ ] Commit with `feat(update): warn about stale interactive sessions`.

## Verification

1. `git diff --check` exits zero.
2. `cargo test -p mur-core stale_sessions_exclude_updater_and_noninteractive_binaries` proves the selector’s inclusion/exclusion boundary.
3. `cargo test -p mur-core stale_session_notice_names_terminal_and_sorted_peers` proves output and deterministic ordering.
4. `cargo test -p mur-core update::` proves the update module suite remains green.
5. `cargo test -p mur-core` proves the crate suite remains green.
