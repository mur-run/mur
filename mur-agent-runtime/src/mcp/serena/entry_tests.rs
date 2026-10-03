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
        let paths = serena_paths(&agent_home);
        let folder = paths.projects_dir.join("repo");
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(folder.join("project.yml"), "language_servers: [rust]\n").unwrap();
        let tools = SERENA_TOOL_ALLOWLIST.join(", ");
        fs::write(
            &paths.config_file,
            format!(
                "trusted_project_path_patterns: []\n\
                 project_serena_folder_location: \"{}/$projectFolderName\"\n\
                 fixed_tools: [{tools}]\n\
                 web_dashboard: false\n",
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

    fn verify(&self, entries: &[McpServerEntry]) -> Result<(), SerenaEntryError> {
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
fn project_absent_relative_or_missing_is_refused() {
    let fx = Fx::new();
    for project in [
        None,
        Some(PathBuf::from("repo")),
        Some(fx.repo.join("does-not-exist")),
    ] {
        let err = fx.verify(&[fx.serena(project)]).unwrap_err();
        assert!(matches!(err, SerenaEntryError::Project { .. }), "{err}");
        let msg = err.to_string();
        assert!(msg.contains("`code-nav`"), "{msg}");
        assert!(msg.contains("never falls back"), "{msg}");
    }
}

#[test]
fn preflight_failure_names_entry_and_check() {
    let fx = Fx::new();
    let cfg = serena_paths(&fx.agent_home).config_file;
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
    let paths = serena_paths(&fx.agent_home);
    assert_eq!(got.env, launch_env(&paths));
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
    let cfg = serena_paths(&fx.agent_home).config_file;
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
