//! Proxy placement (plan Task 2.3, spec "Proxy process placement", O7
//! option A): the supervisor connects to the upstream ssh-agent **before**
//! the seal and holds that one stream; after the seal it listens in the
//! agent's own home and serves only this agent and its descendants.
//!
//! No reconnect after the seal (the sandbox would refuse it on a boundary
//! platform, and a fallback that sometimes works is worse than none). No
//! `/tmp` fallback for a long socket path: the proxy simply does not start.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;
use tokio::net::{UnixListener, UnixStream};

use super::policy::Policy;
use super::refusal::{Refusal, RefusalCode, RefusalSink};
use super::session::{Session, Upstream};
use super::wire;

/// File name of the proxy socket inside the agent's home. Under
/// `<mur_home>/agents`, which the agent's sandbox already lets it dial, so
/// the allowlist does not widen (#1873).
pub const SSH_PROXY_SOCKET_NAME: &str = "ssh-agent.sock";

/// Bound on the pre-seal connect to the upstream agent.
pub const UPSTREAM_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);

/// Bound on one request/answer exchange with the upstream agent. Past it the
/// held stream is dropped: a half-read answer cannot be resynced.
pub const UPSTREAM_IO_TIMEOUT: Duration = Duration::from_secs(10);

/// Pause after a failed `accept` (e.g. `EMFILE`) so the loop cannot spin.
pub const ACCEPT_ERROR_BACKOFF: Duration = Duration::from_millis(250);

/// Socket file mode: user-only.
const SOCKET_MODE: u32 = 0o600;

/// Where the proxy listens for the agent living at `agent_home`.
pub fn socket_path(agent_home: &Path) -> PathBuf {
    agent_home.join(SSH_PROXY_SOCKET_NAME)
}

/// Whether `path` fits `sockaddr_un.sun_path` with its terminating NUL.
pub fn socket_path_fits(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    // SAFETY: an all-zero sockaddr_un is a valid value; only its size is read.
    let cap = unsafe { std::mem::zeroed::<libc::sockaddr_un>() }
        .sun_path
        .len();
    path.as_os_str().as_bytes().len() < cap
}

/// Bind the listener: replace a stale socket from an earlier run, then make
/// the file user-only. tokio creates the socket close-on-exec.
pub fn bind(path: &Path) -> std::io::Result<UnixListener> {
    use std::os::unix::fs::{FileTypeExt, PermissionsExt};
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists and is not a socket", path.display()),
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let l = UnixListener::bind(path)?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(SOCKET_MODE))?;
    Ok(l)
}

/// The one upstream connection, opened before the seal and shared by every
/// client connection behind a mutex. `None` once it was never made or has
/// dropped; there is no reconnect.
pub struct HeldUpstream {
    stream: tokio::sync::Mutex<Option<UnixStream>>,
}

impl HeldUpstream {
    /// No upstream: every request that needs one is refused
    /// `upstream_unavailable`.
    pub fn disconnected() -> Self {
        Self {
            stream: tokio::sync::Mutex::new(None),
        }
    }

    /// Connect now. Must run before the seal.
    pub async fn connect(path: &Path) -> std::io::Result<Self> {
        let s = tokio::time::timeout(UPSTREAM_CONNECT_TIMEOUT, UnixStream::connect(path))
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out connecting to the upstream ssh-agent",
                )
            })??;
        Ok(Self::from_stream(s))
    }

    pub(crate) fn from_stream(s: UnixStream) -> Self {
        Self {
            stream: tokio::sync::Mutex::new(Some(s)),
        }
    }

    pub async fn is_connected(&self) -> bool {
        self.stream.lock().await.is_some()
    }

    #[cfg(test)]
    pub(crate) async fn raw_fd(&self) -> Option<std::os::fd::RawFd> {
        use std::os::fd::AsRawFd;
        self.stream.lock().await.as_ref().map(AsRawFd::as_raw_fd)
    }
}

impl Upstream for HeldUpstream {
    async fn roundtrip(&self, frame: Vec<u8>) -> std::io::Result<Vec<u8>> {
        let mut held = self.stream.lock().await;
        let Some(s) = held.as_mut() else {
            return Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "no upstream connection is held (never made, or dropped; \
                 MUR does not reconnect after the seal)",
            ));
        };
        let r = match tokio::time::timeout(UPSTREAM_IO_TIMEOUT, exchange(s, &frame)).await {
            Ok(r) => r,
            Err(_) => Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "upstream ssh-agent did not answer in time",
            )),
        };
        if r.is_err() {
            // The stream may be mid-frame; it can never be trusted again.
            *held = None;
        }
        r
    }
}

async fn exchange(s: &mut UnixStream, frame: &[u8]) -> std::io::Result<Vec<u8>> {
    s.write_all(frame).await?;
    match wire::read_frame(s).await? {
        Some(Ok(body)) => Ok(body),
        Some(Err(e)) => Err(std::io::Error::other(format!("upstream answer: {e}"))),
        None => Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "upstream ssh-agent closed the connection",
        )),
    }
}

/// Decides whether a connecting peer may use the proxy. `Err` says why not.
pub type PeerGate = Arc<dyn Fn(Option<u32>) -> Result<(), String> + Send + Sync>;

/// Admit only this agent and processes it spawned: the lineage walk the A2A
/// gate uses, but nothing else counts — not the user, not a peer agent.
pub fn self_only_gate(self_pid: u32, agents_dir: PathBuf) -> PeerGate {
    use crate::communication_policy::{Caller, identify, running_agents};
    Arc::new(move |pid| {
        match identify(
            pid,
            self_pid,
            &running_agents(&agents_dir),
            crate::hitl::shim_ticket::parent_of,
        ) {
            Caller::SelfAgent => Ok(()),
            Caller::Agent(name) => Err(format!("peer belongs to agent '{name}'")),
            Caller::User => Err("peer is not descended from this agent".into()),
            Caller::Unknown => Err("peer process could not be identified".into()),
        }
    })
}

/// Everything one listener serves with. Built once, after the seal.
pub struct Proxy {
    pub policy: Policy,
    pub upstream: Arc<HeldUpstream>,
    pub sink: RefusalSink,
    pub gate: PeerGate,
}

impl Proxy {
    /// One client connection. The peer check runs before any byte is read
    /// from the client or written upstream; a refused peer is closed with
    /// nothing forwarded.
    pub async fn handle(&self, stream: UnixStream) {
        let pid = crate::transport::unix_socket::peer_pid(&stream);
        if let Err(why) = (self.gate)(pid) {
            self.sink.record(
                &Refusal::new(RefusalCode::PeerNotSelf)
                    .detail(format!("peer pid {}: {why}", pid.unwrap_or(0))),
            );
            return;
        }
        let session = Session::new(&self.policy, &*self.upstream, &self.sink);
        if let Err(e) = session.serve(stream).await {
            tracing::debug!(error = %e, "ssh-agent proxy client connection ended");
        }
    }
}

/// Accept forever. Each connection gets its own task.
pub async fn serve(listener: UnixListener, proxy: Arc<Proxy>) {
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                let proxy = proxy.clone();
                tokio::spawn(async move { proxy.handle(stream).await });
            }
            Err(e) => {
                tracing::warn!(error = %e, "ssh-agent proxy accept failed");
                tokio::time::sleep(ACCEPT_ERROR_BACKOFF).await;
            }
        }
    }
}

/// Fingerprints listed in `policy` that the upstream does not hold, sorted.
/// `None` when the upstream could not be asked (warned about separately).
pub async fn missing_upstream_keys(
    policy: &Policy,
    upstream: &HeldUpstream,
) -> Option<Vec<String>> {
    let listed = policy.key_fps();
    if listed.is_empty() {
        return Some(Vec::new());
    }
    let body = upstream
        .roundtrip(wire::Request::RequestIdentities.encode())
        .await
        .ok()?;
    let wire::Answer::Identities(ids) = wire::decode_answer(&body).ok()? else {
        return None;
    };
    let held: std::collections::HashSet<String> = ids
        .iter()
        .map(|i| super::keys::fingerprint(&i.key_blob))
        .collect();
    let mut out: Vec<String> = listed
        .into_iter()
        .filter(|fp| !held.contains(*fp))
        .map(str::to_string)
        .collect();
    out.sort();
    Some(out)
}
