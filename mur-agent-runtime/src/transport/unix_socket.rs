//! Unix domain socket transport — JSON-RPC 2.0 newline-delimited,
//! with SO_PEERCRED caller resolution (Task 22 consumes this).

use crate::protocol::a2a_server::Dispatcher;
use mur_common::JsonRpcRequest;
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::mpsc;

#[derive(Debug, Clone, Copy)]
pub struct PeerInfo {
    pub pid: u32,
    pub uid: u32,
}

pub async fn serve_unix(
    dispatcher: Arc<Dispatcher>,
    path: PathBuf,
    mut notifications: mpsc::Receiver<Value>,
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
    let (bcast_tx, _) = tokio::sync::broadcast::channel::<Value>(256);
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
        let mut bcast_rx = bcast_tx.subscribe();
        tokio::spawn(async move {
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
            let mut reader = BufReader::new(read);
            let mut line = String::new();
            loop {
                line.clear();
                let n = reader.read_line(&mut line).await.unwrap_or(0);
                if n == 0 {
                    break;
                }
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let req: JsonRpcRequest = match serde_json::from_str(trimmed) {
                    Ok(r) => r,
                    Err(_) => continue,
                };
                let resp = match dispatcher.dispatch(req).await {
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
            let _ = peer; // passed to auth / communication_policy via request context in Task 22
            notif_task.abort();
        });
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
    if rc == 0 {
        Some(PeerInfo {
            pid: 0,
            uid: cred.cr_uid,
        })
    } else {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn peer_info(_stream: &tokio::net::UnixStream) -> Option<PeerInfo> {
    None
}
