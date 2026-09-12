# Plan: bash timeout is a yield, not a kill

> Execute with **`mur-executing-plans`**. Spec:
> `docs/superpowers/specs/2026-09-12-bash-yield-not-kill-design.md` (all sections; D1–D12).
> Base: `origin/main` at or after #1284. Work in a worktree under `.worktrees/` on branch `feat/bash-yield-not-kill`.

**Goal.** `bash` never kills a command at `timeout_secs`; it returns a job handle and the command keeps running, `bash_wait` collects its output later, `bash_kill` ends its whole process group, and every surface (ledger, murmur) shows *still running* instead of success or failure.

**Architecture.** A per-runtime `JobTable` (`tools/bash_jobs.rs`) owns every spawned shell as a process group, pumps its stdout/stderr through a boundary-safe `StreamMasker` into a disk spool and a bounded tail, and records the exit on a `watch` channel. `bash`, `bash_wait` and `bash_kill` are three thin tools over that table; `TaskRunner` stamps every job with its owner task through a tokio task-local, kills owned jobs on `Deadline`/`Stuck`/`tasks/cancel`, resolves the two control tools' policy through `bash`'s rule, and folds a yielded reply's byte count into the stuck-clock fingerprint. `ToolStatus::Running` is a fourth structural status; the settlement ledger and murmur's step card each grow one state for it.

**Tech stack.** Rust 2024, `cargo nextest`. Runtime tests: `cargo nextest run -p mur-agent-runtime`. `mur-core` env: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432`; from a worktree add `CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target`.

## Global Constraints (from the spec)

- D1: when `timeout_secs` elapses and the child is still running, return `ToolStatus::Running` and **keep the child running**. Nothing is killed by the clock.
- D2: `timeout_secs` keeps its name and its `[0, 600]` clamp; the clamp bounds the wait only. `wait_secs` is an alias on `bash` and `bash_wait`.
- D3: jobs survive the end of the turn. They die with the runtime (`kill_all`), with `tasks/cancel` of the owner task, and with an unattended `Deadline`/`Stuck` stop of the owner task. An attended turn never kills them.
- D4: a `Running` result's `bytes_seen` is folded into the stuck-clock fingerprint; a wait that returned nothing repeats the previous fingerprint.
- D5: a yielded call is `is_error: false` and `ToolStatus::Running { job_id, bytes_seen }`; the text says *still running*, never *timed out*.
- D6: two new tools `bash_wait { job_id, wait_secs }` and `bash_kill { job_id }`; `timeout_secs: 0` on `bash` returns the handle at once.
- D7: combined output spools to `<working_dir>/jobs/<job_id>.log` (the tool's `working_dir` is the agent home in production); each reply carries the bytes since the last reply, capped to the tail.
- D8: owner task id comes from `tokio::task_local!` `CURRENT_TASK_ID`, scoped by the runner around **both** execute sites in `handle_tool_call` through one helper. `bash_wait`/`bash_kill` never check ownership.
- D9: unix children are spawned with `process_group(0)`; kill = `killpg(SIGTERM)` → `KILL_GRACE` (2 s) → `killpg(SIGKILL)`. Windows = `taskkill /F /T /PID` (tree kill; `ponytail:` comment). `kill_on_drop(true)` stays. A `setpgid` failure fails the spawn; never fall back to an ungrouped child.
- D10: masking is streaming and boundary-safe: hold back `longest_secret_bytes - 1` bytes plus any incomplete UTF-8 tail; never emit a prefix of a straddling secret; flush at EOF. Both sinks consume masked bytes only. The whole-result `masked()` chokepoint in `task_runner` stays.
- D11: `bash_wait`/`bash_kill` resolve policy by their own exact name first, then as `bash`, then today's default.
- D12: `MAX_JOBS = 8` is a constant. No profile key.
- Spec §3.6 bounds: `TAIL_BYTES` 16 KiB per stream, `SPOOL_MAX_BYTES` 64 MiB (the spool stops growing past the cap and the reply says so — this plan implements "says so" as stop-writing, not head-truncation, and updates the spec line in Task 8), `JOB_RESULT_TTL` 1 h, `KILL_GRACE` 2 s.
- Spec §4: unknown `job_id` → `ToolError::InvalidInput` naming the known jobs; job cap → `InvalidInput`; spool write failure never kills the child; `bash_kill` on an exited job → `Ok` saying it had already exited.
- Spec §3.7: `turn_ledger::Outcome::Running`, rendered `⏳ <tool> · still running (<job_id>) — bash_wait to continue`, counted separately from ✔/✘; `step/completed` gains `running: bool`; murmur renders it as ⏳, neither Done nor Error.
- Two execute sites in `handle_tool_call` (the Allow path and the post-approval path) — every change to how a tool is executed lands on **both** (memory: a fix on one of them is not a fix).
- `BashTool` is constructed by struct literal in `tools/registry.rs` tests; every new field is added there too.
- No new `mur-common` types. Grep `mur-hub-gui/src-tauri/src` for every renamed or re-signed `pub fn` you touch; run the Hub `cargo check` **last**.
- Before every commit: `cargo fmt --all`, `cargo clippy -p <crate> --all-targets -- -D warnings` (read the exit code), the named tests green.

## File structure

| File | Responsibility | Task |
|---|---|---|
| `mur-agent-runtime/src/tools/mod.rs` | `ToolStatus::Running`; register `bash_jobs`, `bash_control` modules | 1, 3, 4 |
| `mur-agent-runtime/src/turn_ledger.rs` | `Outcome::Running`, `classify`, `running()`, render line | 1 |
| `mur-agent-runtime/src/secrets.rs` | `StreamMasker`, `SecretVault::{longest_value_len, masker, safe_split}` | 2 |
| `mur-agent-runtime/src/tools/bash_jobs.rs` (new) | `JobTable`, `SpawnSpec`, `Poll`, `Exit`, `JobError`, `CURRENT_TASK_ID`, process-group kill, pump, constants | 3 |
| `mur-agent-runtime/src/tools/bash.rs` | `BashTool` over the table; `finish_poll`; `control_tools` | 4 |
| `mur-agent-runtime/src/tools/bash_control.rs` (new) | `BashWaitTool`, `BashKillTool` | 4 |
| `mur-agent-runtime/src/tools/registry.rs` | `attach_bash_control` | 4 |
| `mur-agent-runtime/src/task_runner.rs` | task-local scope at both execute sites, `running` in `step/completed`, `policy_name`, fingerprint fold, `with_bash_jobs`, `kill_jobs_of`, `kill_all_jobs` | 5 |
| `mur-agent-runtime/src/supervisor_runner.rs` | build the table, attach control tools, thread it into `build_runner` | 6 |
| `mur-agent-runtime/src/supervisor.rs` | `kill_all_jobs` after drain | 6 |
| `mur-core/src/a2a_dial.rs` | `StepEvent::Completed.running` | 7 |
| `mur-core/src/cmd/agent/cli/stream.rs`, `mod.rs`, `app.rs`, `step.rs` | `StreamMsg::StepCompleted.running`, `CallOutcome::Running`, `StepState::Yielded`, ⏳ | 7 |
| `docs/superpowers/specs/2026-09-12-bash-yield-not-kill-design.md`, `README.md`, docs site | spool-cap wording, Windows tree kill, tool docs | 8 |

---

## Task 1 — `ToolStatus::Running` and `Outcome::Running`

Types first so every later task compiles against them.

**Interfaces.** Produces: `crate::tools::ToolStatus::Running { job_id: String, bytes_seen: u64 }`; `crate::turn_ledger::Outcome::Running(String)`; `TurnLedger::running(&self) -> Vec<&Action>`.

- [x] In `mur-agent-runtime/src/tools/mod.rs`, extend the enum:

```rust
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
pub enum ToolStatus {
    #[default]
    Ok,
    Failed {
        exit_code: i32,
    },
    Denied {
        detail: String,
    },
    /// The call yielded (spec 2026-09-12 bash-yield D1/D5): the command is
    /// still running under `job_id` and this reply carried its output up to
    /// byte `bytes_seen`. Not an error — the work is in flight, not lost.
    Running {
        job_id: String,
        bytes_seen: u64,
    },
}
```

- [x] In `mur-agent-runtime/src/turn_ledger.rs`, add the variant and the accessor, and change `blocked`, `warrants_settlement`, `classify`, `render`:

```rust
pub enum Outcome {
    Ok,
    /// The tool ran and reported an error.
    Failed(String),
    /// The kernel sandbox refused it. Distinguished from `Failed` because the
    /// remedy is different — a denial is routed or granted, not retried.
    Denied(String),
    /// The call yielded and the command is still running (the detail is the
    /// job id). Neither evidence nor a failure: the outcome does not exist yet.
    Running(String),
}
```

```rust
    /// Failed or refused — what did not happen, and why.
    pub fn blocked(&self) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| !matches!(a.outcome, Outcome::Ok | Outcome::Running(_)))
            .collect()
    }

    /// Yielded and still running — work whose outcome the turn does not know.
    pub fn running(&self) -> Vec<&Action> {
        self.actions
            .iter()
            .filter(|a| matches!(a.outcome, Outcome::Running(_)))
            .collect()
    }

    pub fn warrants_settlement(&self) -> bool {
        !self.changed().is_empty()
            || !self.blocked().is_empty()
            || !self.running().is_empty()
            || !self.stop.is_clean()
    }
```

```rust
pub fn classify(content: &str, is_error: bool, status: &crate::tools::ToolStatus) -> Outcome {
    if let crate::tools::ToolStatus::Denied { detail } = status {
        return Outcome::Denied(truncate(detail, RUNAWAY_BACKSTOP));
    }
    // Before the `is_error` check: a yield is never an error, and it must not
    // be mistaken for `Ok` — "the tests are still running" is not "the tests
    // passed" (spec §3.7).
    if let crate::tools::ToolStatus::Running { job_id, .. } = status {
        return Outcome::Running(job_id.clone());
    }
    if is_error {
        return Outcome::Failed(truncate(content.trim(), RUNAWAY_BACKSTOP));
    }
    Outcome::Ok
}
```

In `render`, insert after the `verified` block and before `let changed = ...`:

```rust
    // Yielded calls get their own glyph: not ✔ (nothing is proven yet), not ✘
    // (nothing failed). The remedy is on the line because it is always the same.
    for a in ledger.running() {
        let tool = short_tool(&a.tool);
        let job = match &a.outcome {
            Outcome::Running(j) => j.as_str(),
            _ => "",
        };
        out.push_str(&format!(
            "  ⏳ {tool} · still running ({job}) — bash_wait to continue\n"
        ));
    }
```

and make the `blocked` loop's match exhaustive:

```rust
            let why = match &a.outcome {
                Outcome::Denied(d) => format!("sandbox: {d}"),
                Outcome::Failed(f) => clean_reason(f),
                Outcome::Ok | Outcome::Running(_) => String::new(),
            };
```

- [x] Add tests at the end of `turn_ledger::tests`:

```rust
    /// Spec §3.7: a yield is its own outcome — before `is_error`, and never
    /// folded into `Ok`.
    #[test]
    fn classify_running_is_neither_ok_nor_failed() {
        let running = classify(
            "cargo test …\n[still running after 30s — job_id: j-1]",
            false,
            &crate::tools::ToolStatus::Running {
                job_id: "j-1".into(),
                bytes_seen: 512,
            },
        );
        assert_eq!(running, Outcome::Running("j-1".into()));
        let mut l = TurnLedger::default();
        l.record(act("bash", "cargo test", running));
        assert!(l.verified().is_empty(), "a yield is not evidence");
        assert!(l.blocked().is_empty(), "a yield is not a failure");
        assert_eq!(l.running().len(), 1);
        assert!(l.warrants_settlement());
        let card = render(&l);
        assert!(card.contains("⏳ bash · still running (j-1)"), "{card}");
        assert!(!card.contains("✘"), "{card}");
    }
```

- [x] `cargo check -p mur-agent-runtime 2>&1 | tail -20` — expect exhaustiveness errors only where this plan already lists a match (none outside `turn_ledger.rs`; if the compiler names another, add an `Outcome::Running(_)`/`ToolStatus::Running { .. }` arm that behaves like `Ok` and note it in the commit body).
- [x] `cargo nextest run -p mur-agent-runtime turn_ledger` → all green, including `classify_running_is_neither_ok_nor_failed`.
- [x] Commit: `feat(runtime): ToolStatus::Running + ledger Outcome::Running (bash-yield T1)`.

---

## Task 2 — `StreamMasker` (D10)

**Interfaces.** Produces: `SecretVault::longest_value_len(&self) -> usize`; `SecretVault::masker(self: &Arc<Self>) -> StreamMasker`; `SecretVault::safe_split(&self, buf: &[u8], split: usize) -> usize`; `StreamMasker::{push(&mut self, &[u8]) -> Vec<u8>, finish(&mut self) -> Vec<u8>}`.

- [x] Write the failing tests first, appended to `secrets::tests`:

```rust
    fn masked_through(v: &Arc<SecretVault>, a: &[u8], b: &[u8]) -> Vec<u8> {
        let mut m = v.masker();
        let mut out = m.push(a);
        out.extend(m.push(b));
        out.extend(m.finish());
        out
    }

    /// D10, test 9: a secret split at EVERY byte boundary between two pipe
    /// reads is still replaced exactly once, and the plaintext never appears.
    #[test]
    fn stream_masker_catches_a_secret_across_every_split_point() {
        let v = Arc::new(SecretVault::new());
        v.set("PW", "hunter2hunter2").unwrap();
        let text = b"pre hunter2hunter2 post";
        for k in 0..=text.len() {
            let out = masked_through(&v, &text[..k], &text[k..]);
            let s = String::from_utf8(out).unwrap();
            assert!(!s.contains("hunter2hunter2"), "split at {k}: {s}");
            assert_eq!(s.matches("[SECRET:PW]").count(), 1, "split at {k}: {s}");
            assert_eq!(s, "pre [SECRET:PW] post", "split at {k}");
        }
    }

    /// A multi-byte character right at the split must not be cut in half.
    #[test]
    fn stream_masker_keeps_utf8_intact_around_the_split() {
        let v = Arc::new(SecretVault::new());
        v.set("PW", "hunter2hunter2").unwrap();
        let text = "préfix ü hunter2hunter2 ü".as_bytes();
        for k in 0..=text.len() {
            let out = masked_through(&v, &text[..k], &text[k..]);
            let s = String::from_utf8(out).unwrap_or_else(|e| panic!("split at {k}: {e}"));
            assert_eq!(s, "préfix ü [SECRET:PW] ü", "split at {k}");
        }
    }

    /// Two secrets where one is a prefix of the other: the longer one wins,
    /// whatever the split.
    #[test]
    fn stream_masker_prefers_the_longer_secret_across_splits() {
        let v = Arc::new(SecretVault::new());
        v.set("SHORT", "hunter2hunter2").unwrap();
        v.set("LONG", "hunter2hunter2extra").unwrap();
        let text = b"x hunter2hunter2extra y";
        for k in 0..=text.len() {
            let out = masked_through(&v, &text[..k], &text[k..]);
            let s = String::from_utf8(out).unwrap();
            assert_eq!(s, "x [SECRET:LONG] y", "split at {k}: {s}");
        }
    }

    /// An empty vault is a passthrough with no hold-back.
    #[test]
    fn stream_masker_without_secrets_passes_bytes_straight_through() {
        let v = Arc::new(SecretVault::new());
        let mut m = v.masker();
        assert_eq!(m.push(b"abc"), b"abc".to_vec());
        assert_eq!(m.finish(), Vec::<u8>::new());
    }
```

- [x] `cargo nextest run -p mur-agent-runtime secrets::tests::stream_masker` → compile error (no `masker`). That is the failing state.
- [x] Implement, in `secrets.rs` after `impl SecretVault { ... }` (add `use std::sync::Arc;` at the top):

```rust
impl SecretVault {
    /// Longest stored value in bytes; 0 when the vault is empty. The streaming
    /// masker holds back one less than this between reads.
    pub fn longest_value_len(&self) -> usize {
        self.lock()
            .values()
            .map(|v| v.expose_secret().len())
            .max()
            .unwrap_or(0)
    }

    /// A masker for a byte stream whose chunk boundaries are arbitrary.
    pub fn masker(self: &Arc<Self>) -> StreamMasker {
        StreamMasker {
            vault: Arc::clone(self),
            carry: Vec::new(),
        }
    }

    /// Move `split` left until no stored value straddles it. Emitting
    /// `buf[..split]` is then safe: every occurrence in it is complete, so
    /// `mask` sees it whole. Returns an occurrence START when it moves, which
    /// is a char boundary because every value is a `str`.
    fn safe_split(&self, buf: &[u8], mut split: usize) -> usize {
        let guard = self.lock();
        loop {
            let mut moved = false;
            for v in guard.values() {
                let needle = v.expose_secret().as_bytes();
                if needle.is_empty() || needle.len() > buf.len() {
                    continue;
                }
                for s in 0..=buf.len() - needle.len() {
                    let e = s + needle.len();
                    if s < split && split < e && &buf[s..e] == needle {
                        split = s;
                        moved = true;
                        break;
                    }
                }
            }
            if !moved {
                return split;
            }
        }
    }
}

/// Masks a stream read in arbitrary chunks (D10). `push` returns the bytes
/// that are safe to emit; `finish` flushes what was held back. Between calls
/// it retains `longest_value_len() - 1` bytes (enough that a value cannot end
/// in an already-emitted chunk without having been seen whole) and never
/// splits inside a UTF-8 sequence or inside an occurrence of a value.
pub struct StreamMasker {
    vault: Arc<SecretVault>,
    carry: Vec<u8>,
}

impl StreamMasker {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        self.carry.extend_from_slice(chunk);
        // Re-read every push: `secret/set` can add a longer value mid-job.
        let hold = self.vault.longest_value_len().saturating_sub(1);
        if self.carry.len() <= hold {
            return Vec::new();
        }
        let mut split = self.carry.len() - hold;
        // Back off a continuation byte so a multi-byte char stays whole.
        // Bounded at 3: past that the bytes are not UTF-8 and lossy is fine.
        let floor = split.saturating_sub(3);
        while split > floor && split < self.carry.len() && (self.carry[split] & 0xC0) == 0x80 {
            split -= 1;
        }
        let split = self.vault.safe_split(&self.carry, split);
        if split == 0 {
            return Vec::new();
        }
        let ready: Vec<u8> = self.carry.drain(..split).collect();
        mask_bytes(&self.vault, &ready)
    }

    pub fn finish(&mut self) -> Vec<u8> {
        let rest = std::mem::take(&mut self.carry);
        mask_bytes(&self.vault, &rest)
    }
}

fn mask_bytes(vault: &SecretVault, bytes: &[u8]) -> Vec<u8> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let text = String::from_utf8_lossy(bytes);
    vault.mask(&text).into_owned().into_bytes()
}
```

- [x] `cargo nextest run -p mur-agent-runtime secrets` → all green (the four new tests plus the existing nine).
- [x] `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo exit=$?` → `exit=0`.
- [x] Commit: `feat(runtime): boundary-safe StreamMasker over the secret vault (bash-yield T2)`.

---

## Task 3 — `JobTable` (D1, D3, D7, D8, D9, D12)

**Interfaces.** Consumes: `StreamMasker` (T2). Produces, all in `crate::tools::bash_jobs`:

```rust
pub const MAX_JOBS: usize = 8;
pub const TAIL_BYTES: usize = 16 * 1024;
pub const SPOOL_MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const JOB_RESULT_TTL: Duration = Duration::from_secs(60 * 60);
pub const KILL_GRACE: Duration = Duration::from_secs(2);
tokio::task_local! { pub static CURRENT_TASK_ID: String; }
pub fn current_task_id() -> Option<String>;
pub struct Exit { pub code: Option<i32>, pub killed_by: Option<&'static str>, pub ended_at: Instant }
pub struct Poll { pub job_id, pub command, pub new_stdout: String, pub new_stderr: String, pub skipped: u64, pub bytes_seen: u64, pub elapsed: Duration, pub spool: Option<PathBuf>, pub spool_note: Option<String>, pub exit: Option<Exit> }
pub struct SpawnSpec<'a> { pub command: &'a str, pub cwd: &'a Path, pub env: Vec<(String, String)>, pub spool_dir: &'a Path, pub vault: Option<Arc<SecretVault>> }
pub enum JobError { TooMany(String), Spawn(std::io::Error), Unknown(String, String) }
impl JobTable {
    pub fn new() -> Arc<Self>;
    pub fn spawn(&self, spec: SpawnSpec<'_>) -> Result<String, JobError>;
    pub async fn poll(&self, job_id: &str, wait: Duration) -> Result<Poll, JobError>;   // removes the job once it has exited
    pub async fn kill(&self, job_id: &str) -> Result<Poll, JobError>;                   // ditto
    pub async fn kill_owned_by(&self, task_id: &str) -> usize;
    pub async fn kill_all(&self) -> usize;
    pub fn running_ids(&self) -> Vec<String>;
    pub fn pid(&self, job_id: &str) -> Option<u32>;
}
#[cfg(unix)] pub fn pid_alive(pid: u32) -> bool;
```

- [x] Create `mur-agent-runtime/src/tools/bash_jobs.rs`:

```rust
//! Background bash jobs: the table behind `bash`'s yield.
//!
//! Spec `2026-09-12-bash-yield-not-kill-design.md`. A timeout is a yield (D1):
//! the child keeps running here, owned by a pump task, as a process group
//! (D9), stamped with the task that started it (D8), its output masked as it
//! streams (D10) into a spool file and a bounded tail (D7).

use std::collections::HashMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::{Child, Command};
use tokio::sync::watch;

use crate::secrets::{SecretVault, StreamMasker};

/// D12: a constant, not a profile key. A doom-looping model must not
/// fork-bomb the host.
pub const MAX_JOBS: usize = 8;
/// Per-stream in-memory retention; the reply never carries more than this.
pub const TAIL_BYTES: usize = 16 * 1024;
/// The spool stops growing here; the tail stays live.
pub const SPOOL_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// An exited job nobody collected is dropped after this.
pub const JOB_RESULT_TTL: Duration = Duration::from_secs(60 * 60);
/// SIGTERM → this → SIGKILL, on the whole group.
pub const KILL_GRACE: Duration = Duration::from_secs(2);
/// After the shell exits, how long the pump waits for the pipes to drain
/// before recording the exit. A grandchild holding the pipe (`sleep 60 &`)
/// must not make the job look alive.
const DRAIN_GRACE: Duration = Duration::from_millis(250);
const READ_BUF: usize = 8 * 1024;

tokio::task_local! {
    /// The task whose turn is executing the current tool call (D8). Scoped by
    /// `TaskRunner` around both execute sites; read here at spawn.
    pub static CURRENT_TASK_ID: String;
}

pub fn current_task_id() -> Option<String> {
    CURRENT_TASK_ID.try_with(|s| s.clone()).ok()
}

/// How a job ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Exit {
    /// `None` when a signal ended it.
    pub code: Option<i32>,
    /// `Some("SIGTERM")` / `Some("SIGKILL")` / `Some("taskkill")` when
    /// `kill` ended it.
    pub killed_by: Option<&'static str>,
    pub ended_at: Instant,
}

#[derive(Debug, Default)]
struct Chan {
    /// Masked bytes, head-dropped to the last `TAIL_BYTES`.
    tail: Vec<u8>,
    /// Total masked bytes ever produced on this stream.
    total: u64,
}

#[derive(Debug, Default)]
struct Output {
    stdout: Chan,
    stderr: Chan,
    spool_written: u64,
    spool_capped: bool,
    spool_error: Option<String>,
}

struct Job {
    owner: Option<String>,
    command: String,
    pid: u32,
    started_at: Instant,
    spool: Option<PathBuf>,
    out: Arc<Mutex<Output>>,
    done: watch::Receiver<Option<Exit>>,
    /// Set by `kill` before the signal, so the recorded exit names it.
    killed_by: Arc<Mutex<Option<&'static str>>>,
    cursor_stdout: u64,
    cursor_stderr: u64,
}

/// What one `bash` / `bash_wait` / `bash_kill` reply gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Poll {
    pub job_id: String,
    pub command: String,
    pub new_stdout: String,
    pub new_stderr: String,
    /// Bytes that fell out of the tails before this poll read them.
    pub skipped: u64,
    /// Sum of both streams' totals — the fingerprint input (D4).
    pub bytes_seen: u64,
    pub elapsed: Duration,
    pub spool: Option<PathBuf>,
    pub spool_note: Option<String>,
    pub exit: Option<Exit>,
}

pub struct SpawnSpec<'a> {
    pub command: &'a str,
    pub cwd: &'a Path,
    /// `PATH` plus the vault's env pairs — everything the child gets.
    pub env: Vec<(String, String)>,
    pub spool_dir: &'a Path,
    pub vault: Option<Arc<SecretVault>>,
}

#[derive(Debug, thiserror::Error)]
pub enum JobError {
    #[error("{MAX_JOBS} jobs running — bash_wait or bash_kill one first: {0}")]
    TooMany(String),
    #[error("spawn failed: {0}")]
    Spawn(#[source] std::io::Error),
    #[error("no such job {0}; jobs known: {1}")]
    Unknown(String, String),
}

#[derive(Default)]
pub struct JobTable {
    jobs: Mutex<HashMap<String, Job>>,
}

impl JobTable {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Job>> {
        self.jobs.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn known(jobs: &HashMap<String, Job>) -> String {
        let mut ids: Vec<&str> = jobs.keys().map(String::as_str).collect();
        ids.sort_unstable();
        if ids.is_empty() {
            "none".to_string()
        } else {
            ids.join(", ")
        }
    }

    /// Drop exited jobs nobody collected within `JOB_RESULT_TTL`.
    fn reap_expired(&self) {
        let now = Instant::now();
        self.lock().retain(|_, j| match &*j.done.borrow() {
            Some(e) => now.duration_since(e.ended_at) < JOB_RESULT_TTL,
            None => true,
        });
    }

    pub fn running_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .lock()
            .iter()
            .filter(|(_, j)| j.done.borrow().is_none())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort_unstable();
        ids
    }

    pub fn pid(&self, job_id: &str) -> Option<u32> {
        self.lock().get(job_id).map(|j| j.pid)
    }

    pub fn spawn(&self, spec: SpawnSpec<'_>) -> Result<String, JobError> {
        self.reap_expired();
        let mut jobs = self.lock();
        let running = self.running_ids_locked(&jobs);
        if running.len() >= MAX_JOBS {
            return Err(JobError::TooMany(running.join(", ")));
        }
        let mut cmd = Command::new("bash");
        cmd.arg("-c")
            .arg(spec.command)
            .current_dir(spec.cwd)
            .envs(spec.env)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        // D9: the child leads its own group so `kill` reaches `cargo`, test
        // binaries and pipeline stages, not just the shell. A `setpgid`
        // failure surfaces as a spawn error — never an ungrouped child that
        // `bash_kill` would then lie about.
        #[cfg(unix)]
        cmd.process_group(0);
        let mut child = cmd.spawn().map_err(JobError::Spawn)?;
        let pid = child.id().unwrap_or(0);
        let id = format!("j-{}", uuid::Uuid::now_v7());
        let (spool_path, spool_file, spool_error) = match std::fs::create_dir_all(spec.spool_dir)
            .and_then(|()| {
                let p = spec.spool_dir.join(format!("{id}.log"));
                std::fs::File::create(&p).map(|f| (p, f))
            }) {
            Ok((p, f)) => (Some(p), Some(f), None),
            Err(e) => (None, None, Some(e.to_string())),
        };
        let out = Arc::new(Mutex::new(Output {
            spool_error,
            ..Default::default()
        }));
        let (done_tx, done_rx) = watch::channel(None);
        let killed_by = Arc::new(Mutex::new(None));
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        tokio::spawn(pump(
            child,
            stdout,
            stderr,
            out.clone(),
            spool_file,
            done_tx,
            spec.vault,
            killed_by.clone(),
        ));
        jobs.insert(
            id.clone(),
            Job {
                owner: current_task_id(),
                command: spec.command.to_string(),
                pid,
                started_at: Instant::now(),
                spool: spool_path,
                out,
                done: done_rx,
                killed_by,
                cursor_stdout: 0,
                cursor_stderr: 0,
            },
        );
        Ok(id)
    }

    fn running_ids_locked(&self, jobs: &HashMap<String, Job>) -> Vec<String> {
        let mut ids: Vec<String> = jobs
            .iter()
            .filter(|(_, j)| j.done.borrow().is_none())
            .map(|(id, _)| id.clone())
            .collect();
        ids.sort_unstable();
        ids
    }

    fn done_of(&self, job_id: &str) -> Result<watch::Receiver<Option<Exit>>, JobError> {
        let jobs = self.lock();
        jobs.get(job_id)
            .map(|j| j.done.clone())
            .ok_or_else(|| JobError::Unknown(job_id.to_string(), Self::known(&jobs)))
    }

    /// Wait up to `wait` for the job to exit, then report what is new since
    /// the last poll. An exited job is removed once reported.
    pub async fn poll(&self, job_id: &str, wait: Duration) -> Result<Poll, JobError> {
        let mut done = self.done_of(job_id)?;
        if done.borrow().is_none() && !wait.is_zero() {
            let _ = tokio::time::timeout(wait, done.wait_for(|e| e.is_some())).await;
        }
        self.take_snapshot(job_id)
    }

    /// SIGTERM the group, wait `KILL_GRACE`, SIGKILL it, then report.
    pub async fn kill(&self, job_id: &str) -> Result<Poll, JobError> {
        let (pid, mut done, killed_by) = {
            let jobs = self.lock();
            let j = jobs
                .get(job_id)
                .ok_or_else(|| JobError::Unknown(job_id.to_string(), Self::known(&jobs)))?;
            (j.pid, j.done.clone(), j.killed_by.clone())
        };
        if done.borrow().is_none() {
            *killed_by.lock().unwrap_or_else(|e| e.into_inner()) = Some(FIRST_SIGNAL);
            signal_group(pid, Signal::Term);
            if tokio::time::timeout(KILL_GRACE, done.wait_for(|e| e.is_some()))
                .await
                .is_err()
            {
                *killed_by.lock().unwrap_or_else(|e| e.into_inner()) = Some(SECOND_SIGNAL);
                signal_group(pid, Signal::Kill);
                let _ = tokio::time::timeout(KILL_GRACE, done.wait_for(|e| e.is_some())).await;
            }
        }
        self.take_snapshot(job_id)
    }

    /// D3/D8: end every running job the given task started.
    pub async fn kill_owned_by(&self, task_id: &str) -> usize {
        let ids: Vec<String> = {
            let jobs = self.lock();
            jobs.iter()
                .filter(|(_, j)| j.owner.as_deref() == Some(task_id) && j.done.borrow().is_none())
                .map(|(id, _)| id.clone())
                .collect()
        };
        let mut n = 0;
        for id in ids {
            if self.kill(&id).await.is_ok() {
                n += 1;
            }
        }
        n
    }

    /// Runtime shutdown: every running job, whoever started it.
    pub async fn kill_all(&self) -> usize {
        let ids = self.running_ids();
        let mut n = 0;
        for id in ids {
            if self.kill(&id).await.is_ok() {
                n += 1;
            }
        }
        n
    }

    fn take_snapshot(&self, job_id: &str) -> Result<Poll, JobError> {
        let mut jobs = self.lock();
        // Two steps, not `get_mut(..).ok_or_else(..)`: the closure would
        // borrow `jobs` while `get_mut` holds it mutably.
        if !jobs.contains_key(job_id) {
            return Err(JobError::Unknown(job_id.to_string(), Self::known(&jobs)));
        }
        let j = jobs.get_mut(job_id).expect("checked above");
        let exit = j.done.borrow().clone();
        let (new_stdout, new_stderr, skipped, bytes_seen, spool_note) = {
            let o = j.out.lock().unwrap_or_else(|e| e.into_inner());
            let (so, sk1) = read_since(&o.stdout, &mut j.cursor_stdout);
            let (se, sk2) = read_since(&o.stderr, &mut j.cursor_stderr);
            let note = if let Some(e) = &o.spool_error {
                Some(format!("spool unavailable: {e}"))
            } else if o.spool_capped {
                Some(format!(
                    "spool capped at {} MiB; later output only in this tail",
                    SPOOL_MAX_BYTES / (1024 * 1024)
                ))
            } else {
                None
            };
            (so, se, sk1 + sk2, o.stdout.total + o.stderr.total, note)
        };
        let poll = Poll {
            job_id: job_id.to_string(),
            command: j.command.clone(),
            new_stdout,
            new_stderr,
            skipped,
            bytes_seen,
            elapsed: exit
                .as_ref()
                .map(|e| e.ended_at.duration_since(j.started_at))
                .unwrap_or_else(|| j.started_at.elapsed()),
            spool: j.spool.clone(),
            spool_note,
            exit,
        };
        if poll.exit.is_some() {
            // Reported once; a second wait on it is "no such job" (§4).
            jobs.remove(job_id);
        }
        Ok(poll)
    }
}

/// Bytes of `chan` from `*cursor` to the end, bounded by what the tail still
/// holds. Advances the cursor. Returns `(text, skipped)`.
fn read_since(chan: &Chan, cursor: &mut u64) -> (String, u64) {
    let tail_start = chan.total - chan.tail.len() as u64;
    let from = (*cursor).max(tail_start);
    let skipped = from - *cursor;
    let text = String::from_utf8_lossy(&chan.tail[(from - tail_start) as usize..]).into_owned();
    *cursor = chan.total;
    (text, skipped)
}

#[derive(Clone, Copy)]
enum Signal {
    Term,
    Kill,
}

#[cfg(unix)]
const FIRST_SIGNAL: &str = "SIGTERM";
#[cfg(unix)]
const SECOND_SIGNAL: &str = "SIGKILL";
#[cfg(not(unix))]
const FIRST_SIGNAL: &str = "taskkill";
#[cfg(not(unix))]
const SECOND_SIGNAL: &str = "taskkill";

#[cfg(unix)]
fn signal_group(pid: u32, sig: Signal) {
    let sig = match sig {
        Signal::Term => libc::SIGTERM,
        Signal::Kill => libc::SIGKILL,
    };
    // SAFETY: `pid` came from `Child::id()` of a child we spawned with
    // `process_group(0)`, so it is also the group id; `killpg` on a raw pgid
    // has no memory-safety hazard, and a dead group returns ESRCH.
    unsafe {
        libc::killpg(pid as libc::pid_t, sig);
    }
}

#[cfg(not(unix))]
fn signal_group(pid: u32, _sig: Signal) {
    // ponytail: taskkill /T kills the tree on Windows; Job Objects if a
    // Windows user reports an orphan this misses.
    let _ = std::process::Command::new("taskkill")
        .args(["/F", "/T", "/PID", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
}

#[cfg(unix)]
pub fn pid_alive(pid: u32) -> bool {
    // SAFETY: signal 0 probes existence only.
    unsafe { libc::kill(pid as libc::pid_t, 0) == 0 }
}

#[allow(clippy::too_many_arguments)]
async fn pump(
    mut child: Child,
    stdout: Option<tokio::process::ChildStdout>,
    stderr: Option<tokio::process::ChildStderr>,
    out: Arc<Mutex<Output>>,
    spool: Option<std::fs::File>,
    done: watch::Sender<Option<Exit>>,
    vault: Option<Arc<SecretVault>>,
    killed_by: Arc<Mutex<Option<&'static str>>>,
) {
    let spool = spool.map(|f| Arc::new(Mutex::new(f)));
    let a = tokio::spawn(copy_stream(
        stdout,
        Which::Stdout,
        out.clone(),
        spool.clone(),
        vault.as_ref().map(|v| v.masker()),
    ));
    let b = tokio::spawn(copy_stream(
        stderr,
        Which::Stderr,
        out.clone(),
        spool.clone(),
        vault.as_ref().map(|v| v.masker()),
    ));
    let status = child.wait().await;
    // Bounded drain: a grandchild holding the pipe open must not keep the
    // job "running" after the shell is gone. Dropping the handles detaches
    // the readers; they keep appending to the tail until EOF.
    let _ = tokio::time::timeout(DRAIN_GRACE, async {
        let _ = a.await;
        let _ = b.await;
    })
    .await;
    let code = match status {
        Ok(s) => s.code(),
        Err(e) => {
            out.lock()
                .unwrap_or_else(|e| e.into_inner())
                .stderr
                .append(format!("\n[wait failed: {e}]").as_bytes());
            None
        }
    };
    let exit = Exit {
        code,
        killed_by: *killed_by.lock().unwrap_or_else(|e| e.into_inner()),
        ended_at: Instant::now(),
    };
    let _ = done.send(Some(exit));
}

#[derive(Clone, Copy)]
enum Which {
    Stdout,
    Stderr,
}

impl Chan {
    fn append(&mut self, bytes: &[u8]) {
        self.total += bytes.len() as u64;
        self.tail.extend_from_slice(bytes);
        if self.tail.len() > TAIL_BYTES {
            let drop = self.tail.len() - TAIL_BYTES;
            self.tail.drain(..drop);
        }
    }
}

async fn copy_stream<R: AsyncRead + Unpin>(
    reader: Option<R>,
    which: Which,
    out: Arc<Mutex<Output>>,
    spool: Option<Arc<Mutex<std::fs::File>>>,
    mut masker: Option<StreamMasker>,
) {
    let Some(mut r) = reader else { return };
    let mut buf = vec![0u8; READ_BUF];
    loop {
        match r.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => {
                let bytes = match masker.as_mut() {
                    Some(m) => m.push(&buf[..n]),
                    None => buf[..n].to_vec(),
                };
                append(&out, which, spool.as_deref(), &bytes, SPOOL_MAX_BYTES);
            }
        }
    }
    if let Some(m) = masker.as_mut() {
        let rest = m.finish();
        append(&out, which, spool.as_deref(), &rest, SPOOL_MAX_BYTES);
    }
}

/// One masked chunk into the tail and the spool. `cap` is a parameter so the
/// cap path is testable without writing 64 MiB.
fn append(
    out: &Mutex<Output>,
    which: Which,
    spool: Option<&Mutex<std::fs::File>>,
    bytes: &[u8],
    cap: u64,
) {
    if bytes.is_empty() {
        return;
    }
    let mut o = out.lock().unwrap_or_else(|e| e.into_inner());
    match which {
        Which::Stdout => o.stdout.append(bytes),
        Which::Stderr => o.stderr.append(bytes),
    }
    let Some(f) = spool else { return };
    if o.spool_capped || o.spool_error.is_some() {
        return;
    }
    let room = cap.saturating_sub(o.spool_written) as usize;
    let take = bytes.len().min(room);
    let res = f
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .write_all(&bytes[..take]);
    match res {
        // A logging failure never touches the child (§4).
        Err(e) => o.spool_error = Some(e.to_string()),
        Ok(()) => {
            o.spool_written += take as u64;
            if take < bytes.len() {
                o.spool_capped = true;
            }
        }
    }
}
```

- [x] Register the module in `tools/mod.rs`: add `pub mod bash_jobs;` after `pub mod bash;`.
- [x] `cargo check -p mur-agent-runtime` → clean (if `Command::process_group` is reported missing, the tokio version is older than 1.28 — it is 1.52 in `Cargo.lock`; do not work around it).
- [x] Append the tests (unix-only where a process is the subject):

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn spec<'a>(cmd: &'a str, dir: &'a Path, vault: Option<Arc<SecretVault>>) -> SpawnSpec<'a> {
        SpawnSpec {
            command: cmd,
            cwd: dir,
            env: vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())]
                .into_iter()
                .chain(vault.iter().flat_map(|v| v.env_pairs()))
                .collect(),
            spool_dir: dir,
            vault,
        }
    }

    #[cfg(unix)]
    async fn wait_dead(pid: u32, within: Duration) -> bool {
        let t0 = Instant::now();
        while t0.elapsed() < within {
            if !pid_alive(pid) {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        !pid_alive(pid)
    }

    /// Test 1 — the assertion that separates yield from kill: after the wait
    /// runs out the pid is still alive.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_timed_out_wait_returns_running_and_leaves_the_child_alive() {
        let dir = tempfile::tempdir().unwrap();
        let t = JobTable::new();
        let id = t.spawn(spec("sleep 3", dir.path(), None)).unwrap();
        let t0 = Instant::now();
        let p = t.poll(&id, Duration::from_secs(1)).await.unwrap();
        assert!(p.exit.is_none(), "{p:?}");
        assert!(t0.elapsed() < Duration::from_millis(2500), "waited the whole sleep");
        assert!(pid_alive(t.pid(&id).unwrap()), "the clock killed the child");
        assert_eq!(t.running_ids(), vec![id.clone()]);
        // Test 2 — a later wait collects the exit and the job is gone.
        let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(p.exit.as_ref().and_then(|e| e.code), Some(0), "{p:?}");
        assert!(matches!(t.poll(&id, Duration::ZERO).await, Err(JobError::Unknown(..))));
    }

    /// Test 5 — zero wait is the background case.
    #[cfg(unix)]
    #[tokio::test]
    async fn zero_wait_returns_at_once_before_any_output() {
        let dir = tempfile::tempdir().unwrap();
        let t = JobTable::new();
        let id = t.spawn(spec("sleep 0.5; echo late", dir.path(), None)).unwrap();
        let p = t.poll(&id, Duration::ZERO).await.unwrap();
        assert!(p.exit.is_none());
        assert_eq!(p.new_stdout, "");
        t.kill_all().await;
    }

    /// Test 6 — output after the yield arrives on the next wait, exactly once.
    #[cfg(unix)]
    #[tokio::test]
    async fn output_after_a_yield_is_delivered_once() {
        let dir = tempfile::tempdir().unwrap();
        let t = JobTable::new();
        let id = t
            .spawn(spec("echo a; sleep 1; echo b", dir.path(), None))
            .unwrap();
        let p1 = t.poll(&id, Duration::from_millis(300)).await.unwrap();
        assert_eq!(p1.new_stdout, "a\n", "{p1:?}");
        assert!(p1.exit.is_none());
        let p2 = t.poll(&id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(p2.new_stdout, "b\n", "{p2:?}");
        assert_eq!(p2.exit.as_ref().and_then(|e| e.code), Some(0));
        assert!(p2.bytes_seen > p1.bytes_seen);
    }

    /// Test 3 + 4 — D9: killing the job kills the grandchild too, and the
    /// shell is reaped (no zombie), which is what `kill_on_drop` alone used to
    /// get wrong.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_ends_the_whole_group_and_reaps_the_shell() {
        let dir = tempfile::tempdir().unwrap();
        let t = JobTable::new();
        let id = t
            .spawn(spec("sleep 60 & echo $!; wait", dir.path(), None))
            .unwrap();
        let p = t.poll(&id, Duration::from_millis(500)).await.unwrap();
        let grandchild: u32 = p.new_stdout.trim().parse().expect("grandchild pid on stdout");
        assert!(pid_alive(grandchild));
        let shell = t.pid(&id).unwrap();
        let k = t.kill(&id).await.unwrap();
        assert!(k.exit.as_ref().and_then(|e| e.killed_by).is_some(), "{k:?}");
        assert!(
            wait_dead(grandchild, KILL_GRACE + Duration::from_secs(1)).await,
            "grandchild {grandchild} outlived bash_kill"
        );
        // Reaped by the pump's `wait`, so the kernel has no child entry left.
        let mut status: libc::c_int = 0;
        let ret = unsafe { libc::waitpid(shell as libc::pid_t, &mut status, libc::WNOHANG) };
        assert_eq!(ret, -1, "shell {shell} still a child (zombie or running)");
    }

    /// Test 8 — the cap names the running jobs.
    #[cfg(unix)]
    #[tokio::test]
    async fn the_ninth_job_is_refused_and_names_the_others() {
        let dir = tempfile::tempdir().unwrap();
        let t = JobTable::new();
        for _ in 0..MAX_JOBS {
            t.spawn(spec("sleep 30", dir.path(), None)).unwrap();
        }
        match t.spawn(spec("true", dir.path(), None)) {
            Err(JobError::TooMany(list)) => assert_eq!(list.matches("j-").count(), MAX_JOBS),
            other => panic!("expected TooMany, got {other:?}"),
        }
        assert_eq!(t.kill_all().await, MAX_JOBS);
    }

    /// Test 10 — D10 end to end: the secret reaches the child, comes back in
    /// two pipe writes, and the spool FILE holds the mask, not the value.
    #[cfg(unix)]
    #[tokio::test]
    async fn spool_holds_the_mask_even_when_the_secret_arrives_in_two_reads() {
        let dir = tempfile::tempdir().unwrap();
        let v = Arc::new(SecretVault::new());
        v.set("TOK", "hunter2hunter2").unwrap();
        let t = JobTable::new();
        let id = t
            .spawn(spec(
                "printf '%s' \"${TOK:0:3}\"; sleep 0.3; printf '%s\\n' \"${TOK:3}\"",
                dir.path(),
                Some(v),
            ))
            .unwrap();
        let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(p.new_stdout, "[SECRET:TOK]\n", "{p:?}");
        let spool = std::fs::read_to_string(p.spool.unwrap()).unwrap();
        assert_eq!(spool, "[SECRET:TOK]\n");
    }

    /// Test 11 — D8: ownership is the task-local at spawn; cleanup is per
    /// owner; a job spawned outside any scope is only `kill_all`'s.
    #[cfg(unix)]
    #[tokio::test]
    async fn kill_owned_by_kills_only_that_tasks_jobs() {
        let dir = tempfile::tempdir().unwrap();
        let t = JobTable::new();
        let a = CURRENT_TASK_ID
            .scope("task-A".to_string(), async { t.spawn(spec("sleep 30", dir.path(), None)) })
            .await
            .unwrap();
        let b = CURRENT_TASK_ID
            .scope("task-B".to_string(), async { t.spawn(spec("sleep 30", dir.path(), None)) })
            .await
            .unwrap();
        let orphan = t.spawn(spec("sleep 30", dir.path(), None)).unwrap();
        assert_eq!(t.kill_owned_by("task-nope").await, 0);
        assert_eq!(t.kill_owned_by("task-A").await, 1);
        assert!(wait_dead(t.pid(&a).unwrap_or(0), KILL_GRACE + Duration::from_secs(1)).await);
        assert!(pid_alive(t.pid(&b).unwrap()), "B's job died with A");
        assert!(pid_alive(t.pid(&orphan).unwrap()));
        assert_eq!(t.kill_all().await, 2);
    }

    /// Test 7 (tail half) — more output than the tail keeps is reported as
    /// skipped, and the reply is exactly the tail.
    #[cfg(unix)]
    #[tokio::test]
    async fn tail_window_reports_what_it_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let t = JobTable::new();
        let id = t
            .spawn(spec("head -c 40000 /dev/zero | tr '\\0' x", dir.path(), None))
            .unwrap();
        let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(p.new_stdout.len(), TAIL_BYTES);
        assert_eq!(p.skipped, 40000 - TAIL_BYTES as u64);
        assert_eq!(p.bytes_seen, 40000);
    }

    /// Test 7 (spool half) — past the cap the spool stops and says so; the
    /// tail keeps everything.
    #[test]
    fn spool_stops_at_the_cap_and_the_tail_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cap.log");
        let file = Mutex::new(std::fs::File::create(&path).unwrap());
        let out = Mutex::new(Output::default());
        append(&out, Which::Stdout, Some(&file), b"0123456789", 10);
        append(&out, Which::Stdout, Some(&file), b"abcdef", 10);
        let o = out.lock().unwrap();
        assert!(o.spool_capped);
        assert_eq!(o.spool_written, 10);
        assert_eq!(o.stdout.total, 16);
        drop(o);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "0123456789");
    }

    /// A spool that cannot be written is a note, not a dead job.
    #[cfg(unix)]
    #[tokio::test]
    async fn unwritable_spool_dir_is_a_note_not_a_failure() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("not-a-dir");
        std::fs::write(&bad, b"file").unwrap();
        let t = JobTable::new();
        let s = SpawnSpec {
            command: "echo ok",
            cwd: dir.path(),
            env: vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())],
            spool_dir: &bad,
            vault: None,
        };
        let id = t.spawn(s).unwrap();
        let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(p.new_stdout, "ok\n");
        assert!(p.spool.is_none());
        assert!(p.spool_note.as_deref().unwrap_or("").starts_with("spool unavailable"), "{p:?}");
    }
}
```

- [x] `cargo nextest run -p mur-agent-runtime bash_jobs` → all green. If `kill_ends_the_whole_group_and_reaps_the_shell` fails on `grandchild outlived`, the group was not created: check that `process_group(0)` is on the `Command` before `spawn`, not after.
- [x] `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo exit=$?` → `exit=0`.
- [x] Commit: `feat(runtime): JobTable — process-group bash jobs with masked spool and owner task (bash-yield T3)`.

---

## Task 4 — `bash` over the table; `bash_wait`, `bash_kill`; registry

**Interfaces.** Consumes: `JobTable`, `Poll`, `Exit`, `JobError` (T3); `ToolStatus::Running` (T1). Produces: `BashTool { …, pub jobs: Arc<JobTable> }`, `BashTool::with_jobs(self, Arc<JobTable>) -> Self`, `BashTool::control_tools(self: &Arc<Self>) -> Vec<Arc<dyn ToolExecutor>>`, `BashTool::finish_poll(&self, poll: Poll, working_dir: &Path, killed: bool) -> ToolOutput`; `crate::tools::bash_control::{BASH_WAIT, BASH_KILL, BashWaitTool, BashKillTool}`; `crate::tools::registry::attach_bash_control(map: &mut HashMap<String, Arc<dyn ToolExecutor>>, controls: Vec<Arc<dyn ToolExecutor>>)`.

- [x] In `bash.rs`, replace the two constants' docs and `resolve_timeout_secs` (0 is now legal — the background case):

```rust
/// Default wait (in seconds) before `bash` yields a handle when the caller
/// doesn't supply `timeout_secs`.
const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// Upper bound on how long ONE call may hold the turn waiting. It is not a
/// bound on the command: past it the command keeps running and the reply
/// carries a `job_id` (spec 2026-09-12 bash-yield D2). It exists so a turn
/// stays responsive to cancel/steer, nothing else.
const MAX_TIMEOUT_SECS: u64 = 600;

/// `timeout_secs` (alias `wait_secs`): missing/invalid → default; negative →
/// default; `0` → return the handle at once; above the cap → the cap.
fn resolve_timeout_secs(requested: Option<i64>) -> u64 {
    match requested {
        Some(secs) if secs >= 0 => (secs as u64).min(MAX_TIMEOUT_SECS),
        _ => DEFAULT_TIMEOUT_SECS,
    }
}
```

and in the existing test `resolve_timeout_secs_defaults_clamps_and_passes_through` change the `Some(0)` line to `assert_eq!(resolve_timeout_secs(Some(0)), 0, "zero is the background case");`.

- [x] Add the field and builders to `BashTool`:

```rust
    /// The runtime-wide job table (spec D3): shared with `bash_wait` and
    /// `bash_kill`, and with `TaskRunner` for deadline/cancel cleanup.
    pub jobs: std::sync::Arc<crate::tools::bash_jobs::JobTable>,
```

In `BashTool::new` set `jobs: crate::tools::bash_jobs::JobTable::new(),` and add:

```rust
    /// Share a job table (production: one per runtime, built in
    /// `supervisor_runner`).
    pub fn with_jobs(mut self, jobs: std::sync::Arc<crate::tools::bash_jobs::JobTable>) -> Self {
        self.jobs = jobs;
        self
    }

    /// The two control tools over this tool's table. Registered together with
    /// `bash` and gated by its policy (`registry::attach_bash_control`).
    pub fn control_tools(self: &std::sync::Arc<Self>) -> Vec<std::sync::Arc<dyn ToolExecutor>> {
        vec![
            std::sync::Arc::new(crate::tools::bash_control::BashWaitTool { bash: self.clone() }),
            std::sync::Arc::new(crate::tools::bash_control::BashKillTool { bash: self.clone() }),
        ]
    }
```

Update the two struct literals in `tools/registry.rs` tests (`bash_tool_included_when_allowed` and the one in `no_servers_empty_result`'s siblings — grep `BashTool {`) to add `jobs: crate::tools::bash_jobs::JobTable::new(),`.

- [x] Replace `def()`'s description and the `timeout_secs` property:

```rust
            description: format!(
                "Run a bash shell command. Returns stdout, then stderr under `[stderr]`; a non-zero exit code appears in the output and is not an error. \
Waits up to `timeout_secs` (default {DEFAULT_TIMEOUT_SECS}s, max {MAX_TIMEOUT_SECS}s) for the command to finish. If it is still running after that you get a `job_id` and the output so far — the command KEEPS RUNNING. \
Call `bash_wait` to wait longer, `bash_kill` to stop it. Pass `timeout_secs: 0` to start a command in the background immediately. Never use `nohup` or `&` to work around the wait."
            ),
```

```rust
                    "timeout_secs": {
                        "type": "integer",
                        "description": format!(
                            "Seconds to wait for the command before yielding a job handle (default {DEFAULT_TIMEOUT_SECS}, clamped to 0-{MAX_TIMEOUT_SECS}). \
The command is NOT killed when this elapses. 0 = return immediately with the handle. `wait_secs` is accepted as an alias."
                        )
                    }
```

- [x] Replace the body of `execute` from `let timeout_secs = ...` to the end with:

```rust
        let timeout_secs = resolve_timeout_secs(
            input
                .get("timeout_secs")
                .or_else(|| input.get("wait_secs"))
                .and_then(serde_json::Value::as_i64),
        );

        let mut env = vec![(
            "PATH".to_string(),
            augmented_path(std::env::var("PATH").ok().as_deref()),
        )];
        // Values leave the vault only here, straight into the child's
        // environment. The model never sees them: the pump masks every chunk
        // before it reaches the tail or the spool (D10).
        if let Some(vault) = &self.secrets {
            env.extend(vault.env_pairs());
        }
        let spool_dir = self.working_dir.join("jobs");
        let job_id = self
            .jobs
            .spawn(crate::tools::bash_jobs::SpawnSpec {
                command: &command,
                cwd: &working_dir,
                env,
                spool_dir: &spool_dir,
                vault: self.secrets.clone(),
            })
            .map_err(|e| match e {
                crate::tools::bash_jobs::JobError::Spawn(io) => {
                    let mut msg = format!("spawn failed: {io}");
                    if crate::tools::fs_policy::is_removable_volume_eperm(&working_dir, &io) {
                        msg.push_str("\n\n");
                        msg.push_str(crate::tools::fs_policy::REMOVABLE_VOLUME_EPERM_HINT);
                    }
                    ToolError::Execution(msg)
                }
                too_many @ crate::tools::bash_jobs::JobError::TooMany(_) => {
                    ToolError::InvalidInput(too_many.to_string())
                }
                other => ToolError::Execution(other.to_string()),
            })?;
        let poll = self
            .jobs
            .poll(&job_id, std::time::Duration::from_secs(timeout_secs))
            .await
            .map_err(|e| ToolError::Execution(e.to_string()))?;
        Ok(self.finish_poll(poll, &working_dir, false))
    }
}

impl BashTool {
    /// Turn a poll into the model-facing reply. Shared by `bash`, `bash_wait`
    /// and `bash_kill` so the three never disagree about what an exit code,
    /// a denial, or a yield looks like. `killed` = the caller was `bash_kill`.
    pub(crate) fn finish_poll(
        &self,
        poll: crate::tools::bash_jobs::Poll,
        working_dir: &Path,
        killed: bool,
    ) -> ToolOutput {
        let mut combined = String::new();
        if poll.skipped > 0 {
            let where_ = poll
                .spool
                .as_ref()
                .map(|p| format!(" — read_file {}", p.display()))
                .unwrap_or_default();
            combined.push_str(&format!("[… {} bytes not shown{where_}]\n", poll.skipped));
        }
        combined.push_str(&poll.new_stdout);
        if !poll.new_stderr.is_empty() {
            if !combined.is_empty() {
                combined.push_str("\n[stderr]\n");
            }
            combined.push_str(&poll.new_stderr);
        }
        let elapsed = crate::bounds::fmt_dur(poll.elapsed);
        let spool_line = poll
            .spool
            .as_ref()
            .map(|p| format!("; full log: {}", p.display()))
            .unwrap_or_default();
        let note = poll
            .spool_note
            .as_ref()
            .map(|n| format!("\n[{n}]"))
            .unwrap_or_default();

        let Some(exit) = poll.exit else {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            combined.push_str(&format!(
                "[still running after {elapsed} — job_id: {}; call bash_wait to keep waiting, bash_kill to stop{spool_line}]{note}",
                poll.job_id
            ));
            return ToolOutput {
                text: combined,
                status: ToolStatus::Running {
                    job_id: poll.job_id,
                    bytes_seen: poll.bytes_seen,
                },
                images: Vec::new(),
            };
        };

        let code = exit.code.unwrap_or(-1);
        let mut status = ToolStatus::Ok;
        if killed {
            if !combined.is_empty() && !combined.ends_with('\n') {
                combined.push('\n');
            }
            match exit.killed_by {
                Some(sig) => combined.push_str(&format!(
                    "[killed {} by {sig} after {elapsed}]{note}",
                    poll.job_id
                )),
                None => combined.push_str(&format!(
                    "[{} had already exited with code {code} after {elapsed}]{note}",
                    poll.job_id
                )),
            }
            return ToolOutput {
                text: combined,
                status,
                images: Vec::new(),
            };
        }
        if exit.code != Some(0) {
            if !combined.is_empty() {
                combined.push('\n');
            }
            combined.push_str(&format!("[exit code: {code}]"));
            if let Some(bin) = spawn_denied_path(exit.code, &poll.new_stderr)
                && let Some((mur_home, agent)) = &self.agent
            {
                let cwd = working_dir.canonicalize().unwrap_or(working_dir.to_path_buf());
                let routes = who_can_exec(mur_home, agent, &bin, Some(&cwd));
                let hint = spawn_denied_hint(&bin, agent, &routes);
                combined.push_str(&hint);
                status = ToolStatus::Denied { detail: hint };
            } else if let Some(hint) = self.explain_write_denial(&poll.new_stderr, working_dir) {
                combined.push_str(&hint);
                status = ToolStatus::Denied { detail: hint };
            } else {
                status = ToolStatus::Failed { exit_code: code };
            }
        }
        combined.push_str(&note);
        ToolOutput {
            text: combined,
            status,
            images: Vec::new(),
        }
    }
}
```

Delete the now-unused `use tokio::process::Command;` at the top of `bash.rs` if the compiler flags it (the zombie test below still needs `Command`; keep the import inside that test if so).

- [x] Replace the test `timeout_kill_path_leaves_no_zombie_children` (its subject, the kill-at-timeout branch, no longer exists) with:

```rust
    /// The yield path leaves no zombie either: the pump reaps the shell.
    /// Regression cover for dogfood issue 11 under the new semantics.
    #[cfg(unix)]
    #[tokio::test]
    async fn yielded_then_finished_jobs_are_reaped_not_left_defunct() {
        let t = make_tool();
        let mut pids = Vec::new();
        for _ in 0..3 {
            let out = t
                .execute(serde_json::json!({"command": "sleep 0.2", "timeout_secs": 0}))
                .await
                .unwrap();
            let job_id = match out.status {
                ToolStatus::Running { job_id, .. } => job_id,
                other => panic!("expected Running, got {other:?}"),
            };
            pids.push(t.jobs.pid(&job_id).unwrap() as libc::pid_t);
        }
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        for pid in pids {
            let mut status: libc::c_int = 0;
            let ret = unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) };
            assert_eq!(ret, -1, "pid {pid} still a child of this process (zombie)");
        }
    }
```

- [x] Add tool-level tests to `bash::tests`:

```rust
    /// D1 at the tool boundary: a yield is `Running`, not an error, and says
    /// so in words the model can act on.
    #[cfg(unix)]
    #[tokio::test]
    async fn timeout_yields_a_running_status_instead_of_killing() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo start; sleep 3", "timeout_secs": 1}))
            .await
            .unwrap();
        let job_id = match &out.status {
            ToolStatus::Running { job_id, bytes_seen } => {
                assert_eq!(*bytes_seen, 6, "{out:?}");
                job_id.clone()
            }
            other => panic!("expected Running, got {other:?}"),
        };
        assert!(out.text.contains("start"), "{}", out.text);
        assert!(out.text.contains("still running"), "{}", out.text);
        assert!(!out.text.contains("timed out"), "{}", out.text);
        assert!(out.text.contains(&job_id), "{}", out.text);
        assert!(crate::tools::bash_jobs::pid_alive(t.jobs.pid(&job_id).unwrap()));
        t.jobs.kill_all().await;
    }

    /// `wait_secs` is an alias for `timeout_secs`.
    #[cfg(unix)]
    #[tokio::test]
    async fn wait_secs_is_an_alias() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "sleep 2", "wait_secs": 0}))
            .await
            .unwrap();
        assert!(matches!(out.status, ToolStatus::Running { .. }), "{out:?}");
        t.jobs.kill_all().await;
    }

    /// A finished command still renders exactly as before the yield existed.
    #[cfg(unix)]
    #[tokio::test]
    async fn finished_command_renders_stdout_then_stderr_then_exit_code() {
        let t = make_tool();
        let out = t
            .execute(serde_json::json!({"command": "echo out; echo err >&2; exit 3"}))
            .await
            .unwrap();
        assert_eq!(out.status, ToolStatus::Failed { exit_code: 3 });
        assert_eq!(out.text, "out\n\n[stderr]\nerr\n\n[exit code: 3]", "{}", out.text);
    }
```

Note the exact text of the last assertion: stdout `out\n`, then `\n[stderr]\n`, then `err\n`, then `\n[exit code: 3]`. If the existing test `vault_values_reach_the_child_environment` now fails because the value is masked in the tool's own reply, change its assertion to `assert_eq!(out.text.trim(), "[SECRET:GITEA_TOKEN]")` and its comment to: "Masking now happens in the pump (D10) because the spool is model-readable; the runner's chokepoint stays as the guard for every other tool."

- [x] Create `mur-agent-runtime/src/tools/bash_control.rs`:

```rust
//! `bash_wait` and `bash_kill`: the two control tools over `bash`'s job table
//! (spec 2026-09-12 bash-yield D6). Thin on purpose — every reply goes
//! through `BashTool::finish_poll` so the three tools never disagree.

use std::sync::Arc;

use super::bash::BashTool;
use super::{ToolError, ToolExecutor, ToolOutput};
use crate::llm::ToolDef;

pub const BASH_WAIT: &str = "bash_wait";
pub const BASH_KILL: &str = "bash_kill";

/// `bash_wait.wait_secs` default.
const DEFAULT_WAIT_SECS: u64 = 60;
/// Same wait cap as `bash` (D2).
const MAX_WAIT_SECS: u64 = 600;

fn job_id_of(input: &serde_json::Value) -> Result<String, ToolError> {
    input
        .get("job_id")
        .and_then(serde_json::Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .ok_or_else(|| ToolError::InvalidInput("missing 'job_id' field".into()))
}

fn wait_secs_of(input: &serde_json::Value) -> u64 {
    match input
        .get("wait_secs")
        .or_else(|| input.get("timeout_secs"))
        .and_then(serde_json::Value::as_i64)
    {
        Some(s) if s >= 0 => (s as u64).min(MAX_WAIT_SECS),
        _ => DEFAULT_WAIT_SECS,
    }
}

fn job_error(e: crate::tools::bash_jobs::JobError) -> ToolError {
    match e {
        unknown @ crate::tools::bash_jobs::JobError::Unknown(..) => {
            ToolError::InvalidInput(unknown.to_string())
        }
        other => ToolError::Execution(other.to_string()),
    }
}

pub struct BashWaitTool {
    pub bash: Arc<BashTool>,
}

#[async_trait::async_trait]
impl ToolExecutor for BashWaitTool {
    fn name(&self) -> &str {
        BASH_WAIT
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: BASH_WAIT.into(),
            description: format!(
                "Keep waiting on a bash command that is still running (a `job_id` from `bash`). \
Returns the output since the last reply. Waits up to `wait_secs` (default {DEFAULT_WAIT_SECS}, max {MAX_WAIT_SECS}); \
if the command is still running after that you get the handle again — call bash_wait again, or bash_kill to stop it."
            ),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "The job_id a previous bash/bash_wait reply gave you" },
                    "wait_secs": { "type": "integer", "description": format!("Seconds to wait before yielding again (default {DEFAULT_WAIT_SECS}, 0-{MAX_WAIT_SECS})") }
                },
                "required": ["job_id"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let job_id = job_id_of(&input)?;
        let wait = std::time::Duration::from_secs(wait_secs_of(&input));
        let poll = self.bash.jobs.poll(&job_id, wait).await.map_err(job_error)?;
        let cwd = self.bash.session_cwd.current();
        Ok(self.bash.finish_poll(poll, &cwd, false))
    }
}

pub struct BashKillTool {
    pub bash: Arc<BashTool>,
}

#[async_trait::async_trait]
impl ToolExecutor for BashKillTool {
    fn name(&self) -> &str {
        BASH_KILL
    }

    fn def(&self) -> ToolDef {
        ToolDef {
            name: BASH_KILL.into(),
            description: "Stop a running bash command (a `job_id` from `bash`): SIGTERM to its whole process group, then SIGKILL after 2 seconds. Returns its last output.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "job_id": { "type": "string", "description": "The job_id a previous bash/bash_wait reply gave you" }
                },
                "required": ["job_id"]
            }),
        }
    }

    async fn execute(&self, input: serde_json::Value) -> Result<ToolOutput, ToolError> {
        let job_id = job_id_of(&input)?;
        let poll = self.bash.jobs.kill(&job_id).await.map_err(job_error)?;
        let cwd = self.bash.session_cwd.current();
        Ok(self.bash.finish_poll(poll, &cwd, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tools::ToolStatus;

    fn bash() -> Arc<BashTool> {
        let base = std::env::temp_dir();
        Arc::new(BashTool::new(
            base.clone(),
            crate::tools::fs_policy::SessionCwd::new(base),
        ))
    }

    /// Test 2 at the tool boundary: bash → Running, bash_wait → Ok with the
    /// exit code, a second bash_wait → InvalidInput naming the job.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_wait_collects_the_exit_once() {
        let b = bash();
        let out = b
            .execute(serde_json::json!({"command": "sleep 0.5; echo done", "timeout_secs": 0}))
            .await
            .unwrap();
        let ToolStatus::Running { job_id, .. } = out.status else {
            panic!("{out:?}")
        };
        let wait = BashWaitTool { bash: b.clone() };
        let out = wait
            .execute(serde_json::json!({"job_id": job_id, "wait_secs": 5}))
            .await
            .unwrap();
        assert_eq!(out.status, ToolStatus::Ok, "{out:?}");
        assert_eq!(out.text, "done\n");
        let again = wait.execute(serde_json::json!({"job_id": job_id})).await;
        match again {
            Err(ToolError::InvalidInput(msg)) => assert!(msg.contains(&job_id), "{msg}"),
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    /// Test 3 at the tool boundary: kill is `Ok`, names the signal and the
    /// elapsed time, and the pid is gone.
    #[cfg(unix)]
    #[tokio::test]
    async fn bash_kill_is_ok_and_names_the_signal() {
        let b = bash();
        let out = b
            .execute(serde_json::json!({"command": "sleep 30", "timeout_secs": 0}))
            .await
            .unwrap();
        let ToolStatus::Running { job_id, .. } = out.status else {
            panic!("{out:?}")
        };
        let pid = b.jobs.pid(&job_id).unwrap();
        let out = BashKillTool { bash: b.clone() }
            .execute(serde_json::json!({"job_id": job_id}))
            .await
            .unwrap();
        assert_eq!(out.status, ToolStatus::Ok, "{out:?}");
        assert!(out.text.contains("killed") && out.text.contains("SIG"), "{}", out.text);
        assert!(!crate::tools::bash_jobs::pid_alive(pid));
    }

    #[tokio::test]
    async fn unknown_job_is_invalid_input() {
        let b = bash();
        let r = BashWaitTool { bash: b.clone() }
            .execute(serde_json::json!({"job_id": "j-nope"}))
            .await;
        assert!(matches!(r, Err(ToolError::InvalidInput(_))), "{r:?}");
        let r = BashKillTool { bash: b }
            .execute(serde_json::json!({}))
            .await;
        assert!(matches!(r, Err(ToolError::InvalidInput(_))), "{r:?}");
    }
}
```

Register it in `tools/mod.rs`: `pub mod bash_control;` after `pub mod bash;`.

- [x] In `tools/registry.rs` add, after `build_tools`:

```rust
/// Register `bash_wait`/`bash_kill` iff `bash` itself was registered — the
/// three share one gate (spec D6/D11). Call after `build_tools`.
pub fn attach_bash_control(
    map: &mut HashMap<String, Arc<dyn ToolExecutor>>,
    controls: Vec<Arc<dyn ToolExecutor>>,
) {
    if !map.contains_key("bash") {
        return;
    }
    for tool in controls {
        map.insert(tool.name().to_string(), tool);
    }
}
```

and the test (test 16), inside `registry::tests`:

```rust
    /// Test 16: denying `bash` registers none of the three; allowing it
    /// registers all three.
    #[tokio::test]
    async fn bash_control_tools_follow_bash_registration() {
        use crate::tools::bash::BashTool;
        let mk = || {
            Arc::new(BashTool::new(
                std::path::PathBuf::from("/tmp"),
                crate::tools::fs_policy::SessionCwd::new(std::path::PathBuf::from("/tmp")),
            ))
        };
        let deny = vec![ToolRule {
            pattern: "bash".into(),
            policy: ToolPolicy::Deny,
            risk: None,
        }];
        for (rules, expect) in [(deny, false), (vec![], true)] {
            let bash = mk();
            let exec: Arc<dyn ToolExecutor> = bash.clone();
            let pool = McpPool::new(vec![], SandboxPolicy::default(), None);
            let (_defs, mut map) =
                build_tools(Some((bash.def(), exec)), None, None, None, &[], &rules, pool).await;
            attach_bash_control(&mut map, bash.control_tools());
            assert_eq!(map.contains_key("bash"), expect);
            assert_eq!(map.contains_key("bash_wait"), expect);
            assert_eq!(map.contains_key("bash_kill"), expect);
        }
    }
```

- [x] `cargo nextest run -p mur-agent-runtime tools::` → green. Then the full crate: `cargo nextest run -p mur-agent-runtime` → green (any test that asserted `command timed out` must now assert `still running`; list each in the commit body).
- [x] `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo exit=$?` → `exit=0`.
- [x] Commit: `feat(runtime): bash yields a job handle; bash_wait/bash_kill (bash-yield T4)`.

---

## Task 5 — `TaskRunner`: owner scope, `running` event flag, policy alias, fingerprint fold, cleanup

**Interfaces.** Consumes: `CURRENT_TASK_ID`, `JobTable` (T3); `ToolStatus::Running` (T1). Produces: `TaskRunner::with_bash_jobs(self, Arc<JobTable>) -> Self`, `TaskRunner::kill_all_jobs(&self) -> usize` (async), `fn policy_name(tool: &str) -> &str` (private).

- [x] Add the field to `TaskRunner` (after `secrets`): `bash_jobs: Option<Arc<crate::tools::bash_jobs::JobTable>>,` and `bash_jobs: None,` in `with_backend`. Add after `with_secrets`:

```rust
    /// The bash job table (spec D3/D8), so the loop can end a task's jobs on
    /// an unattended stop or a cancel, and the supervisor every job at exit.
    pub fn with_bash_jobs(mut self, jobs: Arc<crate::tools::bash_jobs::JobTable>) -> Self {
        self.bash_jobs = Some(jobs);
        self
    }

    /// Runtime shutdown: every running bash job, whoever started it.
    pub async fn kill_all_jobs(&self) -> usize {
        match &self.bash_jobs {
            Some(t) => t.kill_all().await,
            None => 0,
        }
    }

    async fn kill_jobs_of(&self, task_id: &str) {
        if let Some(t) = &self.bash_jobs {
            let n = t.kill_owned_by(task_id).await;
            if n > 0 {
                tracing::info!(task_id, jobs = n, "ended the task's running bash jobs");
            }
        }
    }

    /// D8: every tool executes inside the owner task's scope, from BOTH
    /// execute sites, so `bash` can stamp its job. One helper — a third site
    /// must call it too or its jobs belong to nobody.
    async fn execute_scoped(
        tool: &dyn crate::tools::ToolExecutor,
        task_id: &str,
        input: serde_json::Value,
    ) -> Result<crate::tools::ToolOutput, crate::tools::ToolError> {
        crate::tools::bash_jobs::CURRENT_TASK_ID
            .scope(task_id.to_string(), tool.execute(input))
            .await
    }
```

- [x] In `handle_tool_call`, at **both** execute sites (each is preceded by `let tool = tool.unwrap();`, so `tool: Arc<dyn ToolExecutor>`), replace `tool.execute(call.input.clone()).await` with `Self::execute_scoped(tool.as_ref(), task_id, call.input.clone()).await`, and in **both** `step/completed` JSON objects add, after the `"denied"` line:

```rust
                                    "running": matches!(status, crate::tools::ToolStatus::Running { .. }),
```

- [x] Replace `effective_tool_policy`:

```rust
/// D11: the control tools resolve as themselves first, then as `bash`.
fn policy_name(tool: &str) -> &str {
    match tool {
        crate::tools::bash_control::BASH_WAIT | crate::tools::bash_control::BASH_KILL => "bash",
        other => other,
    }
}

fn effective_tool_policy(
    rules: &[mur_common::agent::ToolRule],
    tool_name: &str,
) -> mur_common::agent::ToolPolicy {
    use mur_common::agent::{ToolPolicy, resolve_tool_policy_opt};
    if crate::tools::suggest::suggest_replies_allowed(tool_name) {
        return ToolPolicy::Allow;
    }
    match resolve_tool_policy_opt(rules, tool_name)
        .or_else(|| resolve_tool_policy_opt(rules, policy_name(tool_name)))
    {
        Some(explicit) => explicit,
        None if crate::tools::recall::recall_needs_no_approval(tool_name) => ToolPolicy::Allow,
        None => ToolPolicy::default(),
    }
}
```

- [x] Fingerprint fold (D4). In the zip loop `for (call, entry) in resp.tool_calls.iter().zip(results.iter())`, declare before the loop `let mut progress_calls: Vec<(String, u64)> = Vec::with_capacity(results.len());` and add as the loop's last statements:

```rust
                // D4: a yield that delivered new bytes is progress; one that
                // delivered nothing repeats the previous fingerprint exactly.
                let mut args_fp = fingerprint_args(&call.input);
                if let crate::tools::ToolStatus::Running { bytes_seen, .. } = &entry.status {
                    args_fp ^= fingerprint_str(&format!("bytes_seen:{bytes_seen}"));
                }
                progress_calls.push((call.tool_name.clone(), args_fp));
```

and replace the `progress.observe(&resp.tool_calls.iter().map(...).collect::<Vec<_>>(), now)` call with `progress.observe(&progress_calls, std::time::Instant::now());`.

- [x] Cleanup on stop (D3). Immediately before each of the two `self.graceful_exit(client, &history, LoopStop::Deadline, …)` and `LoopStop::Stuck` calls in the loop (the unattended arm only — the attended arm warns and continues), insert `self.kill_jobs_of(task_id).await;`. In `cancel`, make the kill unconditional on cancellability:

```rust
    pub async fn cancel(&self, task_id: &str) -> Result<(), String> {
        let tx = self
            .cancel_signals
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(task_id);
        // Whether or not the generation is still cancellable, the task's
        // jobs are (D3): a cancel means "stop everything this task started".
        self.kill_jobs_of(task_id).await;
        match tx {
            Some(tx) => {
                let _ = tx.send(());
                Ok(())
            }
            None => Err(format!("task {task_id} not cancellable")),
        }
    }
```

- [x] Tests, in `task_runner::tests` (reuse the existing `loop_spec`, `end_turn_response`, `empty_pending_approvals`, `SequenceLlm`, `task_usage`):

```rust
    /// Test 15 — D11 policy aliasing.
    #[test]
    fn bash_control_tools_inherit_bashs_rule_unless_named() {
        use mur_common::agent::{ToolPolicy, ToolRule};
        let rule = |p: &str, policy| ToolRule {
            pattern: p.into(),
            policy,
            risk: None,
        };
        let allow = vec![rule("bash", ToolPolicy::Allow)];
        assert_eq!(effective_tool_policy(&allow, "bash_wait"), ToolPolicy::Allow);
        assert_eq!(effective_tool_policy(&allow, "bash_kill"), ToolPolicy::Allow);
        let ask = vec![rule("bash", ToolPolicy::Ask)];
        assert_eq!(effective_tool_policy(&ask, "bash_wait"), ToolPolicy::Ask);
        let mixed = vec![rule("bash", ToolPolicy::Allow), rule("bash_kill", ToolPolicy::Deny)];
        assert_eq!(effective_tool_policy(&mixed, "bash_wait"), ToolPolicy::Allow);
        assert_eq!(effective_tool_policy(&mixed, "bash_kill"), ToolPolicy::Deny);
        assert_eq!(effective_tool_policy(&[], "bash_wait"), ToolPolicy::default());
    }

    fn bash_call(id: &str, command: &str, timeout_secs: u64) -> crate::llm::LlmResponse {
        crate::llm::LlmResponse {
            text: String::new(),
            input_tokens: 5,
            output_tokens: 5,
            model: "test".into(),
            tool_calls: vec![crate::llm::ToolCallResult {
                call_id: id.into(),
                tool_name: "bash".into(),
                input: serde_json::json!({"command": command, "timeout_secs": timeout_secs}),
            }],
            stop_reason: crate::llm::StopReason::ToolUse,
        }
    }

    fn runner_with_real_bash(
        responses: Vec<crate::llm::LlmResponse>,
        deadline: Option<&str>,
    ) -> (Arc<TaskRunner>, Arc<crate::tools::bash_jobs::JobTable>) {
        use crate::llm::stub::SequenceLlm;
        let base = std::env::temp_dir();
        let jobs = crate::tools::bash_jobs::JobTable::new();
        let bash: Arc<dyn crate::tools::ToolExecutor> = Arc::new(
            crate::tools::bash::BashTool::new(
                base.clone(),
                crate::tools::fs_policy::SessionCwd::new(base),
            )
            .with_jobs(jobs.clone()),
        );
        let runner = Arc::new(
            TaskRunner::with_llm(Arc::new(SequenceLlm::new(responses)))
                .with_pending_approvals(empty_pending_approvals())
                .with_notifier(tokio::sync::mpsc::channel(16).0)
                .with_tools(vec![bash])
                .with_tools_policy(vec![mur_common::agent::ToolRule {
                    pattern: "bash".into(),
                    policy: mur_common::agent::ToolPolicy::Allow,
                    risk: None,
                }])
                .with_bash_jobs(jobs.clone())
                .with_iteration_ceiling(50)
                .with_limits(
                    mur_common::limits::Limits {
                        deadline: deadline.map(str::to_string),
                        stuck: Some("off".into()),
                        cost_usd: None,
                    },
                    None,
                ),
        );
        (runner, jobs)
    }

    /// Test 12 — an unattended deadline stop ends the task's jobs; an
    /// attended turn that ends normally leaves them running.
    #[cfg(unix)]
    #[tokio::test]
    async fn unattended_deadline_kills_the_tasks_jobs_and_attended_does_not() {
        // Unattended: job spawned at once, a 2 s call carries the loop past
        // the 1 s deadline, the next iteration stops and kills the job.
        let (runner, jobs) = runner_with_real_bash(
            vec![
                bash_call("c0", "sleep 30", 0),
                bash_call("c1", "sleep 2", 5),
                end_turn_response("DONE"),
            ],
            Some("1s"),
        );
        let mut spec = loop_spec("deadline");
        spec.attended = false;
        spec.deadline_secs = Some(1);
        let out = runner.run_sync(spec).await;
        assert_eq!(task_usage(&out)["stop_reason"], "deadline");
        assert!(jobs.running_ids().is_empty(), "{:?}", jobs.running_ids());

        // Attended: same script, no deadline applies, the job outlives the turn.
        let (runner, jobs) = runner_with_real_bash(
            vec![bash_call("c0", "sleep 30", 0), end_turn_response("DONE")],
            Some("1s"),
        );
        let mut spec = loop_spec("attended");
        spec.attended = true;
        runner.run_sync(spec).await;
        assert_eq!(jobs.running_ids().len(), 1, "an attended turn must not kill its jobs");
        jobs.kill_all().await;
    }

    /// Test 13 — `tasks/cancel` ends the task's jobs even when the
    /// generation is no longer cancellable.
    #[cfg(unix)]
    #[tokio::test]
    async fn cancel_kills_the_tasks_jobs() {
        let (runner, jobs) = runner_with_real_bash(vec![], None);
        let base = std::env::temp_dir();
        let id = crate::tools::bash_jobs::CURRENT_TASK_ID
            .scope("task-c".to_string(), async {
                jobs.spawn(crate::tools::bash_jobs::SpawnSpec {
                    command: "sleep 30",
                    cwd: &base,
                    env: vec![("PATH".into(), std::env::var("PATH").unwrap_or_default())],
                    spool_dir: &base,
                    vault: None,
                })
            })
            .await
            .unwrap();
        let pid = jobs.pid(&id).unwrap();
        let r = runner.cancel("task-c").await;
        assert!(r.is_err(), "nothing registered a cancel signal: {r:?}");
        assert!(!crate::tools::bash_jobs::pid_alive(pid), "cancel left the job running");
    }

    /// Test 14 — D4: the stuck fingerprint differs when bytes arrived and
    /// repeats when nothing did.
    #[test]
    fn running_fingerprint_folds_bytes_seen() {
        let input = serde_json::json!({"job_id": "j-1"});
        let fp = |bytes_seen: u64| {
            fingerprint_args(&input) ^ fingerprint_str(&format!("bytes_seen:{bytes_seen}"))
        };
        assert_ne!(fp(10), fp(20));
        assert_eq!(fp(20), fp(20));
        assert_ne!(fp(10), fingerprint_args(&input), "a yield is not the bare call");
    }
```

- [x] Add a check that the scope reaches the tool from both sites: extend the existing `CountingBashTool` in `task_runner::tests` (line ≈3630) with a `seen_task: Arc<Mutex<Option<String>>>` field set from `crate::tools::bash_jobs::current_task_id()` inside `execute`, and in ONE existing test that uses it under `Allow` and ONE under `Ask` (grep `CountingBashTool {` — four uses; pick the first Allow-policy one and the first Ask-policy one) assert `seen_task.lock().unwrap().is_some()` after the run. Name the assertions "D8: the owner scope reaches the Allow site" / "… the Ask site".
- [x] `cargo nextest run -p mur-agent-runtime task_runner` → green (the deadline test is ≈4 s; `SLOW` is fine, a hang is not).
- [x] `cargo clippy -p mur-agent-runtime --all-targets -- -D warnings; echo exit=$?` → `exit=0`.
- [x] Commit: `feat(runtime): owner-scoped tool execution, running flag, bash policy alias, job cleanup on stop/cancel (bash-yield T5)`.

---

## Task 6 — Wiring: supervisor builds one table, attaches the control tools, kills all at exit

**Interfaces.** Consumes: `JobTable::new`, `BashTool::{with_jobs, control_tools}`, `attach_bash_control`, `TaskRunner::{with_bash_jobs, kill_all_jobs}`. Produces: `build_runner(.., bash_jobs: Option<Arc<JobTable>>)` (new last parameter).

- [x] `supervisor_runner.rs`, in `build_provider_runner`: build the table and the concrete tool:

```rust
    let bash_jobs = crate::tools::bash_jobs::JobTable::new();
    let bash = Arc::new(
        BashTool::new(agent_home.to_path_buf(), session_cwd.clone())
            .with_agent(mur_home.clone(), profile.inner.name.clone())
            .with_write_grants(/* unchanged */)
            .with_secrets(secrets.clone())
            .with_jobs(bash_jobs.clone()),
    );
    let bash_exec: Arc<dyn crate::tools::ToolExecutor> = bash.clone();
```

(keep the existing `with_write_grants(...)` argument verbatim). After `let (_defs, mut tool_map) = build_tools(...).await;` add:

```rust
    // bash_wait / bash_kill ride on bash's registration and policy (D6/D11).
    crate::tools::registry::attach_bash_control(&mut tool_map, bash.control_tools());
```

Add `bash_jobs: Option<Arc<crate::tools::bash_jobs::JobTable>>,` as the last parameter of `build_runner`, apply it with `if let Some(j) = bash_jobs { runner = runner.with_bash_jobs(j); }` next to the `secrets` line, pass `Some(bash_jobs.clone())` from the `build` closure in `build_provider_runner`, and `None` in the test call `build_runner_applies_profile_limits` (`task_runner.rs` ≈4595).

- [x] `supervisor.rs`, in the graceful shutdown after the drain block and before `for t in transport_tasks`:

```rust
    // Every bash job is a process group this runtime started; nothing else
    // will end them once we are gone (spec D3/D9).
    let killed = runner.kill_all_jobs().await;
    if killed > 0 {
        info!(jobs = killed, "ended running bash jobs");
    }
```

- [x] `cargo check -p mur-agent-runtime` → clean; `cargo nextest run -p mur-agent-runtime` → green.
- [x] `grep -rn "build_runner\|BashTool" mur-hub-gui/src-tauri/src mur-gui-core/src` → expect no matches (both are runtime-internal). If there is a match, fix the call and note it in the commit.
- [x] Commit: `feat(runtime): one job table per runtime, control tools attached, kill_all at shutdown (bash-yield T6)`.

---

## Task 7 — murmur shows ⏳ for a yielded call

**Interfaces.** Consumes: the `running` key on `step/completed` (T5). Produces: `StepEvent::Completed { .., running: bool }`, `StreamMsg::StepCompleted { .., running: bool }`, `App::update_step_completed(.., denied: bool, running: bool)`, `CallOutcome::Running`, `StepState::Yielded`, glyph `⏳`.

- [x] `mur-core/src/a2a_dial.rs`: add to `StepEvent::Completed`

```rust
        /// The call yielded and the command is still running (a runtime ≥ the
        /// bash-yield release sends this; older ones omit it = not running).
        running: bool,
```

and in `parse_step`: `running: p.get("running").and_then(Value::as_bool).unwrap_or(false),`. In the test `parses_completed` add `"running": false` to the JSON and `running` to the destructuring with `assert!(!running);`. Add a second test:

```rust
    #[test]
    fn parses_completed_running_flag() {
        let p = serde_json::json!({
            "step_id": "s3", "task_id": "t3", "ok": true, "output": "…",
            "truncated": false, "full_len": 1u64, "error": null,
            "duration_ms": 30000u64, "denied": false, "running": true
        });
        match parse_step(&p, true) {
            StepEvent::Completed { running, ok, .. } => {
                assert!(running);
                assert!(ok, "a yield is not an error");
            }
            other => panic!("{other:?}"),
        }
    }
```

- [x] `mur-core/src/cmd/agent/cli/stream.rs`: add `running: bool,` to `StreamMsg::StepCompleted` (after `denied`) and thread it through the `StepEvent::Completed { .. } => StreamMsg::StepCompleted { .. }` conversion (both the destructuring and the constructor).
- [x] `mur-core/src/cmd/agent/cli/step.rs`:

```rust
pub enum StepState {
    Running,
    Done,
    Error,
    /// The call yielded: the runtime answered, the command did not finish.
    /// Neither `Done` (nothing is proven) nor `Error` (nothing failed), and
    /// not `Running` — that spinner is for a call the runtime has not
    /// answered yet and gets abandoned when the turn ends.
    Yielded,
}
```

```rust
pub enum CallOutcome {
    Ok,
    Failed,
    Denied,
    /// Yielded — still running under a job id.
    Running,
}
```

in `complete`: `CallOutcome::Running => StepState::Yielded,`; in `glyph`: `StepState::Yielded => "⏳",`. Tests:

```rust
    #[test]
    fn complete_running_sets_yielded_and_hourglass() {
        let mut c = card();
        c.complete(CallOutcome::Running, "[still running …]".into(), false, 20, None, 30_000);
        assert_eq!(c.state, StepState::Yielded);
        assert_eq!(c.glyph(), "⏳");
        assert_ne!(c.state, StepState::Error);
        assert_ne!(c.state, StepState::Done);
    }
```

- [x] `mur-core/src/cmd/agent/cli/app.rs`: add `running: bool` as the last parameter of `update_step_completed` and make the outcome

```rust
            let outcome = match (ok, denied, running) {
                (_, true, _) => super::step::CallOutcome::Denied,
                (_, _, true) => super::step::CallOutcome::Running,
                (true, _, _) => super::step::CallOutcome::Ok,
                (false, _, _) => super::step::CallOutcome::Failed,
            };
```

Append `, false` to the four existing test calls (lines ≈2324, 2344, 2529, 2547) and add a test next to them:

```rust
    /// Test 18 — a yield is ⏳, and the end of the turn does not abandon it
    /// the way it abandons a card the runtime never answered.
    #[test]
    fn a_running_step_renders_yielded_not_done() {
        let mut a = app();
        a.begin_user_turn("hi");
        a.push_step_started(
            "s1".into(),
            "bash".into(),
            serde_json::json!({ "command": "cargo test" }),
        );
        a.update_step_completed(
            "s1",
            true,
            "[still running after 30s — job_id: j-1]".into(),
            false,
            40,
            None,
            30_000,
            false,
            true,
        );
        let state = |a: &App| {
            a.messages
                .iter()
                .rev()
                .find_map(|m| m.step.as_ref())
                .map(|c| (c.state, c.glyph()))
                .unwrap()
        };
        assert_eq!(state(&a), (super::step::StepState::Yielded, "⏳"));
        a.resolve_open_steps("turn ended");
        assert_eq!(state(&a).0, super::step::StepState::Yielded, "abandon must skip a yield");
    }
```

(`app()` and `begin_user_turn` are the same helpers the neighbouring test `update_step_completed_marks_card_done` uses; `App` is the type in this file.)

- [x] `mur-core/src/cmd/agent/cli/mod.rs`: destructure `running` in the `StreamMsg::StepCompleted { .. }` arm and pass it as the last argument. `render_card.rs` needs no change (`_ => theme.accent` covers `Yielded`); confirm with `grep -n "StepState::" mur-core/src/cmd/agent/cli/render_card.rs` that no exhaustive match exists.
- [x] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core cli::step cli::app a2a_dial` → green.
- [x] `cargo clippy -p mur-core --all-targets -- -D warnings; echo exit=$?` → `exit=0`.
- [x] Commit: `feat(murmur): ⏳ yielded step state from the runtime's running flag (bash-yield T7)`.

---

## Task 8 — Docs

- [ ] Spec `docs/superpowers/specs/2026-09-12-bash-yield-not-kill-design.md`: in §3.6 change the spool row's "Why" to "log lands on disk; past the cap the spool stops growing, the tail stays live, and the reply says so"; in §3.4 replace the Windows paragraph with "Windows: `taskkill /F /T /PID <pid>` — a tree kill, so grandchildren die there too (`ponytail:` note in code names Job Objects as the upgrade if an orphan is ever reported)"; in §3.7 note that the settlement line carries the job id (elapsed is in the tool text). Set **Status** to "Implemented in #<PR>".
- [x] `README.md`: in the agent tools list, the `bash` entry becomes "`bash` — runs a command; waits up to `timeout_secs` then yields a `job_id` and the command keeps running; `bash_wait` / `bash_kill` continue or stop it."
- [ ] Invoke the **`update-docs`** skill for the docs site (`https://app.mur.run/docs/core`) and product page; the change is user-facing (three surfaces per CLAUDE.md).
- [ ] Release-note line for the next bump PR (put it in this PR's description so the release author copies it): "`bash` no longer kills a command at `timeout_secs`; it returns a job handle and the command keeps running. New tools `bash_wait`, `bash_kill`. Agents pick this up on restart."
- [x] Commit: `docs: bash yield — spec status, README tool list (bash-yield T8)`.

---

## Task 9 — Whole-workspace verification and live check

- [x] `cargo fmt --all -- --check; echo exit=$?` → `exit=0`.
- [x] `cargo clippy --workspace --all-targets -- -D warnings; echo exit=$?` → `exit=0`.
- [x] `cargo nextest run -p mur-agent-runtime; echo exit=$?` → `exit=0`.
- [x] `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUST_MIN_STACK=33554432 cargo nextest run -p mur-core; echo exit=$?` → `exit=0`.
- [ ] Hub, **last**: `cd mur-hub-gui/src-tauri && cargo check; echo exit=$?` → `exit=0` (symlink `mur-hub-gui/ui/dist` from the main checkout if the worktree lacks it; remove the symlink before committing).
- [ ] Live (spec §5): `./build.sh --install`, `mur agent restart <agent> --stale`, then from murmur ask the agent to run `cargo build --release` in a repo it can reach. Expect the step card `⏳ bash` and the settlement line `⏳ bash · still running (j-…)`; a `bash_wait` lands the exit code; a `bash_kill` of a second `cargo build` followed by `pgrep -f rustc` on the host shows nothing from that build. Record the three observations (card, settlement, pgrep) in the PR description — these are the D9 and seatbelt checks that no unit test can stand in for.
- [ ] Open the PR: title `feat: bash timeout is a yield, not a kill — job handles, bash_wait/bash_kill (#1285 spec)`, body = the spec's D1–D12 in one line each, the release-note line from T8, the live observations.

## Self-review

- Spec coverage: D1 T3/T4 · D2 T4 · D3 T3/T5/T6 · D4 T5 · D5 T1/T4 · D6 T4 · D7 T3/T4 · D8 T3/T5 · D9 T3 · D10 T2/T3 · D11 T5 · D12 T3 · §3.7 T1/T5/T7 · §4 rows T3/T4 · §5 tests 1–18 → 1 T3, 2 T3/T4, 3 T3/T4, 4 T3, 5 T3, 6 T3, 7 T3, 8 T3, 9 T2, 10 T3, 11 T3, 12 T5, 13 T5, 14 T5, 15 T5, 16 T4, 17 T1, 18 T7; live T9.
- Cross-task names: `JobTable::{new, spawn, poll, kill, kill_owned_by, kill_all, running_ids, pid}` (T3) used verbatim in T4/T5/T6; `BashTool::{jobs, with_jobs, control_tools, finish_poll}` (T4) used in T4 tests/T6; `attach_bash_control` (T4) in T6; `with_bash_jobs`/`kill_all_jobs` (T5) in T6; `ToolStatus::Running { job_id, bytes_seen }` (T1) in T3? — no, T3 does not construct it; T4 constructs it, T5 matches it; `running` key (T5) read in T7.
- Deviations from the spec, each carried into the spec text by T8: spool stop-at-cap instead of head truncation; Windows tree kill via `taskkill` instead of direct-child-only; ledger line without elapsed.
