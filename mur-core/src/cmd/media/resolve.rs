//! Source resolution helpers: external-tool detection, DRM heuristic, last-source.

use std::path::{Path, PathBuf};

/// Detect yt-dlp: `MUR_YTDLP_PATH` override, else `yt-dlp` on PATH.
pub fn detect_ytdlp() -> Option<PathBuf> {
    detect_tool("MUR_YTDLP_PATH", "yt-dlp")
}

/// Detect ffmpeg: `MUR_FFMPEG_PATH` override, else `ffmpeg` on PATH.
pub fn detect_ffmpeg() -> Option<PathBuf> {
    detect_tool("MUR_FFMPEG_PATH", "ffmpeg")
}

fn detect_tool(env_key: &str, bin: &str) -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(env_key) {
        let p = PathBuf::from(p);
        return p.exists().then_some(p);
    }
    which_on_path(bin)
}

/// Minimal PATH search (avoids a new `which` crate dependency).
fn which_on_path(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path) {
        let candidate = dir.join(bin);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

/// Heuristic: does this URL host a DRM-protected streaming service we cannot capture?
pub fn is_drm_host(source: &str) -> bool {
    const DRM_HOSTS: &[&str] = &[
        "netflix.com",
        "disneyplus.com",
        "primevideo.com",
        "hbomax.com",
        "max.com",
        "hulu.com",
        "appletv.com",
    ];
    let lower = source.to_ascii_lowercase();
    DRM_HOSTS.iter().any(|h| lower.contains(h))
}

fn last_source_path(mur_home: &Path) -> PathBuf {
    mur_home.join("runtime").join("last_source")
}

/// Persist the most recently opened source string (plain text, atomic).
pub fn save_last_source(mur_home: &Path, source: &str) -> std::io::Result<()> {
    let path = last_source_path(mur_home);
    if let Some(p) = path.parent() {
        std::fs::create_dir_all(p)?;
    }
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, source.as_bytes())?;
    std::fs::rename(&tmp, &path)
}

/// Load the most recently opened source string, if any.
pub fn load_last_source(mur_home: &Path) -> Option<String> {
    let s = std::fs::read_to_string(last_source_path(mur_home)).ok()?;
    let s = s.trim().to_string();
    (!s.is_empty()).then_some(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn ytdlp_env_override_missing_is_none() {
        unsafe { std::env::set_var("MUR_YTDLP_PATH", "/no/such/yt-dlp") };
        assert_eq!(detect_ytdlp(), None);
        unsafe { std::env::remove_var("MUR_YTDLP_PATH") };
    }

    #[test]
    fn drm_hosts_detected() {
        assert!(is_drm_host("https://www.netflix.com/watch/123"));
        assert!(is_drm_host("https://DisneyPlus.com/x"));
        assert!(!is_drm_host("https://youtu.be/abc"));
        assert!(!is_drm_host("/home/me/movie.mkv"));
    }

    #[test]
    fn last_source_roundtrips() {
        let home = TempDir::new().unwrap();
        assert_eq!(load_last_source(home.path()), None);
        save_last_source(home.path(), "https://youtu.be/abc").unwrap();
        assert_eq!(load_last_source(home.path()).as_deref(), Some("https://youtu.be/abc"));
    }
}
