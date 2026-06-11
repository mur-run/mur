//! argv[0] dispatch for the `murmur` symlink — `murmur <names…>` is
//! shorthand for `mur agent cli <names…>` (BusyBox convention, same as
//! `mur_agent_<name>` → `mur-agent-runtime`).

use std::ffi::OsString;

/// True when the invoked binary's file stem is `murmur` (case-insensitive;
/// tolerates a Windows `.exe` suffix and any leading path).
pub fn is_murmur_invocation(argv0: Option<&OsString>) -> bool {
    let Some(a) = argv0 else { return false };
    std::path::Path::new(a)
        .file_stem()
        .is_some_and(|s| s.to_string_lossy().eq_ignore_ascii_case("murmur"))
}

/// Rewrite murmur argv (`rest` excludes argv[0]) into a full
/// `mur agent cli …` argv for clap. When no positional agent name is
/// present: inject the concierge name `mur` if `concierge_exists`,
/// otherwise return `None` (caller prints the agent list and exits).
pub fn map_args(rest: &[OsString], concierge_exists: bool) -> Option<Vec<OsString>> {
    let has_name = rest.iter().any(|a| !a.to_string_lossy().starts_with('-'));
    let mut argv: Vec<OsString> = vec!["mur".into(), "agent".into(), "cli".into()];
    argv.extend(rest.iter().cloned());
    if !has_name {
        if !concierge_exists {
            return None;
        }
        argv.push("mur".into());
    }
    Some(argv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn os(v: &[&str]) -> Vec<OsString> {
        v.iter().map(OsString::from).collect()
    }

    #[test]
    fn detects_murmur_argv0_variants() {
        assert!(is_murmur_invocation(Some(&OsString::from("murmur"))));
        assert!(is_murmur_invocation(Some(&OsString::from(
            "/opt/homebrew/bin/murmur"
        ))));
        assert!(is_murmur_invocation(Some(&OsString::from("MURMUR.exe"))));
        assert!(!is_murmur_invocation(Some(&OsString::from(
            "/opt/homebrew/bin/mur"
        ))));
        assert!(!is_murmur_invocation(None));
    }

    #[test]
    fn maps_names_and_flags() {
        let argv = map_args(&os(&["a1", "a2", "--auto"]), false).unwrap();
        assert_eq!(argv, os(&["mur", "agent", "cli", "a1", "a2", "--auto"]));
    }

    #[test]
    fn no_name_injects_concierge_when_present() {
        let argv = map_args(&os(&["--resume"]), true).unwrap();
        assert_eq!(argv, os(&["mur", "agent", "cli", "--resume", "mur"]));
        let argv = map_args(&os(&[]), true).unwrap();
        assert_eq!(argv, os(&["mur", "agent", "cli", "mur"]));
    }

    #[test]
    fn no_name_no_concierge_returns_none() {
        assert!(map_args(&os(&[]), false).is_none());
    }
}
