//! `~/.mur/browser/` layout (SPEC §3.4). Single source of truth for paths so
//! slices 2–6 never hard-code strings.

use std::path::{Path, PathBuf};

/// Root of all browser state under a MUR home.
pub fn browser_root(mur_home: &Path) -> PathBuf {
    mur_home.join("browser")
}

/// `profiles/<site>/` — encrypted storageState + meta.
pub fn profile_dir(mur_home: &Path, site: &str) -> PathBuf {
    browser_root(mur_home).join("profiles").join(site)
}

/// `profiles/<site>/state.json.age`
pub fn profile_state(mur_home: &Path, site: &str) -> PathBuf {
    profile_dir(mur_home, site).join("state.json.age")
}

/// `profiles/<site>/meta.yaml`
pub fn profile_meta(mur_home: &Path, site: &str) -> PathBuf {
    profile_dir(mur_home, site).join("meta.yaml")
}

/// `runs/<name>/` — one recorded run.
pub fn run_dir(mur_home: &Path, run: &str) -> PathBuf {
    browser_root(mur_home).join("runs").join(run)
}

/// `runs/<name>/actions.yaml` — the source of truth for replay.
pub fn run_actions(mur_home: &Path, run: &str) -> PathBuf {
    run_dir(mur_home, run).join("actions.yaml")
}

/// `runs/<name>/hits.jsonl`
pub fn run_hits(mur_home: &Path, run: &str) -> PathBuf {
    run_dir(mur_home, run).join("hits.jsonl")
}

/// `runs/<name>/report.md`
pub fn run_report(mur_home: &Path, run: &str) -> PathBuf {
    run_dir(mur_home, run).join("report.md")
}

/// `broker.sock` — 0600, created by `record`, removed on exit.
pub fn broker_socket(mur_home: &Path) -> PathBuf {
    browser_root(mur_home).join("broker.sock")
}

/// Run and site names are used as directory components; keep them boring.
pub fn validate_name(name: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
        && !name.starts_with('.');
    anyhow::ensure!(
        ok,
        "invalid name {name:?}: use [A-Za-z0-9._-], max 64 chars, not starting with '.'"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_matches_spec() {
        let home = Path::new("/h");
        assert_eq!(
            run_actions(home, "smoke"),
            PathBuf::from("/h/browser/runs/smoke/actions.yaml")
        );
        assert_eq!(
            profile_state(home, "pchome"),
            PathBuf::from("/h/browser/profiles/pchome/state.json.age")
        );
        assert_eq!(broker_socket(home), PathBuf::from("/h/browser/broker.sock"));
    }

    #[test]
    fn names_are_path_safe() {
        assert!(validate_name("pchome-login_v2").is_ok());
        assert!(validate_name("../etc").is_err());
        assert!(validate_name(".hidden").is_err());
        assert!(validate_name("a/b").is_err());
        assert!(validate_name("").is_err());
    }
}
