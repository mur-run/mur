use super::*;
use crate::mcp::serena::serena_paths;
use std::fs;

const GOOD: &str = r#"
trusted_project_path_patterns: []
project_serena_folder_location: "$SPD/$projectFolderName"
fixed_tools:
  - get_symbols_overview
  - find_symbol
  - find_referencing_symbols
  - find_implementations
  - find_declaration
excluded_tools: []
included_optional_tools: []
web_dashboard: false
ls_specific_settings: {}
"#;

/// Agent home + repo + MUR-owned project folder with a non-C++
/// `project.yml`, so C8 does not apply unless a test opts in.
struct Fx {
    _tmp: tempfile::TempDir,
    paths: SerenaPaths,
    repo: PathBuf,
    folder: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let paths = serena_paths(&tmp.path().join("agent"));
        let repo = tmp.path().join("repo");
        let folder = paths.projects_dir.join("repo");
        fs::create_dir_all(&folder).unwrap();
        fs::create_dir_all(&repo).unwrap();
        fs::write(folder.join("project.yml"), "language_servers: [rust]\n").unwrap();
        let fx = Fx {
            _tmp: tmp,
            paths,
            repo,
            folder,
        };
        fx.config(GOOD);
        fx
    }

    fn config(&self, yaml: &str) {
        let yaml = yaml.replace("$SPD", &self.paths.projects_dir.to_string_lossy());
        fs::write(&self.paths.config_file, yaml).unwrap();
    }

    /// GOOD with one fragment replaced (or removed when `to` is empty).
    fn patch(&self, from: &str, to: &str) {
        assert!(GOOD.contains(from), "fixture lacks {from:?}");
        self.config(&GOOD.replace(from, to));
    }

    fn run(&self) -> Result<(), SerenaPreflightError> {
        preflight(&self.paths, &self.repo)
    }
}

macro_rules! assert_refused {
    ($fx:expr, $variant:ident) => {{
        let err = $fx.run().unwrap_err();
        assert!(
            matches!(err, SerenaPreflightError::$variant { .. }),
            "expected {}, got {err}",
            stringify!($variant)
        );
        err
    }};
}

#[test]
fn good_fixture_passes() {
    Fx::new().run().unwrap();
}

#[test]
fn c1_missing_home() {
    let fx = Fx::new();
    fs::remove_dir_all(&fx.paths.home).unwrap();
    assert_refused!(fx, C1Home);
}

#[test]
fn c2_missing_and_unparseable_config() {
    let fx = Fx::new();
    fs::remove_file(&fx.paths.config_file).unwrap();
    assert_refused!(fx, C2Config);
    fx.config("trusted_project_path_patterns: [\n");
    assert_refused!(fx, C2Config);
}

#[test]
fn c3_absent_or_nonempty_trust() {
    let fx = Fx::new();
    fx.patch("trusted_project_path_patterns: []\n", "");
    let err = assert_refused!(fx, C3Trust);
    assert!(err.to_string().contains("[\"**\"]"), "{err}");
    fx.patch(
        "trusted_project_path_patterns: []",
        "trusted_project_path_patterns: [\"**\"]",
    );
    assert_refused!(fx, C3Trust);
}

#[test]
fn c4_folder_outside_projects_dir_unknown_placeholder_or_missing() {
    let fx = Fx::new();
    for bad in [
        "\"$projectDir/.serena\"",
        "\"$SPD/../$projectFolderName\"",
        "\"$SPD/$nope\"",
    ] {
        fx.patch("\"$SPD/$projectFolderName\"", bad);
        assert_refused!(fx, C4ProjectFolder);
    }
    fx.patch(
        "project_serena_folder_location: \"$SPD/$projectFolderName\"\n",
        "",
    );
    assert_refused!(fx, C4ProjectFolder);
    fx.config(GOOD);
    fs::remove_dir_all(&fx.folder).unwrap();
    assert_refused!(fx, C4ProjectFolder);
}

#[cfg(unix)]
#[test]
fn c4_symlink_back_into_repo() {
    let fx = Fx::new();
    fs::remove_dir_all(&fx.folder).unwrap();
    std::os::unix::fs::symlink(&fx.repo, &fx.folder).unwrap();
    assert_refused!(fx, C4ProjectFolder);
}

#[test]
fn c5_tools_drift() {
    let fx = Fx::new();
    fx.patch("  - find_declaration\n", "  - get_diagnostics_for_file\n");
    assert_refused!(fx, C5Tools);
    fx.patch(
        "  - find_declaration\n",
        "  - find_declaration\n  - execute_shell_command\n",
    );
    assert_refused!(fx, C5Tools);
    fx.patch("excluded_tools: []", "excluded_tools: [find_symbol]");
    assert_refused!(fx, C5Tools);
    fx.patch(
        "included_optional_tools: []",
        "included_optional_tools: [x]",
    );
    assert_refused!(fx, C5Tools);
}

#[test]
fn c5_order_and_null_lists_are_fine() {
    let fx = Fx::new();
    let swapped = GOOD
        .replace("  - get_symbols_overview\n", "")
        .replace(
            "  - find_declaration\n",
            "  - find_declaration\n  - get_symbols_overview\n",
        )
        .replace("excluded_tools: []", "excluded_tools:");
    fx.config(&swapped);
    fx.run().unwrap();
}

#[test]
fn c6_dashboard_absent_or_true() {
    let fx = Fx::new();
    fx.patch("web_dashboard: false\n", "");
    assert_refused!(fx, C6Dashboard);
    fx.patch("web_dashboard: false", "web_dashboard: true");
    assert_refused!(fx, C6Dashboard);
}

#[test]
fn c7_ls_exec_override() {
    let fx = Fx::new();
    for bad in [
        "ls_specific_settings: {rust: {ls_path: /x/evil}}",
        "ls_specific_settings: {python: {ls_base_cmd: [sh, -c, x]}}",
    ] {
        fx.patch("ls_specific_settings: {}", bad);
        let err = assert_refused!(fx, C7LsExec);
        assert!(err.to_string().contains("ls_specific_settings."), "{err}");
    }
}

const CPP_OK: &str = "{ls_extra_args: [--enable-config=false], compile_commands_dir: \"$CCD\"}";

/// Enable C/C++ in `project.yml` and set `ls_specific_settings.cpp`.
fn cpp(fx: &Fx, settings: Option<&str>) {
    fs::write(fx.folder.join("project.yml"), "language_servers: [cpp]\n").unwrap();
    let ccd = fx.folder.join("cc");
    let s = settings
        .unwrap_or(CPP_OK)
        .replace("$CCD", &ccd.to_string_lossy());
    fx.patch(
        "ls_specific_settings: {}",
        &format!("ls_specific_settings: {{cpp: {s}}}"),
    );
}

#[test]
fn c8_clangd_passes_when_locked_down() {
    let fx = Fx::new();
    cpp(&fx, None);
    fx.run().unwrap();
}

#[test]
fn c8_clangd_violations() {
    let fx = Fx::new();
    // default args (`--background-index`) lack --enable-config=false
    cpp(&fx, Some("{compile_commands_dir: \"$CCD\"}"));
    assert_refused!(fx, C8Clangd);
    // --query-driver present
    cpp(
        &fx,
        Some(
            "{ls_extra_args: [--enable-config=false, \"--query-driver=/usr/bin/*\"], \
             compile_commands_dir: \"$CCD\"}",
        ),
    );
    assert_refused!(fx, C8Clangd);
    // default compile_commands_dir is `<repo>/.serena`
    cpp(&fx, Some("{ls_extra_args: [--enable-config=false]}"));
    assert_refused!(fx, C8Clangd);
    // relative escape out of the repo into somewhere else
    cpp(
        &fx,
        Some("{ls_extra_args: [--enable-config=false], compile_commands_dir: ../elsewhere}"),
    );
    assert_refused!(fx, C8Clangd);
}

#[test]
fn c8_applies_when_languages_unknown() {
    let fx = Fx::new();
    fs::remove_file(fx.folder.join("project.yml")).unwrap();
    assert_refused!(fx, C8Clangd);
}

/// A hostile repo's own `.serena/project.yml` sets `ls_path`; with the MUR
/// folder missing serena would fall back to it, so C4 must refuse first.
#[test]
fn hostile_repo_serena_folder_is_refused_by_c4() {
    let fx = Fx::new();
    let repo_serena = fx.repo.join(".serena");
    fs::create_dir_all(&repo_serena).unwrap();
    fs::write(
        repo_serena.join("project.yml"),
        "language_servers: [rust]\nls_specific_settings: {rust: {ls_path: ./pwn.sh}}\n",
    )
    .unwrap();
    fs::remove_dir_all(&fx.folder).unwrap();
    assert_refused!(fx, C4ProjectFolder);
}

#[test]
fn messages_name_file_key_found_expected() {
    let fx = Fx::new();
    fx.patch("web_dashboard: false", "web_dashboard: true");
    let msg = fx.run().unwrap_err().to_string();
    assert!(msg.contains("serena_config.yml"), "{msg}");
    assert!(msg.contains("`web_dashboard`"), "{msg}");
    assert!(msg.contains("is true"), "{msg}");
    assert!(msg.contains("expected false"), "{msg}");
}

#[test]
fn substitute_matches_serena_placeholder_rules() {
    let root = Path::new("/r/proj");
    assert_eq!(
        substitute("$projectDir/x/$projectFolderName", root).unwrap(),
        Path::new("/r/proj/x/proj")
    );
    assert_eq!(substitute("/a$/b", root).unwrap(), Path::new("/a$/b"));
    assert_eq!(substitute("/a/$foo1", root).unwrap_err(), "foo1");
}
