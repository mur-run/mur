//! serena (LSP-backed code navigation) launched as an MCP server with
//! `kind: serena` (code-nav Phase 2).
//!
//! Everything here is derived from the agent home and the entry's fixed
//! `project:` — never from the session cwd and never from the profile's
//! free-form fields — so the spawn site, the preflight and the tool filter
//! all see the same paths. The pure preflight (C1–C9) is in [`preflight`];
//! [`verify_entries`] is the startup gate (2.4); spawn wiring is 2.5–2.6.

mod preflight;
pub mod rewrite;

pub use preflight::{SerenaPreflightError, preflight};

use mur_common::agent::{McpServerEntry, McpServerKind};
use mur_common::mcp_naming::sanitize_server;
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

/// MUR-side tool gate (plan 2.6): for a serena entry, keep only tools named
/// in [`SERENA_TOOL_ALLOWLIST`]; any other entry passes through untouched.
///
/// C5 is the serena-side gate. Both exist because the child can rewrite its
/// own config after preflight; this one holds regardless of what it lists.
pub fn admit_tools(
    kind: Option<McpServerKind>,
    tools: Vec<crate::protocol::mcp_client::ToolInfo>,
) -> Vec<crate::protocol::mcp_client::ToolInfo> {
    if kind != Some(McpServerKind::Serena) {
        return tools;
    }
    tools
        .into_iter()
        .filter(|t| SERENA_TOOL_ALLOWLIST.contains(&t.name.as_str()))
        .collect()
}

/// Directory under the agent home used as serena's `SERENA_HOME`.
pub const SERENA_HOME_DIR: &str = "serena";
/// Parent of every non-default entry's `SERENA_HOME`, beside (not inside) the
/// legacy home so an entry name can never collide with `projects/` or
/// `serena_config.yml`.
pub const SERENA_ENTRY_HOMES_DIR: &str = "serena-homes";
/// The entry name whose `SERENA_HOME` stays at the legacy `<agent_home>/serena`.
/// Existing single-project installs use it, so they need no migration.
pub const DEFAULT_ENTRY_NAME: &str = "serena";
/// Longest entry name accepted as a home directory name.
pub const MAX_ENTRY_NAME_LEN: usize = 64;
/// serena's global config file name inside `SERENA_HOME`.
pub const SERENA_CONFIG_FILE: &str = "serena_config.yml";
/// MUR-owned parent of every per-project serena folder, inside
/// `SERENA_HOME`. `project_serena_folder_location` must resolve under it,
/// so serena never falls back to the repo's own `.serena/`.
pub const SERENA_PROJECTS_DIR: &str = "projects";

/// Env var serena reads to locate its home directory.
pub const SERENA_HOME_ENV: &str = "SERENA_HOME";

/// Inherited env vars removed from the serena child. clangd prepends
/// `CLANGD_FLAGS` to its argv, so it could add `--query-driver` past C8.
pub const SERENA_ENV_REMOVE: [&str; 1] = ["CLANGD_FLAGS"];

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
    /// `<mur_home>/tools`: the only place C7 lets `ls_path` point (3.6b).
    pub tools_dir: PathBuf,
}

/// Derive the serena paths from the agent home, plus MUR's tools root
/// (resolved like the supervisor's `MUR_HOME`). No filesystem I/O, no
/// canonicalization: the preflight checks what is actually on disk.
pub fn serena_paths(agent_home: &Path) -> SerenaPaths {
    let home = agent_home.join(SERENA_HOME_DIR);
    SerenaPaths {
        config_file: home.join(SERENA_CONFIG_FILE),
        projects_dir: home.join(SERENA_PROJECTS_DIR),
        tools_dir: mur_common::home::mur_home_lossy().join(mur_common::config::MUR_TOOLS_DIR),
        home,
    }
}

/// The paths for one named entry. Each entry gets its own `SERENA_HOME`
/// because serena read-modify-writes `serena_config.yml` unlocked: two
/// processes sharing one home lose project registrations when they start
/// together (6 of 6 simultaneous starts dropped one in the #1718 probe).
///
/// The home is a pure function of the entry *name*, never of entry order,
/// so removing or reordering entries cannot move anyone's home:
/// [`DEFAULT_ENTRY_NAME`] keeps `<agent_home>/serena`; any other name maps to
/// `<agent_home>/serena-homes/<name>`. Names are accepted verbatim or refused
/// (`[a-z0-9_-]`, 1..=64 chars) — never sanitized — so two names cannot
/// collapse into one home. Upper case is refused because `Repo` and `repo`
/// are one directory on case-insensitive filesystems (macOS, Windows).
/// Names `sanitize_server` would rewrite (trailing or doubled `_`) are
/// refused too, so the name is also its own tool prefix.
pub fn serena_paths_for(
    agent_home: &Path,
    entry_name: &str,
) -> Result<SerenaPaths, SerenaEntryError> {
    if entry_name == DEFAULT_ENTRY_NAME {
        return Ok(serena_paths(agent_home));
    }
    let ok = !entry_name.is_empty()
        && entry_name.len() <= MAX_ENTRY_NAME_LEN
        && entry_name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
        // The tool prefix is `sanitize_server(name)`; a name it would rewrite
        // (`serena_`, `repo__a`) shares a prefix with its rewritten form.
        && sanitize_server(entry_name) == entry_name;
    if !ok {
        return Err(SerenaEntryError::EntryName {
            entry: entry_name.to_owned(),
        });
    }
    let home = agent_home.join(SERENA_ENTRY_HOMES_DIR).join(entry_name);
    Ok(SerenaPaths {
        config_file: home.join(SERENA_CONFIG_FILE),
        projects_dir: home.join(SERENA_PROJECTS_DIR),
        tools_dir: mur_common::home::mur_home_lossy().join(mur_common::config::MUR_TOOLS_DIR),
        home,
    })
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

/// Why a `kind: serena` entry refused agent startup. Always names the
/// entry, so a profile with several servers points at the right one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SerenaEntryError {
    #[error(
        "MCP server `{entry}` (kind: serena): `project` is {found}, expected an absolute path \
         to an existing directory; serena never falls back to the working directory"
    )]
    Project { entry: String, found: String },
    /// #1979: the configured `project` path does not exist (deleted repo,
    /// unmounted volume). Startup skips just this entry; the spawn gate
    /// still refuses it, so nothing ever runs against a missing tree.
    #[error(
        "MCP server `{entry}` (kind: serena): `project` {path} does not exist (deleted, or \
         its volume is not mounted); this server is skipped until the directory is back or \
         the entry is removed with `mur agent mcp remove`"
    )]
    ProjectMissing { entry: String, path: String },
    #[error(
        "MCP server `{entry}` (kind: serena): the sandbox policy carries no absolute agent \
         home, so `SERENA_HOME` cannot be fixed; refusing to spawn"
    )]
    NoAgentHome { entry: String },
    #[error(
        "MCP server `{entry}` (kind: serena): its tool prefix `mcp__{prefix}__` is also the \
         prefix of server `{other}`, so one would overwrite the other's tools and calls could \
         reach the wrong repository; rename one of them"
    )]
    ToolPrefix {
        entry: String,
        other: String,
        prefix: String,
    },
    #[error(
        "MCP server `{entry}` (kind: serena): the entry name must be 1-64 characters of \
         [a-z0-9_-] (lower case only) because it names the entry's own SERENA_HOME \
         directory, and `Repo` / `repo` are one directory on case-insensitive filesystems"
    )]
    EntryName { entry: String },
    #[error(
        "MCP server `{entry}` (kind: serena): its SERENA_HOME is now `{home}`, but only the \
         older shared home `{legacy}` exists (entries other than `serena` used to share it). \
         Re-run setup to create the entry's own home: \
         `mur code-nav setup --agent {agent} --with-serena --project {project} --entry {entry}`"
    )]
    LegacyHome {
        entry: String,
        agent: String,
        project: String,
        home: String,
        legacy: String,
    },
    #[error("MCP server `{entry}` (kind: serena): {source}")]
    Preflight {
        entry: String,
        #[source]
        source: Box<SerenaPreflightError>,
    },
}

/// Startup gate: every enabled `kind: serena` entry must name an absolute,
/// existing `project` directory and pass [`preflight`]. Entries without a
/// kind are untouched. The first failure wins; there is no fallback to the
/// session cwd.
///
/// #1979: one failure is entry-scoped instead — a `project` path that does
/// not exist ([`SerenaEntryError::ProjectMissing`]). That entry is logged,
/// returned in the `Ok` list, and left to the spawn gate
/// ([`launch_additions`]), which still refuses it. A deleted repo must not
/// put the whole agent into a launchd crash loop; every other check stays
/// fail-closed for the agent.
pub fn verify_entries(
    entries: &[McpServerEntry],
    agent_home: &Path,
) -> Result<Vec<SerenaEntryError>, SerenaEntryError> {
    let mut skipped = Vec::new();
    for entry in entries {
        if entry.kind != Some(McpServerKind::Serena) {
            continue;
        }
        let others = entries.iter().map(|e| e.name.as_str());
        if let Some(other) = tool_prefix_clash(&entry.name, others) {
            return Err(SerenaEntryError::ToolPrefix {
                entry: entry.name.clone(),
                other: other.to_owned(),
                prefix: sanitize_server(&entry.name),
            });
        }
        let paths = entry_paths(agent_home, entry)?;
        match verify_entry(entry, &paths) {
            Ok(_) => {}
            Err(e @ SerenaEntryError::ProjectMissing { .. }) => {
                tracing::warn!(server = %entry.name, "{e}");
                skipped.push(e);
            }
            Err(e) => return Err(e),
        }
    }
    Ok(skipped)
}

/// The name of another server whose tool prefix equals `name`'s, if any.
/// Pure over names, so setup can run it before writing anything. Tool
/// registration keys on the sanitized name, so a shared prefix means the
/// later server silently replaces the earlier one's tools.
pub fn tool_prefix_clash<'a>(
    name: &str,
    others: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    let prefix = sanitize_server(name);
    others
        .into_iter()
        .find(|o| *o != name && sanitize_server(o) == prefix)
}

/// Quote `s` for a POSIX shell line the user will paste. A repo under
/// `/Volumes/My Drive/...` would otherwise split into two arguments. Plain
/// paths stay bare so the common message reads unchanged.
fn sh_quote(s: &str) -> String {
    if !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+~@:=".contains(c))
    {
        s.to_owned()
    } else {
        format!("'{}'", s.replace('\'', r"'\''"))
    }
}

/// The entry's paths, or the recovery message when it predates #1718.
///
/// Before per-entry homes every entry shared `<agent_home>/serena`, so a
/// custom-named entry (e.g. `code-nav`) written by an older setup now points
/// at a home that was never created. When exactly that is on disk — no own
/// home, the legacy one present — say how to recreate it instead of the bare
/// C1 "not a directory". Nothing is migrated: the config embeds absolute
/// paths and a pinned auth secret, and setup already regenerates both.
fn entry_paths(agent_home: &Path, entry: &McpServerEntry) -> Result<SerenaPaths, SerenaEntryError> {
    let paths = serena_paths_for(agent_home, &entry.name)?;
    let legacy = serena_paths(agent_home).home;
    if entry.name != DEFAULT_ENTRY_NAME && !paths.home.exists() && legacy.is_dir() {
        return Err(SerenaEntryError::LegacyHome {
            entry: entry.name.clone(),
            agent: agent_home
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned()),
            project: entry.project.as_deref().map_or_else(
                || "<repo>".to_owned(),
                |p| sh_quote(&p.display().to_string()),
            ),
            home: paths.home.display().to_string(),
            legacy: legacy.display().to_string(),
        });
    }
    Ok(paths)
}

/// One entry: the `project` checks, then [`preflight`]. Returns the checked
/// project root so the caller launches serena on exactly what was verified.
fn verify_entry<'e>(
    entry: &'e McpServerEntry,
    paths: &SerenaPaths,
) -> Result<&'e Path, SerenaEntryError> {
    let project = checked_project(entry)?;
    preflight(paths, project).map_err(|e| SerenaEntryError::Preflight {
        entry: entry.name.clone(),
        source: Box::new(e),
    })?;
    Ok(project)
}

/// What a spawn changes on the child: env pairs to set, inherited env
/// vars to remove, and trailing args.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct LaunchAdditions {
    pub env: Vec<(String, OsString)>,
    pub env_remove: Vec<String>,
    pub args: Vec<OsString>,
}

/// Spawn gate (code-nav 2.5). Serena rewrites its own config while it
/// runs, so the startup check alone is stale by the next spawn: every
/// spawn re-runs [`preflight`]. Entries without a kind get empty additions
/// and are spawned exactly as before. `agent_home` is `None` when the
/// sandbox policy carries none — a serena entry then refuses rather than
/// guessing a `SERENA_HOME`.
pub fn launch_additions(
    entry: &McpServerEntry,
    agent_home: Option<&Path>,
) -> Result<LaunchAdditions, SerenaEntryError> {
    if entry.kind != Some(McpServerKind::Serena) {
        return Ok(LaunchAdditions::default());
    }
    let Some(agent_home) = agent_home else {
        return Err(SerenaEntryError::NoAgentHome {
            entry: entry.name.clone(),
        });
    };
    let paths = entry_paths(agent_home, entry)?;
    let project = verify_entry(entry, &paths)?;
    Ok(LaunchAdditions {
        env: launch_env(&paths),
        env_remove: SERENA_ENV_REMOVE.iter().map(|k| (*k).to_owned()).collect(),
        args: launch_args(project),
    })
}

/// The entry's `project`, if it is absolute and an existing directory.
fn checked_project(entry: &McpServerEntry) -> Result<&Path, SerenaEntryError> {
    let refuse = |found: String| SerenaEntryError::Project {
        entry: entry.name.clone(),
        found,
    };
    let Some(project) = entry.project.as_deref() else {
        return Err(refuse("<absent>".to_owned()));
    };
    if !project.is_absolute() {
        return Err(refuse(format!("{} (relative)", project.display())));
    }
    // Only a definite "not there" is entry-scoped. A path that cannot be
    // stat'ed (permissions) falls through to the fail-closed refusal.
    if matches!(project.try_exists(), Ok(false)) {
        return Err(SerenaEntryError::ProjectMissing {
            entry: entry.name.clone(),
            path: project.display().to_string(),
        });
    }
    if !project.is_dir() {
        return Err(refuse(format!("{} (not a directory)", project.display())));
    }
    Ok(project)
}

#[cfg(test)]
mod entry_tests;

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
        let home = Path::new("/x/a");
        let p = serena_paths(home);
        assert_eq!(
            launch_env(&p),
            vec![(
                "SERENA_HOME".to_owned(),
                home.join(SERENA_HOME_DIR).into_os_string()
            )]
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
