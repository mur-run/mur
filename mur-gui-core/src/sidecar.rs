//! Agent lifecycle supervisor for MUR Hub.
//!
//! The Hub spawns each interactive agent runtime as a direct child process and
//! owns its lifetime — no launchd / systemd. This guarantees exactly one
//! runtime per agent (so nothing competes for the agent's lock) and no orphaned
//! processes: children are killed when the Hub shuts down. `start()` is
//! idempotent — if a live runtime already holds the lock it is reused. A
//! 5-second poll reflects child/lock liveness in the watch channel.
//!
//! (launchd-based always-on background services remain available separately via
//! `mur agent install-service` for non-interactive/scheduled agents.)

use mur_common::lock_file::{SentinelProbe, probe_sentinel};
use serde::Serialize;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;
use tokio::sync::oneshot;
use tokio::sync::watch;
use tracing::{info, warn};

mod boot_window;
mod handover;
pub use handover::{Watched, stop_child};

/// How often the actor re-checks child and lock liveness.
pub const STATUS_POLL: Duration = Duration::from_secs(5);

// ─── Public types ──────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RuntimeState {
    /// Agent process is running (pid unknown in OS-managed mode).
    Running { pid: u32 },
    /// Agent is not running.
    Stopped,
    /// Waiting before next restart attempt (OS handles this; kept for API compat).
    Restarting { attempt: u32, backoff_secs: u64 },
    /// Too many crashes; OS has backed off (kept for API compat).
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AgentRuntimeStatus {
    pub name: String,
    pub state: RuntimeState,
    /// The startup-override flags the live runtime booted with, as the CLI
    /// spells them; absent when it runs with normal permissions.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub relaxed: Option<Vec<String>>,
}

// ─── Internal messages ──────────────────────────────────────────────────────

enum Msg {
    /// Start an agent and report back whether it actually launched, so the
    /// caller (and ultimately the Hub UI) can surface failures instead of
    /// swallowing them into the log.
    Start(String, oneshot::Sender<Result<(), String>>),
    Stop(String),
    /// Lend the child to a blocking caller (the override restart's stop).
    TakeChild(String, oneshot::Sender<Option<Child>>),
    /// A lent child coming back.
    ReturnChild(String, Child),
    /// Start like `Start`, and also report the pid and its exit.
    StartWatched(String, oneshot::Sender<Result<Watched, String>>),
    Shutdown,
}

// ─── Supervisor handle ──────────────────────────────────────────────────────

/// OS-managed agent supervisor. Clone freely — all copies share the actor.
#[derive(Clone)]
pub struct Supervisor {
    msg_tx: tokio::sync::mpsc::Sender<Msg>,
    status_rx: watch::Receiver<Vec<AgentRuntimeStatus>>,
}

impl Supervisor {
    /// Create the supervisor and start its background actor + status poller.
    pub fn new(mur_home: PathBuf) -> Self {
        Self::with_poll_interval(mur_home, STATUS_POLL)
    }

    /// [`Supervisor::new`] with another poll period; tests use a short one so
    /// an exit is noticed without waiting [`STATUS_POLL`].
    pub fn with_poll_interval(mur_home: PathBuf, poll: Duration) -> Self {
        let (msg_tx, msg_rx) = tokio::sync::mpsc::channel::<Msg>(64);
        let (status_tx, status_rx) = watch::channel::<Vec<AgentRuntimeStatus>>(Vec::new());
        tokio::spawn(os_actor(mur_home, msg_rx, status_tx, poll));
        Supervisor { msg_tx, status_rx }
    }

    pub fn status_receiver(&self) -> watch::Receiver<Vec<AgentRuntimeStatus>> {
        self.status_rx.clone()
    }

    /// Register and start an agent via the OS init system.
    ///
    /// Returns `Err` with a human-readable reason if the agent could not be
    /// launched (e.g. the runtime binary is missing or the service refused to
    /// start), so callers can surface it to the user instead of failing silently.
    pub async fn start(&self, name: impl Into<String>) -> Result<(), String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.msg_tx
            .send(Msg::Start(name.into(), reply_tx))
            .await
            .map_err(|_| "supervisor is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "supervisor dropped the start request".to_string())?
    }

    /// Stop an agent via the OS init system.
    pub async fn stop(&self, name: impl Into<String>) {
        let _ = self.msg_tx.send(Msg::Stop(name.into())).await;
    }

    /// Take the supervised child for `slug` out of the actor, so a blocking
    /// caller can stop it without blocking the actor. `None` when the Hub
    /// holds no child for it.
    pub async fn take_child(&self, slug: impl Into<String>) -> Option<Child> {
        let (tx, rx) = oneshot::channel();
        self.msg_tx
            .send(Msg::TakeChild(slug.into(), tx))
            .await
            .ok()?;
        rx.await.ok().flatten()
    }

    /// Give a taken child back without waiting; on a full or closed channel
    /// the child comes back to the caller.
    pub fn try_return_child(&self, slug: String, child: Child) -> Result<(), Child> {
        use tokio::sync::mpsc::error::TrySendError;
        match self.msg_tx.try_send(Msg::ReturnChild(slug, child)) {
            Ok(()) => Ok(()),
            Err(TrySendError::Full(m) | TrySendError::Closed(m)) => Err(take_back(m)),
        }
    }

    /// For a blocking thread after `try_return_child` found the channel full:
    /// wait up to `within` for room. Must not be called on a runtime thread.
    pub fn return_child_within(
        &self,
        rt: &tokio::runtime::Handle,
        slug: String,
        child: Child,
        within: Duration,
    ) -> Result<(), Child> {
        let tx = self.msg_tx.clone();
        rt.block_on(async move {
            let permit = match tokio::time::timeout(within, tx.reserve()).await {
                Ok(Ok(p)) => p,
                _ => return Err(child),
            };
            permit.send(Msg::ReturnChild(slug, child));
            Ok(())
        })
    }

    /// Start `slug` and report the pid it runs as plus its exit, so a caller
    /// waiting for a boot can tell "still booting" from "died".
    pub async fn start_watched(&self, slug: impl Into<String>) -> Result<Watched, String> {
        let (tx, rx) = oneshot::channel();
        self.msg_tx
            .send(Msg::StartWatched(slug.into(), tx))
            .await
            .map_err(|_| "supervisor is not running".to_string())?;
        rx.await
            .map_err(|_| "supervisor dropped the start request".to_string())?
    }

    /// Shut down the supervisor actor (does not stop running agents — OS owns them).
    pub async fn shutdown(self) {
        let _ = self.msg_tx.send(Msg::Shutdown).await;
    }
}

// ─── Background actor ───────────────────────────────────────────────────────

async fn os_actor(
    mur_home: PathBuf,
    mut rx: tokio::sync::mpsc::Receiver<Msg>,
    status_tx: watch::Sender<Vec<AgentRuntimeStatus>>,
    poll: Duration,
) {
    // Agents this Hub spawned and supervises directly. The Hub owns each
    // child's lifetime, so there is exactly one runtime per agent.
    let mut children: HashMap<String, Child> = HashMap::new();
    // Runtimes already alive when this Hub session started — from the CLI, from
    // `mur agent install-service`, or from a previous Hub — belong in the
    // tracked set too. Without this seed the set only ever holds agents *this*
    // session started, so `emit_status`'s lock check (which exists precisely to
    // recognise foreign runtimes) is never asked about them and the Agents page
    // renders every one of them idle while they are plainly running.
    let mut known: HashSet<String> = adopt_live_runtimes(&mur_home);
    // Lent children that came back to a refilled slot: killed, awaiting reap.
    let mut reaping: Vec<(String, Child)> = Vec::new();
    // `StartWatched` callers waiting to hear their child exited.
    let mut watchers: HashMap<String, watch::Sender<Option<ExitStatus>>> = HashMap::new();
    let mut poll_interval = tokio::time::interval(poll);
    poll_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            Some(msg) = rx.recv() => {
                match msg {
                    Msg::Shutdown => {
                        for (slug, mut child) in children.drain() {
                            let _ = child.kill();
                            let status = child.wait().ok();
                            handover::notify_one(&mut watchers, &slug, status);
                            info!(agent = %slug, "stopped supervised runtime");
                        }
                        // `wait`, not `try_wait`: SIGKILL lands asynchronously,
                        // so a single `try_wait` right after it usually sees
                        // `None`, and the dropped `Child` is never reaped — a
                        // zombie for the life of the process. SIGKILL cannot be
                        // ignored, so this wait is bounded.
                        for (_, mut child) in reaping.drain(..) {
                            let _ = child.kill();
                            let _ = child.wait();
                        }
                        break;
                    }

                    Msg::Start(slug, reply) => {
                        handover::notify_exits(&mut children, &mut watchers);
                        known.insert(slug.clone());
                        let result = ensure_running(&slug, &mur_home, &mut children);
                        match &result {
                            Ok(()) => info!(agent = %slug, "runtime running"),
                            Err(e) => warn!(agent = %slug, "start failed: {e}"),
                        }
                        emit_status(&known, &mur_home, &mut children, &status_tx);
                        let _ = reply.send(result);
                    }

                    Msg::Stop(slug) => {
                        match children.remove(&slug) {
                            Some(mut child) => {
                                let _ = child.kill();
                                let status = child.wait().ok();
                                handover::notify_one(&mut watchers, &slug, status);
                            }
                            // No handle of our own — the runtime may still be
                            // alive from an earlier Hub session or the CLI.
                            // Stop it for real before clearing its files.
                            None => stop_unowned_runtime(&mur_home, &slug),
                        }
                        clear_runtime_state(&mur_home, &slug);
                        known.remove(&slug);
                        emit_status(&known, &mur_home, &mut children, &status_tx);
                    }

                    Msg::TakeChild(slug, reply) => {
                        let _ = reply.send(children.remove(&slug));
                    }

                    Msg::ReturnChild(slug, child) => {
                        handover::reinsert(&mut children, &mut reaping, slug, child);
                    }

                    Msg::StartWatched(slug, reply) => {
                        handover::notify_exits(&mut children, &mut watchers);
                        known.insert(slug.clone());
                        let result = ensure_running(&slug, &mur_home, &mut children).map(|()| {
                            let pid = children.get(&slug).map(Child::id);
                            let (tx, exit) = watch::channel(None);
                            if pid.is_some() {
                                watchers.insert(slug.clone(), tx);
                            }
                            Watched { pid, exit }
                        });
                        if let Err(e) = &result {
                            warn!(agent = %slug, "start failed: {e}");
                        }
                        emit_status(&known, &mur_home, &mut children, &status_tx);
                        let _ = reply.send(result);
                    }
                }
            }

            _ = poll_interval.tick() => {
                // Re-adopt on every tick, not just at startup: an agent can be
                // started from the CLI while the Hub is open, and it would
                // otherwise stay invisible until the next Hub restart.
                known.extend(adopt_live_runtimes(&mur_home));
                handover::drain_reaping(&mut reaping);
                handover::notify_exits(&mut children, &mut watchers);
                if !known.is_empty() {
                    emit_status(&known, &mur_home, &mut children, &status_tx);
                }
            }
        }
    }
}

/// The child inside a `ReturnChild` that could not be sent.
fn take_back(msg: Msg) -> Child {
    match msg {
        Msg::ReturnChild(_, child) => child,
        _ => unreachable!("only ReturnChild is sent through take_back"),
    }
}

fn emit_status(
    known: &HashSet<String>,
    mur_home: &Path,
    children: &mut HashMap<String, Child>,
    tx: &watch::Sender<Vec<AgentRuntimeStatus>>,
) {
    let mut snapshot: Vec<AgentRuntimeStatus> = known
        .iter()
        .map(|name| {
            let child_alive = children
                .get_mut(name)
                .map(|c| matches!(c.try_wait(), Ok(None)))
                .unwrap_or(false);
            let lock = live_lock(mur_home, name);
            let state = if child_alive || lock.is_some() {
                RuntimeState::Running { pid: 0 }
            } else {
                RuntimeState::Stopped
            };
            let relaxed = lock
                .and_then(|l| l.startup_override)
                .map(|o| o.flags.cli_names().into_iter().map(String::from).collect());
            AgentRuntimeStatus {
                name: name.clone(),
                state,
                relaxed,
            }
        })
        .collect();
    snapshot.sort_by(|a, b| a.name.cmp(&b.name));
    let _ = tx.send(snapshot);
}

/// Ensure a supervised runtime is up for `slug`, spawning one if needed.
/// Idempotent: a no-op if our child is still alive or a live runtime already
/// holds the lock (e.g. one started from the CLI).
fn ensure_running(
    slug: &str,
    mur_home: &Path,
    children: &mut HashMap<String, Child>,
) -> Result<(), String> {
    if let Some(child) = children.get_mut(slug) {
        match child.try_wait() {
            Ok(None) => return Ok(()), // still running
            _ => {
                children.remove(slug); // exited — fall through to respawn
            }
        }
    }
    if lock_is_live(mur_home, slug) {
        return Ok(());
    }
    let child = spawn_runtime(slug, mur_home)?;
    children.insert(slug.to_string(), child);
    Ok(())
}

/// Spawn the agent runtime as a direct child process, returning a
/// human-readable error so the Hub UI can surface a failure (C3).
fn spawn_runtime(slug: &str, mur_home: &Path) -> Result<Child, String> {
    let runtime_bin = find_runtime_binary().map_err(|e| {
        format!(
            "agent runtime not found ({e}). Reinstall MUR so mur-agent-runtime \
             is available, or run build.sh to install it."
        )
    })?;
    mur_common::binary_attestation::verify_runtime_signature(&runtime_bin)
        .map_err(|e| e.mount_failure_message())?;
    // Clear any stale lock/socket a previously crashed runtime left behind so
    // the new one binds cleanly.
    clear_runtime_state(mur_home, slug);
    let log = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(mur_home.join("agents").join(slug).join("stderr.log"))
        .map(Stdio::from)
        .unwrap_or_else(|_| Stdio::null());
    let mut cmd = Command::new(&runtime_bin);
    cmd.arg("--profile")
        .arg(slug)
        .env("MUR_HOME", mur_home)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(log);
    // Route subscription-OAuth (`sk-ant-oat*`) traffic through the local
    // cc-proxy bridge when it's enabled and listening, otherwise leave the
    // runtime on the direct api.anthropic.com path. Without this a GUI-spawned
    // runtime (which doesn't inherit the user's shell env) sends an oat token as
    // `x-api-key` and gets a 401. See `oauth_bridge`.
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    if let Some(base_url) = crate::oauth_bridge::resolve_bridge_url(
        &cfg.cc_proxy,
        std::env::var("ANTHROPIC_BASE_URL").ok().as_deref(),
        crate::oauth_bridge::bridge_is_listening,
    ) {
        info!(slug, %base_url, "routing agent runtime through cc-proxy bridge");
        cmd.env("ANTHROPIC_BASE_URL", base_url);
    }
    // STT language follows the device. The Hub process can read the OS locale,
    // but the sandboxed agent runtime cannot reach CFLocale — so pass it down as
    // MUR_STT_LANGUAGE. The runtime maps e.g. "zh-Hant-TW" → Whisper language
    // "zh" + Traditional output. An explicit env (user override) always wins.
    if std::env::var_os("MUR_STT_LANGUAGE").is_none()
        && let Some(locale) = sys_locale::get_locale()
    {
        info!(slug, %locale, "passing OS locale to agent runtime for STT");
        cmd.env("MUR_STT_LANGUAGE", locale);
    }
    cmd.spawn()
        .map_err(|e| format!("could not start the agent runtime: {e}"))
}

/// Whether a live process currently owns the agent's `running.lock`.
fn lock_is_live(mur_home: &Path, slug: &str) -> bool {
    live_lock(mur_home, slug).is_some()
}

/// The pid of the live process owning the agent's `running.lock`, if any.
fn lock_owner_pid(mur_home: &Path, slug: &str) -> Option<u32> {
    live_lock(mur_home, slug).map(|l| l.pid)
}

/// The agent's `running.lock` when its owner is alive; one read serves both
/// liveness and the override record.
fn live_lock(mur_home: &Path, slug: &str) -> Option<mur_common::LockFile> {
    let lock = mur_home.join("agents").join(slug).join("running.lock");
    match mur_common::lock_file::read(&lock) {
        Ok(Some(l)) if mur_common::lock_file::pid_alive(l.pid) => Some(l),
        _ => None,
    }
}

/// Every agent holding a live `running.lock` right now, including runtimes this
/// Hub never spawned. Cheap enough for the 5-second poll: one `read_dir` plus a
/// small JSON read and a `kill(0)` per agent directory.
fn adopt_live_runtimes(mur_home: &Path) -> HashSet<String> {
    let Ok(entries) = std::fs::read_dir(mur_home.join("agents")) else {
        return HashSet::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|slug| lock_is_live(mur_home, slug))
        .collect()
}

/// Stop whatever process owns `slug`'s lock, when it is not one of our own
/// tracked children (a runtime started by an earlier Hub session, or by the
/// CLI). SIGTERM, then SIGKILL if it outlives the grace period.
///
/// Without this, `Msg::Stop` for an untracked runtime went straight to
/// `clear_runtime_state` and only deleted the lock files out from under a
/// process that kept running — see the duplicate-supervisor path documented on
/// `clear_runtime_state`.
///
/// A runtime still booting has no `running.lock` yet; its target then comes
/// from the flocked sentinel (`boot_window::sentinel_target`).
fn stop_unowned_runtime(mur_home: &Path, slug: &str) {
    let Some(pid) =
        lock_owner_pid(mur_home, slug).or_else(|| boot_window::sentinel_target(mur_home, slug))
    else {
        return;
    };
    warn!(agent = %slug, pid, "stopping untracked runtime holding the lock");
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGTERM);
    }
    let deadline = std::time::Instant::now() + STOP_GRACE;
    while std::time::Instant::now() < deadline {
        if !mur_common::lock_file::pid_alive(pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, libc::SIGKILL);
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
}

/// How long an untracked runtime gets to exit on SIGTERM before SIGKILL.
const STOP_GRACE: std::time::Duration = std::time::Duration::from_secs(10);

/// Remove an agent's lock/socket/sentinel files (after stopping or before a
/// fresh spawn) — **only when no live process owns them**.
///
/// The owner check is load-bearing, not defensive. `running.sentinel` carries
/// the flock that makes supervisor startup mutually exclusive, and a flock
/// lives on an inode, not a path: deleting the sentinel while its holder is
/// still alive leaves that holder locking a file nobody can see, so the next
/// supervisor acquires the "lock" and both run against one agent home. That is
/// exactly how two `mur-agent-runtime --profile mur` processes ended up sharing
/// `agent.sock` (issue #790) — a Stop for an agent the Hub had lost the child
/// handle for cleared the files without stopping anything.
///
/// Deleting `agent.sock` has the same shape: the path may already belong to a
/// newer supervisor's listener.
///
/// The JSON pid alone is not enough: a booting supervisor takes the sentinel
/// flock before it writes `running.lock` (it seals the startup override in
/// between), so for that whole window the JSON is missing or names the previous
/// owner. `probe_sentinel` covers it.
///
/// `running.sentinel` itself is never removed — not here, not by the runtime's
/// own release (`mur-agent-runtime/src/lock_file.rs`). Its existence means
/// nothing; only the flock on it does, and every unlink is a chance for a
/// holder to end up locking an inode the next opener can't see. A probe that
/// released the flock and *then* unlinked had exactly that gap: a runtime
/// booting in between flocked the old inode, lost it to the unlink, and a
/// second runtime got a fresh, uncontested one. The probe flock is instead held
/// until the other files are gone, so a runtime that races the clear fails its
/// own `try_lock_exclusive` and exits as a duplicate would.
fn clear_runtime_state(mur_home: &Path, slug: &str) {
    if let Some(pid) = lock_owner_pid(mur_home, slug) {
        warn!(
            agent = %slug,
            pid,
            "refusing to clear runtime state: a live process still owns the lock"
        );
        return;
    }
    let dir = mur_home.join("agents").join(slug);
    let _held = match probe_sentinel(&dir) {
        SentinelProbe::NoAgentDir => return,
        SentinelProbe::Owned => {
            warn!(
                agent = %slug,
                "refusing to clear runtime state: running.sentinel is flocked or names a live pid"
            );
            return;
        }
        SentinelProbe::Free(held) => held,
    };
    for f in ["running.lock", "agent.sock"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
}

/// A non-empty regular file. The `binaries/` directory ships zero-byte
/// `externalBin` placeholders in git that are only filled in at release time;
/// spawning one fails, so a candidate must have real bytes to count.
fn is_real_binary(p: &Path) -> bool {
    std::fs::metadata(p)
        .map(|m| m.is_file() && m.len() > 0)
        .unwrap_or(false)
}

/// Locate the `mur-agent-runtime` binary across the locations it can live in:
/// an explicit override, alongside the Hub binary (dev builds), inside the
/// macOS `.app` bundle's Resources, or on `PATH` (installed next to `mur`).
/// Zero-byte placeholders are skipped so we never spawn a broken stub (C2).
fn find_runtime_binary() -> anyhow::Result<PathBuf> {
    #[cfg(target_os = "windows")]
    let name = "mur-agent-runtime.exe";
    #[cfg(not(target_os = "windows"))]
    let name = "mur-agent-runtime";

    // Explicit override wins, matching mur-core's resolver.
    if let Some(v) = std::env::var_os("MUR_AGENT_RUNTIME_BIN") {
        let p = PathBuf::from(v);
        if is_real_binary(&p) {
            return Ok(p);
        }
    }

    let mut candidates: Vec<PathBuf> = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        candidates.push(dir.join(name)); // dev build / simple install
        // macOS .app: Contents/MacOS/<exe> → Contents/Resources/binaries/<name>
        candidates.push(dir.join("../Resources/binaries").join(name));
        candidates.push(dir.join("../Resources").join(name));
    }
    // Anywhere on PATH (e.g. installed next to the `mur` CLI).
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            candidates.push(dir.join(name));
        }
    }

    for c in &candidates {
        if is_real_binary(c) {
            return Ok(c.clone());
        }
    }
    anyhow::bail!(
        "mur-agent-runtime not found (looked alongside the Hub, in the app bundle, and on PATH)"
    )
}

// ─── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
