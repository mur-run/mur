//! Media companion: control VLC and explain the current frame with the local
//! multimodal model. All control uses VLC's HTTP interface (no libVLC).

pub mod analyze;
pub mod error;
pub mod resolve;
pub mod scene;
pub mod transcript;
pub mod vlc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// Per-session VLC HTTP connection details. Generated once and persisted so
/// repeated tool calls reach the same running VLC instance.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VlcRuntime {
    pub port: u16,
    pub password: String,
    /// Directory VLC writes snapshots to (`--snapshot-path`).
    pub snapshot_dir: PathBuf,
}

/// Reserve a free localhost TCP port.
pub fn pick_free_port() -> std::io::Result<u16> {
    Ok(TcpListener::bind("127.0.0.1:0")?.local_addr()?.port())
}

/// 32-hex-char random password from the OS RNG (not a long-term secret, but
/// must not be guessable by other local users).
pub fn gen_password() -> anyhow::Result<String> {
    let mut buf = [0u8; 16];
    getrandom::getrandom(&mut buf)?;
    Ok(buf.iter().map(|b| format!("{b:02x}")).collect())
}

pub fn runtime_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("vlc.json")
}

/// Load the persisted runtime config, if present and parseable.
pub fn load_runtime(mur_home: &Path) -> Option<VlcRuntime> {
    let body = std::fs::read_to_string(runtime_path(mur_home)).ok()?;
    serde_json::from_str(&body).ok()
}

/// Persist the runtime config atomically.
pub fn save_runtime(mur_home: &Path, rt: &VlcRuntime) -> anyhow::Result<()> {
    let path = runtime_path(mur_home);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("json.tmp");
    let data = serde_json::to_vec_pretty(rt).context("serialize VlcRuntime")?;
    std::fs::write(&tmp, data).context("write runtime tmp")?;
    std::fs::rename(&tmp, &path).context("rename runtime tmp")?;
    Ok(())
}

/// Resolve the local OpenAI-compatible model base URL (e.g. http://127.0.0.1:PORT/v1).
pub(crate) fn local_base_url() -> anyhow::Result<String> {
    let home = crate::cmd::resolve_mur_home()?;
    mur_common::local_llm::read_base_url(&home)
        .context("local model endpoint not available (is MuR Hub running?)")
}

/// Reusable HTTP client with connection pooling for VLC and VLM endpoints.
pub fn shared_client() -> &'static reqwest::Client {
    static CLIENT: OnceLock<reqwest::Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        reqwest::Client::builder()
            .build()
            .expect("build reqwest Client")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn password_is_32_hex_chars() {
        let p = gen_password().unwrap();
        assert_eq!(p.len(), 32);
        assert!(p.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn runtime_roundtrips() {
        let home = TempDir::new().unwrap();
        assert!(load_runtime(home.path()).is_none());
        let rt = VlcRuntime {
            port: 50990,
            password: "abc123".into(),
            snapshot_dir: home.path().join("snaps"),
        };
        save_runtime(home.path(), &rt).unwrap();
        assert_eq!(load_runtime(home.path()), Some(rt));
    }
}
