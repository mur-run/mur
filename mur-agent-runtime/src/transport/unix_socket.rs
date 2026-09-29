//! Unix domain socket transport — JSON-RPC 2.0 newline-delimited,
//! with SO_PEERCRED caller resolution (Task 22 consumes this).

use crate::communication_policy::AcceptPolicy;
use crate::protocol::a2a_server::{Dispatcher, HandlerError, RequestContext};
use futures::StreamExt;
use mur_common::{JsonRpcError, JsonRpcRequest, JsonRpcResponse};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::net::UnixListener;
use tokio::sync::mpsc;
use tokio_util::codec::{FramedRead, LinesCodec};

/// Maximum bytes accepted for a single JSON-RPC line. `LinesCodec` enforces
/// this at read time — the internal buffer never exceeds this limit, giving
/// true allocation-bounded reads. Aligned with `transport::noise::MAX_FRAME_BYTES`.
const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Capacity of the notification channels (telemetry broadcast and the
/// per-connection request-scoped sink). Bounded so a slow client can't grow an
/// unbounded backlog; overflow is lossy by design (a dropped token delta is
/// preferable to blocking generation).
const NOTIFY_CHANNEL_CAP: usize = 256;

#[derive(Debug, Clone, Copy)]
pub struct PeerInfo {
    pub pid: u32,
    pub uid: u32,
}

pub async fn serve_unix(
    dispatcher: Arc<Dispatcher>,
    path: PathBuf,
    notifications: mpsc::Receiver<Value>,
) -> std::io::Result<()> {
    serve_unix_gated(dispatcher, path, notifications, None).await
}

/// `serve_unix` with the profile's `accepts_from` enforced per connection.
/// A refused connection stays open and answers every request with
/// `CommunicationDenied` (-32011), so the caller sees why instead of a hang
/// or a bare EOF; it never reaches the dispatcher or the notification stream.
pub async fn serve_unix_gated(
    dispatcher: Arc<Dispatcher>,
    path: PathBuf,
    mut notifications: mpsc::Receiver<Value>,
    policy: Option<Arc<AcceptPolicy>>,
) -> std::io::Result<()> {
    if path.exists() {
        let _ = std::fs::remove_file(&path);
    }
    let listener = UnixListener::bind(&path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = std::fs::metadata(&path)?.permissions();
        perms.set_mode(0o600);
        std::fs::set_permissions(&path, perms)?;
    }
    // Broadcast carries only request-INDEPENDENT notifications (telemetry,
    // skill-executed, progress) to every connection. Request-SCOPED streaming
    // (token deltas + HITL prompts) is NOT broadcast — it is routed to the
    // issuing connection via that connection's own sink (see below), so one
    // client never receives another client's tokens.
    let (bcast_tx, _) = tokio::sync::broadcast::channel::<Value>(NOTIFY_CHANNEL_CAP);
    let bcast_forward = bcast_tx.clone();
    tokio::spawn(async move {
        while let Some(n) = notifications.recv().await {
            let _ = bcast_forward.send(n);
        }
    });
    loop {
        let (stream, _) = listener.accept().await?;
        let peer = peer_info(&stream);
        let dispatcher = dispatcher.clone();
        let policy = policy.clone();
        // Subscribe at accept time, as before the gate existed, so no
        // notification slips between accept and the check; a refused
        // connection drops its receiver unread.
        let mut bcast_rx = bcast_tx.subscribe();
        tokio::spawn(async move {
            let denied = match policy.as_deref().map(|p| p.check(peer.map(|p| p.pid))) {
                Some(Err(reason)) => {
                    tracing::warn!(peer_pid = ?peer.map(|p| p.pid), %reason, "unix_socket: accepts_from refused connection");
                    Some(reason)
                }
                _ => None,
            };
            if let Some(reason) = denied {
                refuse(stream, &reason).await;
                return;
            }
            let (read, write) = stream.into_split();
            let write = std::sync::Arc::new(tokio::sync::Mutex::new(write));
            let w_notif = write.clone();
            let notif_task = tokio::spawn(async move {
                while let Ok(n) = bcast_rx.recv().await {
                    let line = format!("{n}\n");
                    let mut w = w_notif.lock().await;
                    if w.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = w.flush().await;
                }
            });

            // Per-connection sink for request-scoped notifications. Handlers
            // receive this via `RequestContext` and stream `message/delta` /
            // `tool/approval_needed` here — delivered ONLY to this socket.
            let (conn_notif_tx, mut conn_notif_rx) =
                tokio::sync::mpsc::channel::<Value>(NOTIFY_CHANNEL_CAP);
            let w_req = write.clone();
            let req_notif_task = tokio::spawn(async move {
                while let Some(n) = conn_notif_rx.recv().await {
                    let line = format!("{n}\n");
                    let mut w = w_req.lock().await;
                    if w.write_all(line.as_bytes()).await.is_err() {
                        break;
                    }
                    let _ = w.flush().await;
                }
            });
            let ctx = RequestContext::with_notifier(conn_notif_tx).with_conn(
                crate::hitl::shim_ticket::Connection::new(peer.map(|p| p.pid)),
            );

            let codec = LinesCodec::new_with_max_length(MAX_LINE_BYTES);
            let mut framed = FramedRead::new(read, codec);
            while let Some(result) = framed.next().await {
                let line = match result {
                    Ok(l) => l,
                    Err(e) => {
                        tracing::warn!(error = %e, "unix_socket: codec error — discarding frame");
                        continue;
                    }
                };
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let resp = match dispatcher.dispatch(req, &ctx).await {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let out = match serde_json::to_string(&resp) {
                    Ok(s) => format!("{s}\n"),
                    Err(_) => continue,
                };
                let mut w = write.lock().await;
                if w.write_all(out.as_bytes()).await.is_err() {
                    break;
                }
                let _ = w.flush().await;
            }
            notif_task.abort();
            req_notif_task.abort();
        });
    }
}

/// Answer every request on a refused connection with -32011 until the peer
/// hangs up. Notifications (no `id`) get no reply, as JSON-RPC requires.
async fn refuse(stream: tokio::net::UnixStream, reason: &str) {
    let (read, mut write) = stream.into_split();
    let mut framed = FramedRead::new(read, LinesCodec::new_with_max_length(MAX_LINE_BYTES));
    let code = HandlerError::CommunicationDenied(String::new()).code();
    while let Some(Ok(line)) = framed.next().await {
        let Ok(req) = serde_json::from_str::<JsonRpcRequest>(line.trim()) else {
            continue;
        };
        let Some(id) = req.id else { continue };
        let resp = JsonRpcResponse {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message: reason.to_string(),
                data: None,
            }),
        };
        let Ok(out) = serde_json::to_string(&resp) else {
            continue;
        };
        if write
            .write_all(format!("{out}\n").as_bytes())
            .await
            .is_err()
        {
            break;
        }
        let _ = write.flush().await;
    }
}

#[cfg(target_os = "linux")]
fn peer_info(stream: &tokio::net::UnixStream) -> Option<PeerInfo> {
    use std::mem;
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut cred: libc::ucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::ucred>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut _ as *mut _,
            &mut len,
        )
    };
    if rc == 0 {
        Some(PeerInfo {
            pid: cred.pid as u32,
            uid: cred.uid,
        })
    } else {
        None
    }
}

#[cfg(target_os = "macos")]
fn peer_info(stream: &tokio::net::UnixStream) -> Option<PeerInfo> {
    use std::mem;
    use std::os::unix::io::AsRawFd;
    let fd = stream.as_raw_fd();
    let mut cred: libc::xucred = unsafe { mem::zeroed() };
    let mut len = mem::size_of::<libc::xucred>() as libc::socklen_t;
    const LOCAL_PEERCRED: libc::c_int = 0x001;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            0, /* SOL_LOCAL */
            LOCAL_PEERCRED,
            &mut cred as *mut _ as *mut _,
            &mut len,
        )
    };
    if rc != 0 {
        return None;
    }
    // The pid is a separate option on macOS. 0 when unavailable, which every
    // consumer reads as "unknown" — never as a real process.
    let mut pid: libc::pid_t = 0;
    let mut plen = mem::size_of::<libc::pid_t>() as libc::socklen_t;
    let prc = unsafe {
        libc::getsockopt(
            fd,
            libc::SOL_LOCAL,
            libc::LOCAL_PEERPID,
            &mut pid as *mut _ as *mut _,
            &mut plen,
        )
    };
    Some(PeerInfo {
        pid: if prc == 0 { pid as u32 } else { 0 },
        uid: cred.cr_uid,
    })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_info(_stream: &tokio::net::UnixStream) -> Option<PeerInfo> {
    None
}
