//! TCP transport with Noise XK handshake + length-prefixed JSON-RPC frames.
//!
//! Server (responder) accepts connections, completes Noise XK handshake
//! using its static key (= agent identity.key derived to X25519), then
//! passes decrypted payloads to the handler. Client (initiator) dials a
//! known endpoint, knows the responder's X25519 pubkey a priori (from
//! Agent Card lookup), and speaks the same frame protocol post-handshake.

use crate::transport::noise::{MAX_FRAME_BYTES, build_initiator, build_responder, encode_frame};
use mur_common::identity::AgentIdentity;
use std::future::Future;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;
use tracing::{debug, error, info, warn};

pub struct TcpTransportConfig {
    pub bind: String,
}

pub struct TcpListenerHandle {
    local_addr: SocketAddr,
    join: tokio::task::JoinHandle<()>,
}

impl TcpListenerHandle {
    pub fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }
    /// Wait for the listener task to exit. Call this AFTER dropping the
    /// shutdown sender so the listener actually stops.
    pub async fn await_shutdown(self) {
        let _ = self.join.await;
    }
}

/// Spawn a Noise-XK TCP listener. Shutdown by dropping `shutdown_rx` sender.
pub async fn spawn_tcp_listener<F, Fut>(
    cfg: TcpTransportConfig,
    identity: Arc<AgentIdentity>,
    handler: Arc<F>,
    mut shutdown_rx: mpsc::Receiver<()>,
) -> io::Result<TcpListenerHandle>
where
    F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<u8>, io::Error>> + Send + 'static,
{
    let listener = TcpListener::bind(&cfg.bind).await?;
    let local_addr = listener.local_addr()?;
    info!(%local_addr, "TCP Noise listener bound");

    let static_secret = identity.to_x25519_static_secret().to_bytes();

    let join = tokio::spawn(async move {
        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    info!("TCP listener shutting down");
                    break;
                }
                res = listener.accept() => {
                    match res {
                        Ok((stream, peer)) => {
                            debug!(%peer, "TCP accepted");
                            let h = handler.clone();
                            let s = static_secret;
                            tokio::spawn(async move {
                                if let Err(e) = handle_connection(stream, s, h).await {
                                    warn!(?e, "connection ended with error");
                                }
                            });
                        }
                        Err(e) => {
                            error!(?e, "accept failed");
                        }
                    }
                }
            }
        }
    });

    Ok(TcpListenerHandle { local_addr, join })
}

async fn handle_connection<F, Fut>(
    mut stream: TcpStream,
    static_secret: [u8; 32],
    handler: Arc<F>,
) -> io::Result<()>
where
    F: Fn(Vec<u8>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = Result<Vec<u8>, io::Error>> + Send + 'static,
{
    let mut noise = build_responder(&static_secret).map_err(|e| io::Error::other(e.to_string()))?;

    // handshake — msg 1 in
    let buf = read_framed(&mut stream).await?;
    let mut tmp = [0u8; 1024];
    noise
        .read_message(&buf, &mut tmp)
        .map_err(|e| io::Error::other(e.to_string()))?;

    // msg 2 out
    let mut out = [0u8; 1024];
    let n = noise
        .write_message(&[], &mut out)
        .map_err(|e| io::Error::other(e.to_string()))?;
    write_framed(&mut stream, &out[..n]).await?;

    // msg 3 in
    let buf = read_framed(&mut stream).await?;
    noise
        .read_message(&buf, &mut tmp)
        .map_err(|e| io::Error::other(e.to_string()))?;

    debug_assert!(noise.is_handshake_finished());
    let mut transport = noise
        .into_transport_mode()
        .map_err(|e| io::Error::other(e.to_string()))?;

    // Application loop
    loop {
        let cipher_frame = match read_framed(&mut stream).await {
            Ok(b) => b,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e),
        };
        let mut plain = vec![0u8; cipher_frame.len()];
        let n = transport
            .read_message(&cipher_frame, &mut plain)
            .map_err(|e| io::Error::other(e.to_string()))?;
        plain.truncate(n);

        let reply = handler(plain).await?;

        let mut cipher_out = vec![0u8; reply.len() + 16];
        let n = transport
            .write_message(&reply, &mut cipher_out)
            .map_err(|e| io::Error::other(e.to_string()))?;
        cipher_out.truncate(n);
        write_framed(&mut stream, &cipher_out).await?;
    }

    Ok(())
}

async fn read_framed(stream: &mut TcpStream) -> io::Result<Vec<u8>> {
    let mut hdr = [0u8; 4];
    stream.read_exact(&mut hdr).await?;
    let len = u32::from_be_bytes(hdr) as usize;
    if len > MAX_FRAME_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await?;
    Ok(buf)
}

async fn write_framed(stream: &mut TcpStream, payload: &[u8]) -> io::Result<()> {
    let framed = encode_frame(payload).map_err(|e| io::Error::other(e.to_string()))?;
    stream.write_all(&framed).await?;
    Ok(())
}

// ---- Client side ------------------------------------------------------

pub struct TcpConnector {
    stream: TcpStream,
    transport: snow::TransportState,
}

impl TcpConnector {
    pub async fn dial(
        addr: &str,
        identity: Arc<AgentIdentity>,
        remote_static_pub: &[u8; 32],
    ) -> io::Result<Self> {
        let mut stream = TcpStream::connect(addr).await?;
        let static_secret = identity.to_x25519_static_secret().to_bytes();
        let mut noise = build_initiator(&static_secret, remote_static_pub)
            .map_err(|e| io::Error::other(e.to_string()))?;

        // msg 1 out
        let mut buf = [0u8; 1024];
        let n = noise
            .write_message(&[], &mut buf)
            .map_err(|e| io::Error::other(e.to_string()))?;
        write_framed(&mut stream, &buf[..n]).await?;

        // msg 2 in
        let in_buf = read_framed(&mut stream).await?;
        let mut tmp = [0u8; 1024];
        noise
            .read_message(&in_buf, &mut tmp)
            .map_err(|e| io::Error::other(e.to_string()))?;

        // msg 3 out
        let n = noise
            .write_message(&[], &mut buf)
            .map_err(|e| io::Error::other(e.to_string()))?;
        write_framed(&mut stream, &buf[..n]).await?;

        debug_assert!(noise.is_handshake_finished());
        let transport = noise
            .into_transport_mode()
            .map_err(|e| io::Error::other(e.to_string()))?;
        Ok(Self { stream, transport })
    }

    pub async fn send(&mut self, payload: &[u8]) -> io::Result<()> {
        let mut cipher = vec![0u8; payload.len() + 16];
        let n = self
            .transport
            .write_message(payload, &mut cipher)
            .map_err(|e| io::Error::other(e.to_string()))?;
        cipher.truncate(n);
        write_framed(&mut self.stream, &cipher).await
    }

    pub async fn recv(&mut self) -> io::Result<Vec<u8>> {
        let cipher = read_framed(&mut self.stream).await?;
        let mut plain = vec![0u8; cipher.len()];
        let n = self
            .transport
            .read_message(&cipher, &mut plain)
            .map_err(|e| io::Error::other(e.to_string()))?;
        plain.truncate(n);
        Ok(plain)
    }
}
