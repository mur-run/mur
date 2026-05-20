//! GitHub Releases API client used by `mur update`.

use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub struct ReleaseAsset {
    pub name: String,
    pub browser_download_url: String,
}

#[derive(Debug, Clone, Deserialize)]
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
    tag.strip_prefix('v').or_else(|| tag.strip_prefix('V')).unwrap_or(tag)
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
