//! VLC control via the HTTP interface.

use super::VlcRuntime;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::time::Duration;

/// Default macOS VLC binary path.
const DEFAULT_VLC_PATH: &str = "/Applications/VLC.app/Contents/MacOS/VLC";

/// Locate VLC binary: env override `MUR_VLC_PATH`, else default path.
pub fn detect_vlc() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("MUR_VLC_PATH") {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    let candidate = Path::new(DEFAULT_VLC_PATH);
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

/// Parse the subset of VLC's `status.xml` using a proper XML reader.
/// Missing fields default sensibly.
pub fn parse_status_xml(xml: &str) -> VlcStatus {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    let mut state = "stopped".to_string();
    let mut time = 0i64;
    let mut length = 0i64;
    let mut volume = 0i64;
    let mut buf = Vec::new();
    let mut in_tag = String::new();

    loop {
        match reader.read_event_into(&mut buf) {
            Ok(Event::Start(ref e)) => {
                in_tag = String::from_utf8_lossy(e.name().as_ref()).into_owned();
            }
            // Clear the active tag when it closes. Without this, the pretty-printed
            // whitespace VLC emits *after* `</state>` arrives as a Text event while
            // `in_tag` is still "state" and clobbers the value (e.g. state == "\n").
            Ok(Event::End(_)) => {
                in_tag.clear();
            }
            Ok(Event::Text(ref e)) => {
                let text = e.unescape().unwrap_or_default().to_string();
                match in_tag.as_str() {
                    "state" => state = text,
                    "time" => time = text.parse().unwrap_or(0),
                    "length" => length = text.parse().unwrap_or(0),
                    "volume" => volume = text.parse().unwrap_or(0),
                    _ => {}
                }
            }
            Ok(Event::Eof) => break,
            _ => {}
        }
        buf.clear();
    }
    VlcStatus {
        state,
        time,
        length,
        volume,
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

    #[test]
    fn parse_status_pretty_printed_does_not_clobber_state() {
        // Real VLC status.xml is indented; the newline after `</state>` must not
        // overwrite the parsed value (regression: state came back as "\n").
        let xml = "<root>\n  <volume>256</volume>\n  <state>playing</state>\n  <time>42</time>\n  <length>3600</length>\n</root>\n";
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
    // `in_play` enqueues and *should* autoplay, but on a freshly-launched VLC it
    // can land in `stopped`. Nudge it into playback so callers (and especially
    // `scene_explain`, which snapshots the current frame) have a live frame.
    if status.state != "playing" {
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
