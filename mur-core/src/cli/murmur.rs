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

/// `mur agent cli` flags that consume the following argv token as their
/// value. Used to skip that token when scanning `rest` for a positional
/// agent name, so a flag's *value* (e.g. `develop` in `--fleet develop`)
/// is never mistaken for an agent name. The `--flag=value` form carries its
/// value inline in one token and does not need this list.
const VALUE_TAKING_FLAGS: &[&str] = &["--skin", "--budget-usd", "--fleet"];

/// Rewrite murmur argv (`rest` excludes argv[0]) into a full
/// `mur agent cli …` argv for clap. When no positional agent name is
/// present: inject the concierge name `mur` if `concierge_exists`,
/// otherwise return `None` (caller prints the agent list and exits).
pub fn map_args(rest: &[OsString], concierge_exists: bool) -> Option<Vec<OsString>> {
    let mut has_name = false;
    let mut skip_next = false;
    for a in rest {
        if skip_next {
            skip_next = false;
            continue;
        }
        let s = a.to_string_lossy();
        if s.starts_with('-') {
            if VALUE_TAKING_FLAGS.contains(&s.as_ref()) {
                skip_next = true;
            }
            continue;
        }
        has_name = true;
        break;
    }
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
    fn fleet_flag_value_is_not_mistaken_for_a_name() {
        let argv = map_args(&os(&["--fleet", "develop"]), true).unwrap();
        assert_eq!(
            argv,
            os(&["mur", "agent", "cli", "--fleet", "develop", "mur"])
        );
    }

    #[test]
    fn skin_flag_value_is_not_mistaken_for_a_name() {
        // Pre-existing bug, same root cause as --fleet: any value-taking
        // flag's argument could be read as a positional agent name.
        let argv = map_args(&os(&["--skin", "dark"]), true).unwrap();
        assert_eq!(argv, os(&["mur", "agent", "cli", "--skin", "dark", "mur"]));
    }

    #[test]
    fn fleet_flag_equals_form_is_not_mistaken_for_a_name() {
        let argv = map_args(&os(&["--fleet=develop"]), true).unwrap();
        assert_eq!(argv, os(&["mur", "agent", "cli", "--fleet=develop", "mur"]));
    }

    #[test]
    fn real_agent_name_still_wins_over_a_following_flag_value() {
        let argv = map_args(&os(&["a1", "--fleet", "develop"]), true).unwrap();
        assert_eq!(
            argv,
            os(&["mur", "agent", "cli", "a1", "--fleet", "develop"])
        );
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
