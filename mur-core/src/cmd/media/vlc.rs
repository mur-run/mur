//! VLC control via the HTTP interface.

use super::VlcRuntime;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default macOS VLC binary path.
const DEFAULT_VLC_PATH: &str = "/Applications/VLC.app/Contents/MacOS/VLC";

/// After `in_play`, VLC autoplays asynchronously: the command's own status response
/// can still read `stopped` before the item-open state machine reaches `playing`.
/// Firing the `pl_play` nudge in that window races VLC's open and can leave playback
/// stopped (flaky, timing-dependent). So when the immediate state is idle we poll the
/// status a few times — giving in_play's autoplay a chance — and only nudge if it is
/// *still* idle (the genuine freshly-launched-VLC case the nudge exists for).
const AUTOPLAY_GRACE_POLLS: u32 = 6;
const AUTOPLAY_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Locate VLC binary: env override `MUR_VLC_PATH`, else default path.
pub fn detect_vlc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MUR_VLC_PATH") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let candidate = Path::new(DEFAULT_VLC_PATH);
    candidate.exists().then(|| candidate.to_path_buf())
}

// `VlcStatus` + `parse_status_xml` live in `mur-common::media` so the runtime's
// WatchScheduler (which cannot depend on mur-core) shares the same parser.
pub use mur_common::media::{VlcStatus, parse_status_xml};

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

// ── Runtime management ──

use super::{gen_password, load_runtime, pick_free_port, save_runtime};

/// Get the persisted runtime or create + persist a fresh one (does not spawn).
fn ensure_runtime(mur_home: &Path) -> Result<VlcRuntime> {
    if let Some(rt) = load_runtime(mur_home) {
        return Ok(rt);
    }
    let rt = VlcRuntime {
        port: pick_free_port().context("pick free port")?,
        password: gen_password().context("generate VLC password")?,
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
        .timeout(Duration::from_secs(3))
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
            .timeout(Duration::from_secs(3))
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
        .timeout(Duration::from_secs(5))
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
        .timeout(Duration::from_secs(5))
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
#[allow(dead_code)]
pub async fn open(source: &str) -> Result<VlcStatus> {
    let client = super::shared_client();
    let home = mur_home()?;
    // Remember the original source so `video_analyze` (no arg) can resolve it later;
    // VLC's status.xml does not expose a usable source URI. (Spec §4.2.)
    let _ = super::resolve::save_last_source(&home, source);
    let rt = ensure_vlc_running(&home, client).await?;
    let status = send_command(&rt, client, "in_play", &[("input", source)]).await?;
    // `in_play` enqueues and *should* autoplay, but its immediate status response can
    // still read `stopped` before VLC's asynchronous item-open reaches `playing`, and a
    // freshly-launched VLC can land in `stopped` for real. Nudge ONLY from a genuinely
    // idle state — not `paused` (user paused), `opening`/`buffering` (already
    // progressing), or `playing`. To avoid racing the item-open (a premature `pl_play`
    // can leave playback stopped — flaky and timing-dependent), first give in_play's own
    // autoplay a brief grace window, polling the status; only nudge if still idle after.
    if matches!(status.state.as_str(), "stopped" | "") {
        for _ in 0..AUTOPLAY_GRACE_POLLS {
            tokio::time::sleep(AUTOPLAY_POLL_INTERVAL).await;
            let s = get_status(&rt, client).await?;
            if !matches!(s.state.as_str(), "stopped" | "") {
                return Ok(s); // in_play autoplayed on its own — no nudge needed
            }
        }
        return send_command(&rt, client, "pl_play", &[]).await;
    }
    Ok(status)
}

/// Playback control. `action` ∈ {play, pause, toggle, stop, seek, volume}.
/// `value` is seconds (seek) or raw VLC volume (volume).
#[allow(dead_code)]
pub async fn playback(action: &str, value: Option<f64>) -> Result<VlcStatus> {
    let client = super::shared_client();
    let home = mur_home()?;
    let rt = ensure_vlc_running(&home, client).await?;
    let v = value.unwrap_or(0.0);
    let vs = format!("{}", v as i64);
    match action {
        "play" => send_command(&rt, client, "pl_forceresume", &[]).await,
        "pause" => send_command(&rt, client, "pl_forcepause", &[]).await,
        "toggle" => send_command(&rt, client, "pl_pause", &[]).await,
        "stop" => send_command(&rt, client, "pl_stop", &[]).await,
        "seek" => send_command(&rt, client, "seek", &[("val", &vs)]).await,
        "volume" => send_command(&rt, client, "volume", &[("val", &vs)]).await,
        other => anyhow::bail!("unknown playback action: {other}"),
    }
}

/// Current playback status.
#[allow(dead_code)]
pub async fn status() -> Result<VlcStatus> {
    let client = super::shared_client();
    let home = mur_home()?;
    let rt = ensure_vlc_running(&home, client).await?;
    get_status(&rt, client).await
}

/// Internal accessor for scene.rs: ensure running and return the runtime.
#[allow(dead_code)]
pub(super) async fn ensure_for_snapshot(client: &reqwest::Client) -> Result<VlcRuntime> {
    let home = mur_home()?;
    ensure_vlc_running(&home, client).await
}

#[allow(dead_code)]
pub(super) async fn snapshot_command(rt: &VlcRuntime, client: &reqwest::Client) -> Result<()> {
    let _ = send_command(rt, client, "snapshot", &[]).await?;
    Ok(())
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
    // VlcStatus / parse_status_xml tests live in mur-common::media (where the
    // parser now lives).
}
