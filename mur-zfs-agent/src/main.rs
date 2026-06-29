use anyhow::Result;
use mur_common::zfs_protocol::{ZfsRequest, ZfsResponse};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixListener;

mod zfs;

const DEFAULT_SOCKET: &str = "/run/mur-zfs-agent.sock";

fn handle(req: ZfsRequest) -> ZfsResponse {
    match req {
        ZfsRequest::CreateTrack { base, name } => {
            let result = zfs::dataset_for_path(&base).and_then(|ds| {
                let snap = zfs::zfs_snapshot(&ds, &format!("mur-parallel-base-{name}"))?;
                let track_ds = format!("{ds}/mur-tracks/{name}");
                let mount = zfs::zfs_clone(&snap, &track_ds)?;
                // Establish base snapshot for diff/promote.
                zfs::zfs_snapshot(&track_ds, "mur-base")?;
                Ok(mount)
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
            match zfs::dataset_for_path(&track).and_then(|ds| {
                // `since` may be a bare label (e.g. "mur-base") or a full ref
                // (e.g. "pool/ds@mur-base"). If it already contains '@', use it
                // as-is; otherwise format as "{ds}@{since}".
                let snap_ref = if since.contains('@') {
                    since.clone()
                } else {
                    format!("{ds}@{since}")
                };
                zfs::zfs_diff_ref(&snap_ref, &ds)
            }) {
                Ok(paths) => ZfsResponse::Files { paths },
                Err(e) => ZfsResponse::Error {
                    message: e.to_string(),
                },
            }
        }
        ZfsRequest::Promote { track, target } => {
            let result = zfs::dataset_for_path(&track)
                .and_then(|ds| zfs::zfs_diff(&ds, "mur-base"))
                .and_then(|changed| {
                    for abs_path in &changed {
                        // zfs_diff returns absolute paths; strip the track mountpoint
                        // prefix to get a relative path before joining with src/dst.
                        let rel = abs_path.strip_prefix(&track).unwrap_or(abs_path);
                        let src = track.join(rel);
                        let dst = target.join(rel);
                        if src.is_file() {
                            if let Some(p) = dst.parent() { std::fs::create_dir_all(p)?; }
                            std::fs::copy(&src, &dst).map(|_| ())?;
                        }
                    }
                    Ok(())
                });
            match result {
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
    eprintln!("mur-zfs-agent listening on {socket_path}");

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
                        eprintln!("connection error from {peer_addr}: {e}");
                    }
                });
            }
            Err(e) => {
                eprintln!("accept error: {e}");
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
    let socket_path = std::env::var("MUR_ZFS_SOCKET")
        .unwrap_or_else(|_| DEFAULT_SOCKET.to_string());

    serve(&socket_path)
}
