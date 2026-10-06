use super::ParallelBackend;
use anyhow::{Context, Result, bail};
use mur_common::zfs_protocol::{ZfsRequest, ZfsResponse};
use std::io::{BufRead, BufReader, Write};
#[cfg(unix)]
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Fail fast rather than wedge the whole parallel run if the agent hangs.
const AGENT_IO_TIMEOUT: Duration = Duration::from_secs(30);

/// Backend that talks to `mur-zfs-agent` running inside a Linux VM (OrbStack / Lima / WSL2)
/// via a Unix domain socket forwarded to the host.
pub struct ZfsSocketBackend {
    pub socket_path: PathBuf,
    /// Project root seen by VM. OrbStack/Lima mirror the host FS at the same absolute path.
    pub project_root: PathBuf,
}

impl ZfsSocketBackend {
    pub fn new(socket_path: PathBuf, project_root: PathBuf) -> Self {
        Self {
            socket_path,
            project_root,
        }
    }

    #[cfg(unix)]
    fn call(&self, req: ZfsRequest) -> Result<ZfsResponse> {
        let stream =
            UnixStream::connect(&self.socket_path).context("connect to mur-zfs-agent socket")?;
        // Bound every read/write so a hung agent fails fast instead of wedging
        // the parallel run forever on a blocking `read_line`.
        let _ = stream.set_read_timeout(Some(AGENT_IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(AGENT_IO_TIMEOUT));
        let mut writer = stream.try_clone()?;
        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        writer.write_all(line.as_bytes())?;
        drop(writer);
        let mut reader = BufReader::new(stream);
        let mut resp_line = String::new();
        reader.read_line(&mut resp_line)?;
        serde_json::from_str(resp_line.trim()).context("parse ZfsResponse")
    }

    #[cfg(not(unix))]
    fn call(&self, _req: ZfsRequest) -> Result<ZfsResponse> {
        bail!("ZFS socket backend requires Unix")
    }
}

impl ParallelBackend for ZfsSocketBackend {
    fn create_track(&self, name: &str) -> Result<PathBuf> {
        match self.call(ZfsRequest::CreateTrack {
            base: self.project_root.clone(),
            name: name.into(),
        })? {
            ZfsResponse::Track { path } => Ok(path),
            ZfsResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn base_snapshot(&self, _track: &Path) -> Result<String> {
        // @mur-base is established by create_track; this is a pure read.
        // Return a bare label; the agent's DiffFiles handler accepts both bare
        // labels ("mur-base") and full refs ("pool/ds@mur-base").
        Ok("mur-base".to_string())
    }

    fn diff_files(&self, track: &Path, since_snapshot: &str) -> Result<Vec<PathBuf>> {
        match self.call(ZfsRequest::DiffFiles {
            track: track.into(),
            since: since_snapshot.into(),
        })? {
            ZfsResponse::Files { paths } => {
                // Agent returns absolute paths; strip track prefix to match
                // ZfsNativeBackend contract of returning relative paths.
                Ok(paths
                    .into_iter()
                    .map(|p| {
                        p.strip_prefix(track)
                            .ok()
                            .map(|r| r.to_path_buf())
                            .unwrap_or(p)
                    })
                    .collect())
            }
            ZfsResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn promote(&self, track: &Path, target: &Path) -> Result<()> {
        match self.call(ZfsRequest::Promote {
            track: track.into(),
            target: target.into(),
        })? {
            ZfsResponse::Ok => Ok(()),
            ZfsResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }

    fn destroy(&self, track: &Path) -> Result<()> {
        match self.call(ZfsRequest::Destroy {
            track: track.into(),
        })? {
            ZfsResponse::Ok => Ok(()),
            ZfsResponse::Error { message } => bail!("{message}"),
            other => bail!("unexpected response: {other:?}"),
        }
    }
}

/// OrbStack forwards guest sockets to `~/.orbstack/run/sockets/` on the host.
/// Start `mur-zfs-agent` inside OrbStack: `orb run -- mur-zfs-agent`
pub fn connect_orbstack_socket() -> Result<PathBuf> {
    let path = dirs::home_dir()
        .context("no home dir")?
        .join(".orbstack/run/sockets/mur-zfs-agent.sock");
    if path.exists() {
        Ok(path)
    } else {
        bail!("OrbStack socket not found: {path:?}")
    }
}

/// Lima forwards guest sockets via socket forwarding configuration.
/// Add to Lima VM config: `portForwards: [{guestSocket: "/run/mur-zfs-agent.sock"}]`
pub fn connect_lima_socket(name: &str) -> Result<PathBuf> {
    let path = dirs::home_dir()
        .context("no home dir")?
        .join(format!(".lima/{name}/sock/mur-zfs-agent.sock"));
    if path.exists() {
        Ok(path)
    } else {
        bail!("Lima socket not found for VM {name:?}: {path:?}")
    }
}

/// WSL2 exposes guest AF_UNIX sockets to Windows 10 1903+ host side.
/// Run `mur-zfs-agent` inside WSL2; path is the Windows-side socket file.
#[cfg(windows)]
pub fn connect_wsl2_socket() -> Result<PathBuf> {
    let appdata = std::env::var("LOCALAPPDATA").context("LOCALAPPDATA not set")?;
    let path = PathBuf::from(appdata).join("Temp\\mur-zfs-agent.sock");
    if path.exists() {
        Ok(path)
    } else {
        bail!("WSL2 socket not found: {path:?}")
    }
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use mur_common::zfs_protocol::ZfsResponse;
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixListener;

    fn mock_agent(socket_path: &Path) {
        let listener = UnixListener::bind(socket_path).unwrap();
        std::thread::spawn(move || {
            if let Ok((stream, _)) = listener.accept() {
                let mut w = stream.try_clone().unwrap();
                let r = BufReader::new(stream);
                // `map_while` (not `flatten`): stop on the first IO error
                // instead of spinning forever if it repeats.
                for line in r.lines().map_while(Result::ok) {
                    let _ = line;
                    let mut resp = serde_json::to_string(&ZfsResponse::Ok).unwrap();
                    resp.push('\n');
                    let _ = w.write_all(resp.as_bytes());
                }
            }
        });
    }

    #[test]
    fn socket_backend_destroy_ok() {
        let sock = PathBuf::from(format!("/tmp/mur-zfs-test-{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&sock);
        mock_agent(&sock);
        std::thread::sleep(std::time::Duration::from_millis(30));
        let backend = ZfsSocketBackend::new(sock.clone(), "/tmp/fake-project".into());
        let result = backend.destroy(Path::new("/tmp/fake-track"));
        let _ = std::fs::remove_file(&sock);
        assert!(result.is_ok());
    }
}
