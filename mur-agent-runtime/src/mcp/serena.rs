//! serena (LSP-backed code navigation) launched as an MCP server with
//! `kind: serena` (code-nav Phase 2).
//!
//! Everything here is derived from the agent home and the entry's fixed
//! `project:` — never from the session cwd and never from the profile's
//! free-form fields — so the spawn site, the preflight and the tool filter
//! all see the same paths. The pure preflight (C1–C8) lives alongside in
//! task 2.3; spawn wiring is 2.4–2.6.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The only serena tools an agent ever sees. An allow-list, not a
/// deny-list: a tool added by a future serena release stays hidden until
/// it is reviewed and listed here.
///
/// Five, not six: `get_diagnostics_for_file` is excluded because for Rust
/// it sends `didSave` first, which runs `cargo check` (plan D2).
pub const SERENA_TOOL_ALLOWLIST: [&str; 5] = [
    "get_symbols_overview",
    "find_symbol",
    "find_referencing_symbols",
    "find_implementations",
    "find_declaration",
];

/// Directory under the agent home used as serena's `SERENA_HOME`.
pub const SERENA_HOME_DIR: &str = "serena";
/// serena's global config file name inside `SERENA_HOME`.
pub const SERENA_CONFIG_FILE: &str = "serena_config.yml";
/// MUR-owned parent of every per-project serena folder, inside
/// `SERENA_HOME`. `project_serena_folder_location` must resolve under it,
/// so serena never falls back to the repo's own `.serena/`.
pub const SERENA_PROJECTS_DIR: &str = "projects";

/// Env var serena reads to locate its home directory.
pub const SERENA_HOME_ENV: &str = "SERENA_HOME";

/// serena CLI flags MUR always passes. The dashboard defaults to on in
/// serena (`web_dashboard: bool = True`), so both are forced off.
const PROJECT_FLAG: &str = "--project";
const DASHBOARD_FLAGS: [(&str, &str); 2] = [
    ("--enable-web-dashboard", "false"),
    ("--open-web-dashboard", "false"),
];

/// The three MUR-owned serena locations for one agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerenaPaths {
    /// `SERENA_HOME`: `<agent_home>/serena`.
    pub home: PathBuf,
    /// `<home>/serena_config.yml`.
    pub config_file: PathBuf,
    /// `<home>/projects`.
    pub projects_dir: PathBuf,
}

/// Derive the serena paths from the agent home alone. No I/O, no
/// canonicalization: the preflight checks what is actually on disk.
pub fn serena_paths(agent_home: &Path) -> SerenaPaths {
    let home = agent_home.join(SERENA_HOME_DIR);
    SerenaPaths {
        config_file: home.join(SERENA_CONFIG_FILE),
        projects_dir: home.join(SERENA_PROJECTS_DIR),
        home,
    }
}

/// Environment for the serena child: `SERENA_HOME` and nothing else.
/// There is deliberately no profile-level `env` field (plan D1).
///
/// Values are `OsString`, not `String`, so a non-UTF-8 agent home reaches
/// serena unchanged instead of being lossily rewritten into another path.
pub fn launch_env(paths: &SerenaPaths) -> Vec<(String, OsString)> {
    vec![(
        SERENA_HOME_ENV.to_owned(),
        paths.home.clone().into_os_string(),
    )]
}

/// Arguments appended after the entry's own `args`: the fixed project and
/// both dashboard flags off. `OsString` for the same reason as
/// [`launch_env`].
pub fn launch_args(project_root: &Path) -> Vec<OsString> {
    let mut args = vec![
        OsString::from(PROJECT_FLAG),
        project_root.as_os_str().to_owned(),
    ];
    for (flag, value) in DASHBOARD_FLAGS {
        args.push(OsString::from(flag));
        args.push(OsString::from(value));
    }
    args
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_is_five_unique_tools_without_diagnostics() {
        let set: std::collections::BTreeSet<_> = SERENA_TOOL_ALLOWLIST.iter().collect();
        assert_eq!(set.len(), 5);
        assert!(!SERENA_TOOL_ALLOWLIST.contains(&"get_diagnostics_for_file"));
    }

    #[test]
    fn paths_derive_from_agent_home_only() {
        let p = serena_paths(Path::new("/h/.mur/agents/a"));
        assert_eq!(p.home, Path::new("/h/.mur/agents/a/serena"));
        assert_eq!(
            p.config_file,
            Path::new("/h/.mur/agents/a/serena/serena_config.yml")
        );
        assert_eq!(
            p.projects_dir,
            Path::new("/h/.mur/agents/a/serena/projects")
        );
    }

    #[test]
    fn env_is_serena_home_only() {
        let p = serena_paths(Path::new("/x/a"));
        assert_eq!(
            launch_env(&p),
            vec![("SERENA_HOME".to_owned(), OsString::from("/x/a/serena"))]
        );
    }

    #[test]
    fn args_fix_project_and_disable_dashboard() {
        let args = launch_args(Path::new("/repo"));
        let got: Vec<_> = args.iter().map(|a| a.to_str().unwrap()).collect();
        assert_eq!(
            got,
            [
                "--project",
                "/repo",
                "--enable-web-dashboard",
                "false",
                "--open-web-dashboard",
                "false"
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_project_path_passes_through_unchanged() {
        use std::os::unix::ffi::OsStrExt;
        let raw = std::ffi::OsStr::from_bytes(b"/repo/\xff");
        let args = launch_args(Path::new(raw));
        assert_eq!(args[1].as_bytes(), raw.as_bytes());
    }
}
