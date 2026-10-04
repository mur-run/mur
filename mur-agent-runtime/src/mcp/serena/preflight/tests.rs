use super::*;
use crate::mcp::serena::serena_paths;
use std::fs;

const GOOD: &str = r#"
trusted_project_path_patterns: []
project_serena_folder_location: '$SPD/$projectFolderName'
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
projects: []
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
        let mut paths = serena_paths(&tmp.path().join("agent"));
        paths.tools_dir = tmp.path().join("mur").join("tools");
        fs::create_dir_all(&paths.tools_dir).unwrap();
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

    /// `$SPD` sits inside a single-quoted YAML scalar: no escapes are
    /// processed there, so Windows `\` separators survive parsing.
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
        "'$projectDir/.serena'",
        "'$SPD/../$projectFolderName'",
        "'$SPD/$nope'",
    ] {
        fx.patch("'$SPD/$projectFolderName'", bad);
        assert_refused!(fx, C4ProjectFolder);
    }
    fx.patch(
        "project_serena_folder_location: '$SPD/$projectFolderName'\n",
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

/// A MUR-installed server: `<tools>/pyright/<pin>/bin/pyright-langserver`.
fn installed_ls(fx: &Fx) -> PathBuf {
    let bin = fx
        .paths
        .tools_dir
        .join("pyright")
        .join("1.1.403")
        .join("bin");
    fs::create_dir_all(&bin).unwrap();
    let exe = bin.join("pyright-langserver");
    fs::write(&exe, "").unwrap();
    exe
}

fn ls_path_yaml(p: &Path) -> String {
    format!(
        "ls_specific_settings: {{python: {{ls_path: '{}'}}}}",
        p.display()
    )
}

#[test]
fn c7_ls_path_under_tools_dir_passes() {
    let fx = Fx::new();
    let exe = installed_ls(&fx);
    fx.patch("ls_specific_settings: {}", &ls_path_yaml(&exe));
    fx.run().unwrap();
}

#[test]
fn c7_ls_path_outside_missing_or_relative_is_refused() {
    let fx = Fx::new();
    let outside = fx.repo.join("pwn.sh");
    fs::write(&outside, "").unwrap();
    let missing = fx.paths.tools_dir.join("pyright").join("nope");
    for p in [outside, missing] {
        fx.patch("ls_specific_settings: {}", &ls_path_yaml(&p));
        let msg = assert_refused!(fx, C7LsExec).to_string();
        assert!(msg.contains("ls_specific_settings.python.ls_path"), "{msg}");
        assert!(msg.contains("expected an absolute path under"), "{msg}");
    }
    for bad in [
        "{python: {ls_path: pyright/bin/x}}",
        "{python: {ls_path: [a]}}",
    ] {
        fx.patch(
            "ls_specific_settings: {}",
            &format!("ls_specific_settings: {bad}"),
        );
        assert_refused!(fx, C7LsExec);
    }
}

/// `..` lexically leaves the tools root; canonicalization must see it.
#[test]
fn c7_ls_path_dotdot_escape_is_refused() {
    let fx = Fx::new();
    installed_ls(&fx);
    let outside = fx.repo.join("pwn.sh");
    fs::write(&outside, "").unwrap();
    let sneaky = fx
        .paths
        .tools_dir
        .join("pyright")
        .join("..")
        .join("..")
        .join("..")
        .join("repo")
        .join("pwn.sh");
    fx.patch("ls_specific_settings: {}", &ls_path_yaml(&sneaky));
    assert_refused!(fx, C7LsExec);
}

#[cfg(unix)]
#[test]
fn c7_ls_path_symlink_out_of_tools_dir_is_refused() {
    let fx = Fx::new();
    let outside = fx.repo.join("pwn.sh");
    fs::write(&outside, "").unwrap();
    let link = fx.paths.tools_dir.join("pyright-langserver");
    std::os::unix::fs::symlink(&outside, &link).unwrap();
    fx.patch("ls_specific_settings: {}", &ls_path_yaml(&link));
    let msg = assert_refused!(fx, C7LsExec).to_string();
    assert!(msg.contains("pwn.sh"), "{msg}");
}

/// serena ignores these while C3 holds; C7 refuses them anyway.
#[test]
fn c7_checks_mur_project_files_too() {
    let fx = Fx::new();
    let exe = installed_ls(&fx);
    for file in ["project.yml", "project.local.yml"] {
        let path = fx.folder.join(file);
        let before = fs::read_to_string(&path).ok();
        fs::write(
            &path,
            "language_servers: [rust]\nls_specific_settings: {rust: {ls_base_cmd: [sh]}}\n",
        )
        .unwrap();
        let msg = assert_refused!(fx, C7LsExec).to_string();
        assert!(msg.contains(file), "{msg}");
        fs::write(
            &path,
            format!("language_servers: [rust]\n{}\n", ls_path_yaml(&exe)),
        )
        .unwrap();
        fx.run().unwrap();
        match before {
            Some(b) => fs::write(&path, b).unwrap(),
            None => fs::remove_file(&path).unwrap(),
        }
    }
}

const CPP_OK: &str = "{ls_extra_args: [--enable-config=false], compile_commands_dir: '$CCD'}";

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
    cpp(&fx, Some("{compile_commands_dir: '$CCD'}"));
    assert_refused!(fx, C8Clangd);
    // --query-driver present
    cpp(
        &fx,
        Some(
            "{ls_extra_args: [--enable-config=false, \"--query-driver=/usr/bin/*\"], \
             compile_commands_dir: '$CCD'}",
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

/// clangd/LLVM argv rules that a naive "contains" check misses: the last
/// `enable-config` wins, `-opt` equals `--opt`, `@file` expands.
#[test]
fn c8_clangd_argv_bypasses_are_refused() {
    let fx = Fx::new();
    for args in [
        "[--enable-config=false, --enable-config=true]",
        "[--enable-config=false, --enable-config]",
        "[--enable-config=false, -enable-config=1]",
        "[--enable-config=false, '@flags.rsp']",
        "[--enable-config=false, '-query-driver=/x/*']",
    ] {
        cpp(
            &fx,
            Some(&format!(
                "{{ls_extra_args: {args}, compile_commands_dir: '$CCD'}}"
            )),
        );
        assert_refused!(fx, C8Clangd);
    }
    // `ls_args` replaces the defaults, then `ls_extra_args` is appended:
    // a later `--enable-config=false` restores the lock-down.
    cpp(
        &fx,
        Some(
            "{ls_args: [--enable-config=true], ls_extra_args: [--enable-config=false], \
             compile_commands_dir: '$CCD'}",
        ),
    );
    fx.run().unwrap();
}

/// serena lowercases language names and migrates legacy keys, so neither
/// `CPP` nor `language: cpp` may skip C8.
#[test]
fn c8_language_case_and_legacy_keys() {
    let fx = Fx::new();
    for yml in [
        "language_servers: [CPP]\n",
        "languages: [Cpp]\n",
        "language: cpp\n",
    ] {
        fs::write(fx.folder.join("project.yml"), yml).unwrap();
        let msg = assert_refused!(fx, C8Clangd).to_string();
        assert!(msg.contains("lists `cpp`"), "{yml}: {msg}");
    }
}

/// `project.local.yml` is merged over `project.yml` by serena.
#[test]
fn c8_reads_project_local_override() {
    let fx = Fx::new();
    let local = fx.folder.join("project.local.yml");
    // serena's own template: comments only, changes nothing.
    fs::write(&local, "# local overrides\n").unwrap();
    fx.run().unwrap();
    fs::write(&local, "language_servers: [rust, cpp]\n").unwrap();
    assert_refused!(fx, C8Clangd);
    fs::write(&local, "language_servers: [\n").unwrap();
    assert_refused!(fx, C8Clangd);
    fs::write(&local, "ignored_paths: []\n").unwrap();
    fx.run().unwrap();
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

/// C8 on a repo whose `project.yml` is missing must say why C/C++ was
/// considered and offer the non-clangd way out, not just "C8 failed".
#[test]
fn c8_message_explains_reason_and_fix() {
    let fx = Fx::new();
    fs::remove_file(fx.folder.join("project.yml")).unwrap();
    let msg = assert_refused!(fx, C8Clangd).to_string();
    assert!(msg.contains("serena C8"), "{msg}");
    assert!(msg.contains("project.yml is missing"), "{msg}");
    assert!(msg.contains("auto-detects"), "{msg}");
    assert!(msg.contains("create "), "{msg}");
    assert!(msg.contains("`language_servers`"), "{msg}");
    assert!(msg.contains("--enable-config=false"), "{msg}");

    // Explicit `cpp` in project.yml: the fix is the clangd lock-down only.
    cpp(&fx, Some("{compile_commands_dir: '$CCD'}"));
    let msg = assert_refused!(fx, C8Clangd).to_string();
    assert!(msg.contains("C/C++ checked because"), "{msg}");
    assert!(!msg.contains("auto-detects"), "{msg}");
    assert!(
        msg.contains("ls_specific_settings.cpp.ls_extra_args"),
        "{msg}"
    );
}

#[test]
fn c9_projects_absent_or_wrong_type() {
    let fx = Fx::new();
    fx.patch("projects: []\n", "");
    let msg = assert_refused!(fx, C9Projects).to_string();
    assert!(msg.contains("serena C9"), "{msg}");
    assert!(msg.contains("serena_config.yml"), "{msg}");
    assert!(msg.contains("`projects` is <absent>"), "{msg}");
    fx.patch("projects: []", "projects: nope");
    assert_refused!(fx, C9Projects);
}

#[test]
fn c9_null_or_listed_projects_are_fine() {
    let fx = Fx::new();
    fx.patch("projects: []", "projects:");
    fx.run().unwrap();
    fx.patch("projects: []", "projects:\n  - /some/repo");
    fx.run().unwrap();
}
