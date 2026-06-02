//! VLC control via the HTTP interface.

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
    pub state: String,   // "playing" | "paused" | "stopped"
    pub time: i64,       // seconds elapsed
    pub length: i64,     // seconds total
    pub volume: i64,     // raw VLC volume (256 == 100%)
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
