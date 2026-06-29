use anyhow::Result;
use mur_common::zfs_protocol::{ZfsRequest, ZfsResponse};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;
use tracing::{error, info};

mod zfs;

const DEFAULT_SOCKET: &str = "/run/mur-zfs-agent.sock";

fn handle(req: ZfsRequest) -> ZfsResponse {
    match req {
        ZfsRequest::CreateTrack { base, name } => {
            let result = zfs::dataset_for_path(&base).and_then(|ds| {
                let snap = zfs::zfs_snapshot(&ds, &format!("mur-parallel-base-{name}"))?;
                let track_ds = format!("{ds}/mur-tracks/{name}");
                zfs::zfs_clone(&snap, &track_ds)
            });
            match result {
                Ok(path) => ZfsResponse::Track { path },
                Err(e) => ZfsResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        ZfsRequest::Snapshot { track, label } => {
            match zfs::dataset_for_path(&track)
                .and_then(|ds| zfs::zfs_snapshot(&ds, &label))
            {
                Ok(snap_id) => ZfsResponse::Snap { snap_id },
                Err(e) => ZfsResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        ZfsRequest::DiffFiles { track, since } => {
            match zfs::dataset_for_path(&track)
                .and_then(|ds| zfs::zfs_diff(&ds, &since))
            {
                Ok(paths) => ZfsResponse::Files { paths },
                Err(e) => ZfsResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        ZfsRequest::Promote { track, target: _ } => {
            // ZFS promote reverses origin/clone relationship, making the track
            // independent of its parent snapshot.
            match zfs::dataset_for_path(&track).and_then(|ds| zfs::zfs_promote(&ds)) {
                Ok(()) => ZfsResponse::Ok,
                Err(e) => ZfsResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        ZfsRequest::Destroy { track } => {
            match zfs::dataset_for_path(&track).and_then(|ds| zfs::zfs_destroy(&ds)) {
                Ok(()) => ZfsResponse::Ok,
                Err(e) => ZfsResponse::Error {
                    message: e.to_string(),
                },
            }
        }
    }
}

fn serve(socket_path: &str) -> Result<()> {
    // Remove stale socket if present.
    let _ = std::fs::remove_file(socket_path);

    let listener = UnixListener::bind(socket_path)?;
    info!("mur-zfs-agent listening on {socket_path}");

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                std::thread::spawn(move || {
                    let peer_addr = stream
                        .peer_addr()
                        .ok()
                        .and_then(|a| a.as_pathname().map(|p| p.display().to_string()))
                        .unwrap_or_else(|| "<unknown>".into());
                    if let Err(e) = handle_connection(stream) {
                        error!("connection error from {peer_addr}: {e}");
                    }
                });
            }
            Err(e) => {
                error!("accept error: {e}");
            }
        }
    }
    Ok(())
}

fn handle_connection(stream: std::os::unix::net::UnixStream) -> Result<()> {
    let mut writer = stream.try_clone()?;
    let reader = BufReader::new(stream);

    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let req: ZfsRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = ZfsResponse::Error {
                    message: format!("parse error: {e}"),
                };
                let mut out = serde_json::to_string(&resp)?;
                out.push('\n');
                writer.write_all(out.as_bytes())?;
                continue;
            }
        };
        let resp = handle(req);
        let mut out = serde_json::to_string(&resp)?;
        out.push('\n');
        writer.write_all(out.as_bytes())?;
    }
    Ok(())
}

fn main() -> Result<()> {
    tracing_subscriber::fmt::init();

    let socket_path = std::env::var("MUR_ZFS_SOCKET")
        .unwrap_or_else(|_| DEFAULT_SOCKET.to_string());

    serve(&socket_path)
}
