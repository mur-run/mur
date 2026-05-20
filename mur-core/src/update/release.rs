//! GitHub Releases API client used by `mur update`.

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
