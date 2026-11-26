//! `mur agent doctor`: `filesystem.no_delete` entries that will not hold (#2582).
//!
//! Two ways an entry protects nothing: the platform's sandbox has no
//! equivalent (the agent then refuses to start, so say why before it does),
//! or the path sits outside every write grant, so it was never writable and
//! the entry changes nothing.

use mur_agent_runtime::sandbox::policy::{NO_DELETE_SUPPORTED, expand_entitlement_path};
use mur_common::agent::FilesystemEntitlement;

/// One warning line per ineffective entry, already prefixed with the agent.
pub fn lines(name: &str, fs: &FilesystemEntitlement) -> Vec<String> {
    lines_for(name, fs, NO_DELETE_SUPPORTED)
}

fn lines_for(name: &str, fs: &FilesystemEntitlement, supported: bool) -> Vec<String> {
    if fs.no_delete.is_empty() {
        return Vec::new();
    }
    if !supported {
        return vec![format!(
            "  {name}: no_delete is set but this platform's sandbox cannot enforce it — \
             the agent will refuse to start (remove with: mur agent perm remove-path \
             {name} no_delete <path>)"
        )];
    }
    let writes: Vec<_> = fs
        .write
        .iter()
        .map(|w| expand_entitlement_path(w))
        .collect();
    fs.no_delete
        .iter()
        .filter(|n| {
            let p = expand_entitlement_path(n);
            !writes.iter().any(|w| p.starts_with(w))
        })
        .map(|n| {
            format!(
                "  {name}: no_delete {n} is outside every write grant — not writable at \
                 all, so the entry changes nothing"
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fs(write: &[&str], nd: &[&str]) -> FilesystemEntitlement {
        FilesystemEntitlement {
            write: write.iter().map(|s| s.to_string()).collect(),
            no_delete: nd.iter().map(|s| s.to_string()).collect(),
            ..Default::default()
        }
    }

    #[test]
    fn silent_when_there_are_no_entries() {
        assert!(lines_for("a", &fs(&["/w"], &[]), false).is_empty());
    }

    #[test]
    fn flags_only_entries_outside_every_write_grant() {
        let out = lines_for("a", &fs(&["/w"], &["/w/keep", "/elsewhere"]), true);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("/elsewhere"));
    }

    #[test]
    fn an_unsupported_platform_gets_one_line_naming_the_fix() {
        let out = lines_for("a", &fs(&["/w"], &["/w/keep"]), false);
        assert_eq!(out.len(), 1);
        assert!(out[0].contains("refuse to start") && out[0].contains("remove-path"));
    }
}
