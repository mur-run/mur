//! Tests for the startup gate [`verify_entries`] (code-nav 2.4) and the
//! per-spawn gate [`launch_additions`] (2.5).

use super::*;
use std::fs;

/// A passing serena setup for `<tmp>/repo` under agent home `<tmp>/agent`.
struct Fx {
    _tmp: tempfile::TempDir,
    agent_home: PathBuf,
    repo: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let agent_home = tmp.path().join("agent");
        let repo = tmp.path().join("repo");
        let paths = serena_paths_for(&agent_home, "code-nav").unwrap();
        let folder = paths.projects_dir.join("repo");
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(folder.join("project.yml"), "language_servers: [rust]\n").unwrap();
        let tools = SERENA_TOOL_ALLOWLIST.join(", ");
        fs::write(
            &paths.config_file,
            format!(
                "trusted_project_path_patterns: []\n\
                 project_serena_folder_location: '{}/$projectFolderName'\n\
                 fixed_tools: [{tools}]\n\
                 web_dashboard: false\n\
                 projects: []\n",
                paths.projects_dir.display()
            ),
        )
        .unwrap();
        Fx {
            _tmp: tmp,
            agent_home,
            repo,
        }
    }

    fn serena(&self, project: Option<PathBuf>) -> McpServerEntry {
        McpServerEntry {
            name: "code-nav".into(),
            command: "serena".into(),
            kind: Some(McpServerKind::Serena),
            project,
            ..Default::default()
        }
    }

    fn verify(
        &self,
        entries: &[McpServerEntry],
    ) -> Result<Vec<SerenaEntryError>, SerenaEntryError> {
        verify_entries(entries, &self.agent_home)
    }
}

#[test]
fn good_entry_passes() {
    let fx = Fx::new();
    fx.verify(&[fx.serena(Some(fx.repo.clone()))]).unwrap();
}

#[test]
fn entries_without_kind_are_untouched() {
    let fx = Fx::new();
    // No serena setup at all for this home would fail C1; a plain entry
    // must not even look.
    let plain = McpServerEntry {
        name: "fs".into(),
        command: "mcp-fs".into(),
        ..Default::default()
    };
    verify_entries(&[plain], &fx.agent_home.join("nowhere")).unwrap();
}

#[test]
fn project_absent_or_relative_is_refused() {
    let fx = Fx::new();
    for project in [None, Some(PathBuf::from("repo"))] {
        let err = fx.verify(&[fx.serena(project)]).unwrap_err();
        assert!(matches!(err, SerenaEntryError::Project { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("`code-nav`"), "{msg}");
        assert!(msg.contains("never falls back"), "{msg}");
    }
}

/// #1979: a deleted (or unmounted) project takes down only its own entry.
/// The agent must still start; the entry is reported, not silently dropped.
#[test]
fn missing_project_skips_the_entry_without_failing_startup() {
    let fx = Fx::new();
    let gone = fx.repo.join("does-not-exist");
    let skipped = fx.verify(&[fx.serena(Some(gone.clone()))]).unwrap();
    assert_eq!(skipped.len(), 1, "{skipped:?}");
    assert!(
        matches!(&skipped[0], SerenaEntryError::ProjectMissing { entry, .. } if entry == "code-nav"),
        "{:?}",
        skipped[0]
    );
    let msg = skipped[0].to_string();
    assert!(msg.contains(&gone.display().to_string()), "{msg}");
    assert!(msg.contains("does not exist"), "{msg}");
}

/// Skipping one entry must not exempt the others from the full gate.
#[test]
fn a_skipped_entry_does_not_hide_a_bad_sibling() {
    let fx = Fx::new();
    let gone = fx.serena(Some(fx.repo.join("does-not-exist")));
    let mut bad = fx.serena(None);
    bad.name = "second".into();
    let err = fx.verify(&[gone, bad]).unwrap_err();
    assert!(err.to_string().contains("`second`"), "{err}");
}

/// The spawn gate stays fail-closed: a skipped entry still spawns nothing.
#[test]
fn missing_project_still_refuses_to_spawn() {
    let fx = Fx::new();
    let entry = fx.serena(Some(fx.repo.join("does-not-exist")));
    let err = launch_additions(&entry, Some(&fx.agent_home)).unwrap_err();
    assert!(
        matches!(err, SerenaEntryError::ProjectMissing { .. }),
        "{err}"
    );
}

/// A path that exists but is a file is a config mistake, not a vanished
/// mount: it still refuses startup.
#[test]
fn project_that_is_a_file_is_refused() {
    let fx = Fx::new();
    let file = fx.repo.join("a.txt");
    fs::write(&file, b"x").unwrap();
    let err = fx.verify(&[fx.serena(Some(file))]).unwrap_err();
    assert!(matches!(err, SerenaEntryError::Project { .. }), "{err}");
}

#[test]
fn preflight_failure_names_entry_and_check() {
    let fx = Fx::new();
    let cfg = serena_paths_for(&fx.agent_home, "code-nav")
        .unwrap()
        .config_file;
    let text = fs::read_to_string(&cfg).unwrap();
    fs::write(
        &cfg,
        text.replace("web_dashboard: false", "web_dashboard: true"),
    )
    .unwrap();
    let err = fx.verify(&[fx.serena(Some(fx.repo.clone()))]).unwrap_err();
    assert!(
        matches!(
            &err,
            SerenaEntryError::Preflight { source, .. }
                if matches!(**source, SerenaPreflightError::C6Dashboard { .. })
        ),
        "{err}"
    );
    let msg = err.to_string();
    assert!(msg.contains("`code-nav`"), "{msg}");
    assert!(msg.contains("serena C6"), "{msg}");
}

#[test]
fn second_serena_entry_is_checked_too() {
    let fx = Fx::new();
    let ok = fx.serena(Some(fx.repo.clone()));
    let mut bad = fx.serena(None);
    bad.name = "second".into();
    let err = fx.verify(&[ok, bad]).unwrap_err();
    assert!(err.to_string().contains("`second`"), "{err}");
}

// ── spawn gate (code-nav 2.5) ─────────────────────────────────────────────

#[test]
fn launch_additions_carry_home_and_fixed_project() {
    let fx = Fx::new();
    let got = launch_additions(&fx.serena(Some(fx.repo.clone())), Some(&fx.agent_home)).unwrap();
    let paths = serena_paths_for(&fx.agent_home, "code-nav").unwrap();
    assert_eq!(got.env, launch_env(&paths));
    assert_eq!(got.env_remove, ["CLANGD_FLAGS"]);
    assert_eq!(got.args, launch_args(&fx.repo));
}

#[test]
fn launch_additions_are_empty_for_plain_entries_even_without_home() {
    let plain = McpServerEntry {
        name: "fs".into(),
        command: "mcp-fs".into(),
        ..Default::default()
    };
    assert_eq!(
        launch_additions(&plain, None).unwrap(),
        LaunchAdditions::default()
    );
}

#[test]
fn launch_additions_refuse_without_agent_home() {
    let fx = Fx::new();
    let err = launch_additions(&fx.serena(Some(fx.repo.clone())), None).unwrap_err();
    assert!(matches!(err, SerenaEntryError::NoAgentHome { .. }), "{err}");
    assert!(err.to_string().contains("`code-nav`"), "{err}");
}

/// Serena rewrites its own config: a check that passed at startup must not
/// carry over to the next spawn.
#[test]
fn launch_additions_rerun_preflight_after_config_drift() {
    let fx = Fx::new();
    let entry = fx.serena(Some(fx.repo.clone()));
    fx.verify(std::slice::from_ref(&entry)).unwrap();
    let cfg = serena_paths_for(&fx.agent_home, "code-nav")
        .unwrap()
        .config_file;
    let text = fs::read_to_string(&cfg).unwrap();
    fs::write(
        &cfg,
        text.replace(
            "trusted_project_path_patterns: []",
            "trusted_project_path_patterns: [\"**\"]",
        ),
    )
    .unwrap();
    let err = launch_additions(&entry, Some(&fx.agent_home)).unwrap_err();
    assert!(matches!(err, SerenaEntryError::Preflight { .. }), "{err}");
    assert!(err.to_string().contains("serena C3"), "{err}");
}

// ── tool allow-list (code-nav 2.6) ────────────────────────────────────────

fn fake_tools(names: &[&str]) -> Vec<crate::protocol::mcp_client::ToolInfo> {
    names
        .iter()
        .map(|n| crate::protocol::mcp_client::ToolInfo {
            name: (*n).to_owned(),
            description: String::new(),
            input_schema: serde_json::json!({"type": "object"}),
        })
        .collect()
}

/// What a fully-enabled serena would list: the five, plus write/exec tools
/// and the excluded `get_diagnostics_for_file`.
const FAKE_LIST: [&str; 10] = [
    "get_symbols_overview",
    "replace_symbol_body",
    "find_symbol",
    "execute_shell_command",
    "find_referencing_symbols",
    "get_diagnostics_for_file",
    "find_implementations",
    "create_text_file",
    "find_declaration",
    "insert_after_symbol",
];

#[test]
fn serena_registers_exactly_the_five() {
    let got = admit_tools(Some(McpServerKind::Serena), fake_tools(&FAKE_LIST));
    let mut names: Vec<_> = got.iter().map(|t| t.name.as_str()).collect();
    names.sort_unstable();
    let mut want = SERENA_TOOL_ALLOWLIST.to_vec();
    want.sort_unstable();
    assert_eq!(names, want);
}

#[test]
fn plain_entry_tools_pass_through() {
    let got = admit_tools(None, fake_tools(&FAKE_LIST));
    assert_eq!(got.len(), FAKE_LIST.len());
}

// ── per-entry SERENA_HOME (#1718) ──────────────────────────────────────

#[test]
fn default_entry_name_keeps_the_legacy_home() {
    let h = Path::new("/h/agent");
    assert_eq!(serena_paths_for(h, "serena").unwrap(), serena_paths(h));
    assert_eq!(
        serena_paths_for(h, "serena").unwrap().home,
        h.join("serena"),
        "existing single-project installs must not move"
    );
}

#[test]
fn other_entries_get_their_own_home_beside_the_legacy_one() {
    let h = Path::new("/h/agent");
    let a = serena_paths_for(h, "repo-a").unwrap();
    let b = serena_paths_for(h, "repo-b").unwrap();
    assert_eq!(a.home, h.join("serena-homes").join("repo-a"));
    assert_ne!(a.home, b.home);
    assert_ne!(a.config_file, b.config_file);
    assert_ne!(a.projects_dir, b.projects_dir);
    // never nested inside the legacy home, so no name can hit `projects/`.
    assert!(!a.home.starts_with(h.join("serena")));
}

#[test]
fn home_depends_on_the_name_only() {
    let h = Path::new("/h/agent");
    assert_eq!(
        serena_paths_for(h, "x").unwrap(),
        serena_paths_for(h, "x").unwrap()
    );
}

#[test]
fn unsafe_or_ambiguous_names_are_refused_not_sanitized() {
    let h = Path::new("/h/agent");
    let too_long = "a".repeat(65);
    for bad in [
        "",
        ".",
        "..",
        "a/b",
        "a_b/c",
        "a\\b",
        "a b",
        "a.b",
        "répo",
        // Case-only differences share one directory on case-insensitive
        // filesystems (macOS, Windows), so upper case is refused outright.
        "Repo",
        "REPO",
        "a\0b",
        too_long.as_str(),
    ] {
        let err = serena_paths_for(h, bad).unwrap_err();
        assert!(matches!(err, SerenaEntryError::EntryName { .. }), "{bad:?}");
    }
    // `a/b` and `a_b` used to be able to collapse to one tool prefix; here
    // the first is refused outright and the second is its own home.
    assert!(serena_paths_for(h, "a_b").is_ok());
}

#[test]
fn two_entries_are_verified_against_their_own_homes() {
    let fx = Fx::new();
    // Second entry has no home of its own on disk: it must fail on ITS home
    // (not borrow code-nav's), and the error names it.
    let first = fx.serena(Some(fx.repo.clone()));
    let mut second = fx.serena(Some(fx.repo.clone()));
    second.name = "other".into();
    let err = fx.verify(&[first, second]).unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("`other`"), "{msg}");
    // Compare against the platform's own rendering: Windows joins with `\`.
    let want = Path::new("serena-homes").join("other");
    assert!(msg.contains(&want.display().to_string()), "{msg}");
}

#[test]
fn launch_additions_set_serena_home_per_entry() {
    let fx = Fx::new();
    let got = launch_additions(&fx.serena(Some(fx.repo.clone())), Some(&fx.agent_home)).unwrap();
    let home = serena_paths_for(&fx.agent_home, "code-nav").unwrap().home;
    assert_eq!(
        got.env,
        vec![(SERENA_HOME_ENV.to_owned(), home.into_os_string())]
    );
}

#[test]
fn an_invalid_entry_name_refuses_to_spawn() {
    let fx = Fx::new();
    let mut e = fx.serena(Some(fx.repo.clone()));
    e.name = "a/b".into();
    let err = launch_additions(&e, Some(&fx.agent_home)).unwrap_err();
    assert!(matches!(err, SerenaEntryError::EntryName { .. }), "{err}");
}

#[test]
fn names_that_sanitize_to_another_name_are_refused() {
    // `sanitize_server` trims a trailing `_` and collapses `__`, so these
    // would share a tool prefix with `serena` / `repo_a`.
    let h = Path::new("/h/agent");
    for bad in ["serena_", "repo__a", "repo_a_", "x___"] {
        let err = serena_paths_for(h, bad).unwrap_err();
        assert!(matches!(err, SerenaEntryError::EntryName { .. }), "{bad:?}");
    }
    for ok in ["repo_a", "_a", "a-b", "a_b-c"] {
        assert!(serena_paths_for(h, ok).is_ok(), "{ok:?}");
    }
}

#[test]
fn serena_entry_colliding_with_another_servers_tool_prefix_is_refused() {
    let fx = Fx::new();
    let serena = fx.serena(Some(fx.repo.clone()));
    // A user's own server `code/nav` also sanitizes to `code_nav`; a serena
    // entry named `code_nav` would then overwrite its tools.
    let other = McpServerEntry {
        name: "code/nav".into(),
        command: "x".into(),
        ..Default::default()
    };
    let clash = McpServerEntry {
        name: "code_nav".into(),
        ..serena.clone()
    };
    let err = verify_entries(&[other, clash], &fx.agent_home).unwrap_err();
    assert!(matches!(err, SerenaEntryError::ToolPrefix { .. }), "{err}");
    // Distinct prefixes stay fine.
    assert!(verify_entries(&[serena], &fx.agent_home).is_ok());
}

#[test]
fn case_variants_can_never_resolve_to_one_directory() {
    let h = Path::new("/h/agent");
    assert!(serena_paths_for(h, "repo").is_ok());
    assert!(serena_paths_for(h, "Repo").is_err());
}

/// An agent home with only the pre-#1718 shared `serena/` home on disk.
fn legacy_only_home() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let agent_home = tmp.path().join("agent");
    let repo = tmp.path().join("repo");
    fs::create_dir_all(serena_paths(&agent_home).home).unwrap();
    fs::create_dir_all(&repo).unwrap();
    (tmp, agent_home, repo)
}

#[test]
fn a_custom_named_entry_on_a_legacy_home_gets_a_recovery_message() {
    let (_tmp, agent_home, repo) = legacy_only_home();
    let entry = McpServerEntry {
        name: "code-nav".into(),
        command: "serena".into(),
        kind: Some(McpServerKind::Serena),
        project: Some(repo),
        ..Default::default()
    };
    for err in [
        verify_entries(std::slice::from_ref(&entry), &agent_home).unwrap_err(),
        launch_additions(&entry, Some(&agent_home)).unwrap_err(),
    ] {
        assert!(matches!(err, SerenaEntryError::LegacyHome { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("mur code-nav setup"), "{msg}");
        assert!(msg.contains("--entry code-nav"), "{msg}");
        assert!(msg.contains("--agent agent"), "{msg}");
    }
}

#[test]
fn the_default_entry_on_a_legacy_home_is_not_a_migration_case() {
    let (_tmp, agent_home, repo) = legacy_only_home();
    let entry = McpServerEntry {
        name: DEFAULT_ENTRY_NAME.into(),
        command: "serena".into(),
        kind: Some(McpServerKind::Serena),
        project: Some(repo),
        ..Default::default()
    };
    // Its home IS the legacy one, so it reaches preflight (and fails there
    // on the missing config), not the recovery message.
    let err = verify_entries(&[entry], &agent_home).unwrap_err();
    assert!(matches!(err, SerenaEntryError::Preflight { .. }), "{err}");
}
