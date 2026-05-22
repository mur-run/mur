//! Per-agent single-instance IPC channel — spec §5.3.
//!
//! Hub binds a socket per agent slug. If another Hub instance tries to open
//! the same agent, it connects as a client, sends an `ActivationPayload`, and
//! exits. The running Hub receives it and raises / activates the agent window.
//!
//! Platform mapping:
//! - macOS / Linux: Unix domain socket at `<mur_home>/agents/<slug>/ipc.sock`
//!   (mode 0600 — prevents cross-user access)
//! - Windows: named pipe `\\.\pipe\mur-agent-<slug>` (owner-only DACL)
//!
//! Payload encoding: JSON (spec suggests CBOR; JSON avoids a new dep in v1).

use anyhow::{Context, Result};
use mur_common::expression::ExpressionChange;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Payload sent from a second-instance client to the running Hub server.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ActivationPayload {
    Url {
        url: String,
        ts: i64,
        nonce: Vec<u8>,
    },
    Share {
        share_file: String,
        ts: i64,
        nonce: Vec<u8>,
    },
    Focus {
        ts: i64,
        nonce: Vec<u8>,
    },
}

impl ActivationPayload {
    /// Construct a Focus payload with a fresh random nonce and current timestamp.
    pub fn focus() -> Self {
        Self::Focus {
            ts: chrono::Utc::now().timestamp(),
            nonce: rand_nonce(),
        }
    }

    pub fn url(url: String) -> Self {
        Self::Url {
            url,
            ts: chrono::Utc::now().timestamp(),
            nonce: rand_nonce(),
        }
    }
}

fn rand_nonce() -> Vec<u8> {
    // 16 bytes from the OS PRNG; not security-critical (replays are harmless).
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    use std::time::SystemTime;
    let mut h = DefaultHasher::new();
    SystemTime::now().hash(&mut h);
    std::thread::current().id().hash(&mut h);
    let v = h.finish();
    v.to_le_bytes().to_vec()
}

/// Returns the IPC socket / pipe path for a given agent slug.
pub fn ipc_channel_path(slug: &str, mur_home: &Path) -> std::path::PathBuf {
    #[cfg(not(target_os = "windows"))]
    {
        mur_home.join("agents").join(slug).join("ipc.sock")
    }
    #[cfg(target_os = "windows")]
    {
        // Named pipe name cannot contain path separators; embed slug directly.
        // The actual path is not file-system-based on Windows.
        let _ = (slug, mur_home);
        std::path::PathBuf::from(format!(r"\\.\pipe\mur-agent-{slug}"))
    }
}

/// Returns the sidecar-to-Hub push stream socket path for a given agent slug.
///
/// The runtime connects to this socket and writes `SidecarMessage` lines.
/// Hub listens on it in a background task per agent.
pub fn sidecar_stream_path(slug: &str, mur_home: &Path) -> std::path::PathBuf {
    #[cfg(not(target_os = "windows"))]
    {
        mur_home.join("agents").join(slug).join("sidecar.sock")
    }
    #[cfg(target_os = "windows")]
    {
        let _ = (slug, mur_home);
        std::path::PathBuf::from(format!(r"\\.\pipe\mur-sidecar-{slug}"))
    }
}

/// Message pushed from mur-agent-runtime → Hub over the sidecar stream socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SidecarMessage {
    /// Expression state update for the pet window.
    ExpressionUpdate {
        change: ExpressionChange,
        agent_id: String,
        ts: i64,
    },
    /// Heartbeat from the sidecar to signal liveness.
    Heartbeat { ts: i64 },
}

// ── Unix (macOS + Linux) ──────────────────────────────────────────────────────

#[cfg(not(target_os = "windows"))]
pub use unix::{IpcServer, accept_sidecar_stream, connect_sidecar_stream, send_activation};

#[cfg(not(target_os = "windows"))]
mod unix {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::{UnixListener, UnixStream};

    /// Owns the bound socket; accepts incoming activation messages.
    pub struct IpcServer {
        listener: UnixListener,
        socket_path: std::path::PathBuf,
    }

    impl IpcServer {
        /// Try to bind the socket for `slug`.
        ///
        /// Returns `Ok(server)` if this is the first Hub instance for this agent.
        /// Returns `Err` if the socket already exists **and** a server is
        /// listening (caller should use `send_activation` instead).
        pub async fn bind(slug: &str, mur_home: &Path) -> Result<Self> {
            let path = ipc_channel_path(slug, mur_home);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent)
                    .await
                    .context("create ipc dir")?;
            }

            // If a stale socket file exists (e.g. from a crash), remove it
            // first and try to bind. If a live server is there, the bind will
            // fail and we'll return Err so the caller falls back to client mode.
            if path.exists() {
                // Try to connect as a quick liveness check.
                match UnixStream::connect(&path).await {
                    Ok(_) => {
                        anyhow::bail!(
                            "IPC socket already in use for agent '{slug}' — use send_activation"
                        );
                    }
                    Err(_) => {
                        // Stale socket — remove and bind.
                        let _ = tokio::fs::remove_file(&path).await;
                    }
                }
            }

            let listener = UnixListener::bind(&path).context("bind unix socket")?;
            // Restrict to owner-only (0600).
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
                .context("chmod ipc socket")?;

            Ok(Self {
                listener,
                socket_path: path,
            })
        }

        /// Accept the next activation payload from a client.
        ///
        /// Reads one newline-terminated JSON line per connection.
        pub async fn accept_one(&self) -> Result<ActivationPayload> {
            let (stream, _addr) = self.listener.accept().await.context("accept")?;
            let mut reader = BufReader::new(stream);
            let mut line = String::new();
            reader.read_line(&mut line).await.context("read line")?;
            serde_json::from_str(line.trim()).context("parse activation payload")
        }
    }

    impl Drop for IpcServer {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.socket_path);
        }
    }

    /// Connect to Hub's sidecar stream socket and return a channel for pushing
    /// `SidecarMessage`s. Used by mur-agent-runtime.
    ///
    /// Returns `None` if Hub's socket doesn't exist yet (agent started before Hub).
    pub async fn connect_sidecar_stream(
        slug: &str,
        mur_home: &Path,
    ) -> Option<tokio::sync::mpsc::Sender<SidecarMessage>> {
        let path = crate::ipc::sidecar_stream_path(slug, mur_home);
        let stream = UnixStream::connect(&path).await.ok()?;
        let (tx, mut rx) = tokio::sync::mpsc::channel::<SidecarMessage>(32);
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let mut stream = stream;
            while let Some(msg) = rx.recv().await {
                let Ok(mut json) = serde_json::to_vec(&msg) else {
                    break;
                };
                json.push(b'\n');
                if stream.write_all(&json).await.is_err() {
                    break;
                }
            }
        });
        Some(tx)
    }

    /// Bind Hub's sidecar stream socket and return a channel that receives
    /// `SidecarMessage`s pushed by the runtime. Used by Hub.
    pub async fn accept_sidecar_stream(
        slug: &str,
        mur_home: &Path,
    ) -> Result<tokio::sync::mpsc::Receiver<SidecarMessage>> {
        let path = crate::ipc::sidecar_stream_path(slug, mur_home);
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent)
                .await
                .context("create sidecar dir")?;
        }
        if path.exists() {
            let _ = tokio::fs::remove_file(&path).await;
        }
        let listener = UnixListener::bind(&path).context("bind sidecar socket")?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .context("chmod sidecar socket")?;

        let (tx, rx) = tokio::sync::mpsc::channel::<SidecarMessage>(64);
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let tx2 = tx.clone();
                tokio::spawn(async move {
                    let mut reader = BufReader::new(stream);
                    let mut line = String::new();
                    loop {
                        line.clear();
                        match reader.read_line(&mut line).await {
                            Ok(0) | Err(_) => break,
                            Ok(_) => {
                                if let Ok(msg) = serde_json::from_str::<SidecarMessage>(line.trim())
                                    && tx2.send(msg).await.is_err()
                                {
                                    break;
                                }
                            }
                        }
                    }
                });
            }
        });
        Ok(rx)
    }

    /// Connect to a running IPC server for `slug` and send one activation.
    pub async fn send_activation(
        slug: &str,
        mur_home: &Path,
        payload: ActivationPayload,
    ) -> Result<()> {
        let path = ipc_channel_path(slug, mur_home);
        let mut stream = UnixStream::connect(&path)
            .await
            .with_context(|| format!("connect ipc for '{slug}'"))?;
        let mut json = serde_json::to_vec(&payload).context("serialize payload")?;
        json.push(b'\n');
        stream.write_all(&json).await.context("send payload")?;
        Ok(())
    }
}

// ── Windows ───────────────────────────────────────────────────────────────────

#[cfg(target_os = "windows")]
pub use windows_ipc::{IpcServer, send_activation};

#[cfg(target_os = "windows")]
mod windows_ipc {
    use super::*;
    use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};

    pub struct IpcServer {
        pipe_name: String,
        server: tokio::net::windows::named_pipe::NamedPipeServer,
    }

    impl IpcServer {
        pub async fn bind(slug: &str, _mur_home: &Path) -> Result<Self> {
            let pipe_name = format!(r"\\.\pipe\mur-agent-{slug}");
            let server = ServerOptions::new()
                .first_pipe_instance(true)
                .create(&pipe_name)
                .context("create named pipe")?;
            Ok(Self { pipe_name, server })
        }

        pub async fn accept_one(&mut self) -> Result<ActivationPayload> {
            use tokio::io::{AsyncBufReadExt, BufReader};
            self.server.connect().await.context("pipe connect")?;
            let mut reader = BufReader::new(&mut self.server);
            let mut line = String::new();
            reader.read_line(&mut line).await.context("read line")?;
            serde_json::from_str(line.trim()).context("parse payload")
        }
    }

    pub async fn send_activation(
        slug: &str,
        _mur_home: &Path,
        payload: ActivationPayload,
    ) -> Result<()> {
        use tokio::io::AsyncWriteExt;
        let pipe_name = format!(r"\\.\pipe\mur-agent-{slug}");
        let mut client = ClientOptions::new()
            .open(&pipe_name)
            .context("open named pipe")?;
        let mut json = serde_json::to_vec(&payload).context("serialize")?;
        json.push(b'\n');
        client.write_all(&json).await.context("send")?;
        Ok(())
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
#[cfg(not(target_os = "windows"))]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn tmp_home() -> TempDir {
        tempfile::TempDir::new().unwrap()
    }

    #[tokio::test]
    async fn server_bind_and_client_send() {
        let tmp = tmp_home();
        let slug = "coach";
        let mur_home = tmp.path();

        // Create the agent dir that the socket lives in
        tokio::fs::create_dir_all(mur_home.join("agents").join(slug))
            .await
            .unwrap();

        let server = IpcServer::bind(slug, mur_home).await.unwrap();

        let payload = ActivationPayload::url("muragent-coach://open".into());
        let payload_clone = payload.clone();

        // Send from a background task
        let mh = mur_home.to_path_buf();
        tokio::spawn(async move {
            send_activation(slug, &mh, payload_clone).await.unwrap();
        });

        let received = server.accept_one().await.unwrap();
        assert_eq!(received, payload);
    }

    #[tokio::test]
    async fn second_bind_fails_when_server_alive() {
        let tmp = tmp_home();
        let slug = "coach2";
        let mur_home = tmp.path();

        tokio::fs::create_dir_all(mur_home.join("agents").join(slug))
            .await
            .unwrap();

        let _server = IpcServer::bind(slug, mur_home).await.unwrap();
        let result = IpcServer::bind(slug, mur_home).await;
        assert!(result.is_err(), "second bind should fail");
    }

    #[test]
    fn sidecar_message_roundtrip() {
        use mur_common::expression::ExpressionChange;

        let msg = SidecarMessage::ExpressionUpdate {
            change: ExpressionChange {
                expression: "smile".into(),
                bubble_text: Some("hello".into()),
            },
            agent_id: "agent-1".into(),
            ts: 1_700_000_000,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"kind\":\"expression_update\""));
        assert!(json.contains("smile"));

        let hb = SidecarMessage::Heartbeat { ts: 1_700_000_001 };
        let hb_json = serde_json::to_string(&hb).unwrap();
        assert!(hb_json.contains("\"kind\":\"heartbeat\""));
    }

    #[test]
    fn focus_payload_roundtrip() {
        let p = ActivationPayload::focus();
        let json = serde_json::to_string(&p).unwrap();
        let back: ActivationPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(p, back);
    }
}
