//! Compiled-in capabilities shipped with the binary (like builtin skills).

use mur_common::capability::Capability;

const MEDIA_YAML: &str = include_str!("media.yaml");

/// All capabilities shipped with this binary.
pub fn builtin_capabilities() -> Vec<Capability> {
    vec![serde_yaml_ng::from_str(MEDIA_YAML).expect("builtin media capability must parse")]
}

/// Look up a builtin capability by name.
pub fn find_builtin(name: &str) -> Option<Capability> {
    builtin_capabilities().into_iter().find(|c| c.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_capability_parses_and_bundles_the_media_pieces() {
        let media = find_builtin("media").expect("media capability present");
        assert_eq!(media.name, "media");
        for s in [
            "video-analyze",
            "watch-together",
            "scene-explain",
            "vlc-control",
        ] {
            assert!(media.skills.iter().any(|x| x == s), "missing skill {s}");
        }
        for p in ["vlc", "yt-dlp"] {
            assert!(
                media.requires_programs.iter().any(|d| d.name == p),
                "missing dep {p}"
            );
        }
        assert_eq!(media.mcp_servers.len(), 1);
        assert_eq!(media.mcp_servers[0].command, "mur-mcp-server");
    }
}
