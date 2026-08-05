//! GitHub Releases API client used by `mur update`.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub assets: Vec<ReleaseAsset>,
}

/// Returns the expected asset filename for the current host platform, or `None`
/// if no prebuilt binary is published for it.
pub fn asset_name_for_host() -> Option<&'static str> {
    asset_name_for(std::env::consts::OS, std::env::consts::ARCH)
}

pub fn asset_name_for(os: &str, arch: &str) -> Option<&'static str> {
    match (os, arch) {
        ("macos", "aarch64") => Some("mur-aarch64-apple-darwin.tar.gz"),
        ("linux", "x86_64") => Some("mur-x86_64-unknown-linux-gnu.tar.gz"),
        ("windows", "x86_64") => Some("mur-x86_64-pc-windows-msvc.zip"),
        _ => None,
    }
}

/// Find the asset matching `name` in a release; returns the asset or an error.
pub fn select_asset<'a>(release: &'a Release, name: &str) -> anyhow::Result<&'a ReleaseAsset> {
    release
        .assets
        .iter()
        .find(|a| a.name == name)
        .ok_or_else(|| anyhow::anyhow!("no asset named {name} in release {}", release.tag_name))
}

#[cfg(test)]
mod asset_tests {
    use super::*;

    fn fake_release() -> Release {
        Release {
            tag_name: "v2.17.0".into(),
            assets: vec![
                ReleaseAsset {
                    name: "mur-aarch64-apple-darwin.tar.gz".into(),
                    browser_download_url: "https://example/mac.tgz".into(),
                },
                ReleaseAsset {
                    name: "mur-x86_64-unknown-linux-gnu.tar.gz".into(),
                    browser_download_url: "https://example/lnx.tgz".into(),
                },
                ReleaseAsset {
                    name: "checksums.txt".into(),
                    browser_download_url: "https://example/c.txt".into(),
                },
            ],
        }
    }

    #[test]
    fn maps_known_targets() {
        assert_eq!(
            asset_name_for("macos", "aarch64"),
            Some("mur-aarch64-apple-darwin.tar.gz")
        );
        assert_eq!(
            asset_name_for("linux", "x86_64"),
            Some("mur-x86_64-unknown-linux-gnu.tar.gz")
        );
        assert_eq!(
            asset_name_for("windows", "x86_64"),
            Some("mur-x86_64-pc-windows-msvc.zip")
        );
    }

    #[test]
    fn returns_none_for_unsupported() {
        assert_eq!(asset_name_for("linux", "aarch64"), None);
        assert_eq!(asset_name_for("macos", "x86_64"), None);
    }

    #[test]
    fn select_asset_finds_match() {
        let r = fake_release();
        let a = select_asset(&r, "mur-aarch64-apple-darwin.tar.gz").unwrap();
        assert_eq!(a.browser_download_url, "https://example/mac.tgz");
    }

    #[test]
    fn select_asset_errors_when_missing() {
        let r = fake_release();
        assert!(select_asset(&r, "mur-aarch64-unknown-linux-gnu.tar.gz").is_err());
    }
}

/// Strip a leading `v` (or `V`) from a git-tag-style version string.
pub fn strip_v_prefix(tag: &str) -> &str {
    tag.strip_prefix('v')
        .or_else(|| tag.strip_prefix('V'))
        .unwrap_or(tag)
}

/// Returns `true` when `latest` is strictly newer than `current` per semver.
pub fn is_newer(current: &str, latest: &str) -> anyhow::Result<bool> {
    let cur = semver::Version::parse(strip_v_prefix(current))?;
    let lat = semver::Version::parse(strip_v_prefix(latest))?;
    Ok(lat > cur)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_v_prefix_handles_both_cases() {
        assert_eq!(strip_v_prefix("v1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix("V1.2.3"), "1.2.3");
        assert_eq!(strip_v_prefix("1.2.3"), "1.2.3");
    }

    #[test]
    fn is_newer_detects_patch_bump() {
        assert!(is_newer("2.16.0", "v2.16.1").unwrap());
    }

    #[test]
    fn is_newer_rejects_same_version() {
        assert!(!is_newer("2.16.1", "v2.16.1").unwrap());
    }

    #[test]
    fn is_newer_rejects_older() {
        assert!(!is_newer("2.16.1", "v2.16.0").unwrap());
    }

    #[test]
    fn is_newer_errors_on_garbage() {
        assert!(is_newer("nope", "v1.0.0").is_err());
    }
}

// ─── Releases API client with cache ─────────────────────────────────────────

use std::path::PathBuf;
use std::time::{Duration, SystemTime};

const LATEST_URL: &str = "https://api.github.com/repos/mur-run/mur/releases/latest";
const CACHE_TTL: Duration = Duration::from_secs(300);
const USER_AGENT: &str = concat!("mur-update/", env!("CARGO_PKG_VERSION"));

fn cache_path() -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(home.join(".mur").join("update-cache.json"))
}

/// Read the cached release if it is still fresh.
pub fn read_cached_release(path: &std::path::Path, now: SystemTime) -> Option<Release> {
    let meta = std::fs::metadata(path).ok()?;
    let modified = meta.modified().ok()?;
    if now.duration_since(modified).ok()? > CACHE_TTL {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice::<Release>(&bytes).ok()
}

pub fn write_cached_release(path: &std::path::Path, release: &Release) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_vec(release)?)?;
    Ok(())
}

/// Fetch the latest release, using a 5-minute cache to avoid rate limits.
pub fn fetch_latest() -> anyhow::Result<Release> {
    let cache = cache_path();
    if let Some(p) = cache.as_ref()
        && let Some(r) = read_cached_release(p, SystemTime::now())
    {
        return Ok(r);
    }
    let client = reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?;
    let resp = client.get(LATEST_URL).send()?;
    if resp.status() == reqwest::StatusCode::FORBIDDEN {
        anyhow::bail!("GitHub API rate limit reached. Try again in a few minutes.");
    }
    let release: Release = resp.error_for_status()?.json()?;
    if let Some(p) = cache.as_ref() {
        let _ = write_cached_release(p, &release);
    }
    Ok(release)
}

#[cfg(test)]
mod cache_tests {
    use super::*;
    use std::time::{Duration, SystemTime};

    fn release_fixture() -> Release {
        Release {
            tag_name: "v9.9.9".into(),
            assets: vec![],
        }
    }

    #[test]
    fn round_trips_cache_within_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        write_cached_release(&path, &release_fixture()).unwrap();
        let got = read_cached_release(&path, SystemTime::now()).unwrap();
        assert_eq!(got.tag_name, "v9.9.9");
    }

    #[test]
    fn expires_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cache.json");
        write_cached_release(&path, &release_fixture()).unwrap();
        let future = SystemTime::now() + Duration::from_secs(301);
        assert!(read_cached_release(&path, future).is_none());
    }
}

// ─── Checksum verification ───────────────────────────────────────────────────

use sha2::{Digest, Sha256};

/// Parse a `checksums.txt` line ("<hex> *<filename>" or "<hex>  <filename>")
/// and return the SHA256 hex for `filename`, or `None`.
pub fn checksum_for(checksums_txt: &str, filename: &str) -> Option<String> {
    for line in checksums_txt.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let hex = parts.next()?.trim();
        let rest = parts.next()?.trim_start_matches('*').trim();
        if rest == filename {
            return Some(hex.to_string());
        }
    }
    None
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

#[cfg(test)]
mod checksum_tests {
    use super::*;

    #[test]
    fn parses_bsd_style_with_asterisk() {
        let txt = "deadbeef *mur-x86_64.tar.gz\nfeedface *mur.zip\n";
        assert_eq!(
            checksum_for(txt, "mur-x86_64.tar.gz").as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn parses_gnu_style_with_double_space() {
        let txt = "deadbeef  mur-x86_64.tar.gz\n";
        assert_eq!(
            checksum_for(txt, "mur-x86_64.tar.gz").as_deref(),
            Some("deadbeef")
        );
    }

    #[test]
    fn returns_none_when_missing() {
        let txt = "deadbeef *mur.zip\n";
        assert!(checksum_for(txt, "other.tar.gz").is_none());
    }

    #[test]
    fn sha256_of_known_string() {
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}

// ─── Archive extraction ──────────────────────────────────────────────────────

use std::io::{Read, Write};
use std::path::Path;

/// Extract the named binary from a release archive (tar.gz or zip) and write
/// it to `dest`. `entry_name` is the exact file name inside the archive (e.g.
/// `mur`, `mur-agent-runtime`, `mur.exe`). Errors if the entry is absent.
pub fn extract_binary(
    archive_name: &str,
    bytes: &[u8],
    entry_name: &str,
    dest: &Path,
) -> anyhow::Result<()> {
    if archive_name.ends_with(".tar.gz") || archive_name.ends_with(".tgz") {
        let gz = flate2::read::GzDecoder::new(bytes);
        let mut tar = tar::Archive::new(gz);
        for entry in tar.entries()? {
            let mut entry = entry?;
            let path = entry.path()?.into_owned();
            let name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
            if name == entry_name {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                std::fs::File::create(dest)?.write_all(&buf)?;
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    std::fs::set_permissions(dest, std::fs::Permissions::from_mode(0o755))?;
                }
                return Ok(());
            }
        }
        anyhow::bail!("archive {archive_name} did not contain a `{entry_name}` binary");
    }
    if archive_name.ends_with(".zip") {
        let cursor = std::io::Cursor::new(bytes);
        let mut zip = zip::ZipArchive::new(cursor)?;
        for i in 0..zip.len() {
            let mut entry = zip.by_index(i)?;
            let name = entry.name().rsplit('/').next().unwrap_or("").to_string();
            if name == entry_name {
                let mut buf = Vec::new();
                entry.read_to_end(&mut buf)?;
                std::fs::File::create(dest)?.write_all(&buf)?;
                return Ok(());
            }
        }
        anyhow::bail!("archive {archive_name} did not contain {entry_name}");
    }
    anyhow::bail!("unknown archive type: {archive_name}")
}

#[cfg(test)]
mod extract_tests {
    use super::*;

    fn build_tgz(contents: &[(&str, &[u8])]) -> Vec<u8> {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gz);
            for (name, data) in contents {
                let mut header = tar::Header::new_gnu();
                header.set_size(data.len() as u64);
                header.set_mode(0o755);
                header.set_cksum();
                tar.append_data(&mut header, name, *data).unwrap();
            }
            tar.finish().unwrap();
        }
        gz.finish().unwrap()
    }

    #[test]
    fn extracts_mur_from_tgz() {
        let bytes = build_tgz(&[("mur", b"FAKEBIN")]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("mur");
        extract_binary("foo.tar.gz", &bytes, "mur", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"FAKEBIN");
    }

    #[test]
    fn extracts_named_sibling_from_tgz() {
        let bytes = build_tgz(&[("mur", b"MURBIN"), ("mur-agent-runtime", b"RUNTIMEBIN")]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("mur-agent-runtime");
        extract_binary("foo.tar.gz", &bytes, "mur-agent-runtime", &dest).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"RUNTIMEBIN");
    }

    #[test]
    fn errors_when_mur_missing_from_tgz() {
        let bytes = build_tgz(&[("README", b"hi")]);
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("mur");
        assert!(extract_binary("foo.tar.gz", &bytes, "mur", &dest).is_err());
    }
}
