//! VLC control via the HTTP interface.
//!
//! Most items here are `pub` but used only by the `mur-mcp-server` crate, not
//! by the `mur` binary — so clippy flags them as dead code in the binary
//! target. This is deliberate: these are library-facing exports.
#![allow(dead_code)]

use super::VlcRuntime;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};

/// Default macOS VLC binary path; overridable via `MUR_VLC_PATH`.
pub fn detect_vlc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MUR_VLC_PATH") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let candidate = Path::new("/Applications/VLC.app/Contents/MacOS/VLC");
    candidate.exists().then(|| candidate.to_path_buf())
}

/// Parsed subset of VLC's `requests/status.xml`.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VlcStatus {
    pub state: String, // "playing" | "paused" | "stopped"
    pub time: i64,     // seconds elapsed
    pub length: i64,   // seconds total
    pub volume: i64,   // raw VLC volume (256 == 100%)
}

/// Base URL for the VLC HTTP interface.
pub fn status_url(port: u16) -> String {
    format!("http://127.0.0.1:{port}/requests/status.xml")
}

/// Build a command URL: `…/status.xml?command=<cmd>[&<extra>]`.
pub fn command_url(port: u16, cmd: &str, extra: &[(&str, &str)]) -> String {
    let mut url = format!("{}?command={}", status_url(port), cmd);
    for (k, v) in extra {
        url.push('&');
        url.push_str(k);
        url.push('=');
        url.push_str(&urlencoding::encode(v));
    }
    url
}

/// Extract the text between `<tag>` and `</tag>` (first occurrence).
fn tag(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)? + open.len();
    let end = xml[start..].find(&close)? + start;
    Some(xml[start..end].to_string())
}

/// Parse the subset of status.xml we use. Missing fields default sensibly.
pub fn parse_status_xml(xml: &str) -> VlcStatus {
    VlcStatus {
        state: tag(xml, "state").unwrap_or_else(|| "stopped".into()),
        time: tag(xml, "time").and_then(|s| s.parse().ok()).unwrap_or(0),
        length: tag(xml, "length").and_then(|s| s.parse().ok()).unwrap_or(0),
        volume: tag(xml, "volume").and_then(|s| s.parse().ok()).unwrap_or(0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detect_respects_env_override() {
        // Point at a path that does not exist → None.
        unsafe { std::env::set_var("MUR_VLC_PATH", "/no/such/vlc") };
        assert_eq!(detect_vlc(), None);
        unsafe { std::env::remove_var("MUR_VLC_PATH") };
    }

    #[test]
    fn command_url_encodes_extra() {
        let u = command_url(8080, "in_play", &[("input", "https://x/y?a=b")]);
        assert!(u.starts_with("http://127.0.0.1:8080/requests/status.xml?command=in_play&input="));
        assert!(u.contains("https%3A%2F%2Fx%2Fy%3Fa%3Db"));
    }

    #[test]
    fn parse_status_extracts_fields() {
        let xml = "<root><volume>256</volume><state>playing</state><time>42</time><length>3600</length></root>";
        let s = parse_status_xml(xml);
        assert_eq!(s.state, "playing");
        assert_eq!(s.time, 42);
        assert_eq!(s.length, 3600);
        assert_eq!(s.volume, 256);
    }
}

// ── Runtime management ──

use super::{gen_password, load_runtime, pick_free_port, save_runtime};

/// Get the persisted runtime or create + persist a fresh one (does not spawn).
fn ensure_runtime(mur_home: &Path) -> Result<VlcRuntime> {
    if let Some(rt) = load_runtime(mur_home) {
        return Ok(rt);
    }
    let rt = VlcRuntime {
        port: pick_free_port().context("pick free port")?,
        password: gen_password(),
        snapshot_dir: mur_home.join("runtime").join("vlc-snapshots"),
    };
    std::fs::create_dir_all(&rt.snapshot_dir).ok();
    save_runtime(mur_home, &rt)?;
    Ok(rt)
}

/// Spawn VLC with the HTTP interface + snapshot path if it is not already
/// answering on the configured port.
async fn ensure_vlc_running(mur_home: &Path, client: &reqwest::Client) -> Result<VlcRuntime> {
    let rt = ensure_runtime(mur_home)?;
    // Probe: if status responds, VLC is up.
    if client
        .get(status_url(rt.port))
        .basic_auth("", Some(&rt.password))
        .send()
        .await
        .map(|r| r.status().is_success())
        .unwrap_or(false)
    {
        return Ok(rt);
    }
    let vlc = detect_vlc().context("VLC not found (install VLC.app)")?;
    std::process::Command::new(vlc)
        .args([
            "--extraintf=http",
            "--http-host=127.0.0.1",
            &format!("--http-port={}", rt.port),
            &format!("--http-password={}", rt.password),
            "--snapshot-format=png",
            &format!("--snapshot-path={}", rt.snapshot_dir.display()),
        ])
        .spawn()
        .context("spawn VLC")?;
    // Give the HTTP iface a moment to come up.
    for _ in 0..20 {
        tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        if client
            .get(status_url(rt.port))
            .basic_auth("", Some(&rt.password))
            .send()
            .await
            .map(|r| r.status().is_success())
            .unwrap_or(false)
        {
            return Ok(rt);
        }
    }
    anyhow::bail!("VLC HTTP interface did not come up on port {}", rt.port)
}

async fn get_status(rt: &VlcRuntime, client: &reqwest::Client) -> Result<VlcStatus> {
    let xml = client
        .get(status_url(rt.port))
        .basic_auth("", Some(&rt.password))
        .send()
        .await?
        .text()
        .await?;
    Ok(parse_status_xml(&xml))
}

async fn send_command(
    rt: &VlcRuntime,
    client: &reqwest::Client,
    cmd: &str,
    extra: &[(&str, &str)],
) -> Result<VlcStatus> {
    let xml = client
        .get(command_url(rt.port, cmd, extra))
        .basic_auth("", Some(&rt.password))
        .send()
        .await?
        .text()
        .await?;
    Ok(parse_status_xml(&xml))
}

// ── Public API ──

fn mur_home() -> Result<PathBuf> {
    crate::cmd::resolve_mur_home()
}

/// Open a local file path or a URL (e.g. YouTube) in VLC.
pub async fn open(source: &str) -> Result<VlcStatus> {
    let client = reqwest::Client::new();
    let home = mur_home()?;
    let rt = ensure_vlc_running(&home, &client).await?;
    send_command(&rt, &client, "in_play", &[("input", source)]).await
}

/// Playback control. `action` ∈ {play, pause, toggle, stop, seek, volume}.
/// `value` is seconds (seek) or raw VLC volume (volume).
pub async fn playback(action: &str, value: Option<f64>) -> Result<VlcStatus> {
    let client = reqwest::Client::new();
    let home = mur_home()?;
    let rt = ensure_vlc_running(&home, &client).await?;
    let v = value.unwrap_or(0.0);
    let vs = format!("{}", v as i64);
    match action {
        "play" => send_command(&rt, &client, "pl_forceresume", &[]).await,
        "pause" => send_command(&rt, &client, "pl_forcepause", &[]).await,
        "toggle" => send_command(&rt, &client, "pl_pause", &[]).await,
        "stop" => send_command(&rt, &client, "pl_stop", &[]).await,
        "seek" => send_command(&rt, &client, "seek", &[("val", &vs)]).await,
        "volume" => send_command(&rt, &client, "volume", &[("val", &vs)]).await,
        other => anyhow::bail!("unknown playback action: {other}"),
    }
}

/// Current playback status.
pub async fn status() -> Result<VlcStatus> {
    let client = reqwest::Client::new();
    let home = mur_home()?;
    let rt = ensure_vlc_running(&home, &client).await?;
    get_status(&rt, &client).await
}

/// Internal accessor for scene.rs: ensure running and return the runtime.
pub(super) async fn ensure_for_snapshot(client: &reqwest::Client) -> Result<VlcRuntime> {
    let home = mur_home()?;
    ensure_vlc_running(&home, client).await
}

pub(super) async fn snapshot_command(rt: &VlcRuntime, client: &reqwest::Client) -> Result<()> {
    let _ = send_command(rt, client, "snapshot", &[]).await?;
    Ok(())
}
