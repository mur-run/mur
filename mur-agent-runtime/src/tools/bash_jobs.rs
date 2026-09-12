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
        assert!(
            t0.elapsed() < Duration::from_millis(2500),
            "waited the whole sleep"
        );
        assert!(
            pid_alive(t.pid(&id).unwrap()),
            "the clock killed the child"
        );
        assert_eq!(t.running_ids(), vec![id.clone()]);
        // Test 2 — a later wait collects the exit and the job is gone.
        let p = t.poll(&id, Duration::from_secs(5)).await.unwrap();
        assert_eq!(p.exit.as_ref().and_then(|e| e.code), Some(0), "{p:?}");
        assert!(matches!(
            t.poll(&id, Duration::ZERO).await,
            Err(JobError::Unknown(..))
        ));
    }

    /// Test 5 — zero wait is the background case.
    #[cfg(unix)]
    #[tokio::test]
    async fn zero_wait_returns_at_once_before_any_output() {
        let dir = tempfile::tempdir().unwrap();
        let t = JobTable::new();
        let id = t
            .spawn(spec("sleep 0.5; echo late", dir.path(), None))
            .unwrap();
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
        let grandchild: u32 = p
            .new_stdout
            .trim()
            .parse()
            .expect("grandchild pid on stdout");
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
            .scope("task-A".to_string(), async {
                t.spawn(spec("sleep 30", dir.path(), None))
            })
            .await
            .unwrap();
        let b = CURRENT_TASK_ID
            .scope("task-B".to_string(), async {
                t.spawn(spec("sleep 30", dir.path(), None))
            })
            .await
            .unwrap();
        let orphan = t.spawn(spec("sleep 30", dir.path(), None)).unwrap();
        // Captured before killing: `kill` removes an exited job from the
        // table (§4, "reported once"), so `pid(&a)` after `kill_owned_by`
        // would return `None` — and `pid_alive(0)` signals this process's
        // own group via `kill(0, 0)`, which succeeds, so the bug hides as a
        // false pass rather than a panic.
        let pid_a = t.pid(&a).unwrap();
        let pid_b = t.pid(&b).unwrap();
        let pid_orphan = t.pid(&orphan).unwrap();
        assert_eq!(t.kill_owned_by("task-nope").await, 0);
        assert_eq!(t.kill_owned_by("task-A").await, 1);
        assert!(wait_dead(pid_a, KILL_GRACE + Duration::from_secs(1)).await);
        assert!(pid_alive(pid_b), "B's job died with A");
        assert!(pid_alive(pid_orphan));
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
            .spawn(spec(
                "head -c 40000 /dev/zero | tr '\\0' x",
                dir.path(),
                None,
            ))
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
        assert!(
            p.spool_note
                .as_deref()
                .unwrap_or("")
                .starts_with("spool unavailable"),
            "{p:?}"
        );
    }
}
