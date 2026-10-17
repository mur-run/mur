use super::*;
use mur_agent_runtime::mcp::serena::serena_paths;

/// Same shape as serena's template (leading comments, block values at
/// column 0 and indented, every owned key present) without its text,
/// which is GPL and not vendored.
const FIXTURE: &str = "\
# leading comment
language_backend: LSP
web_dashboard: True
# comment owned by the next key
web_dashboard_open_on_launch: True
web_dashboard_trusted_hosts:
  - 127.0.0.1
  - localhost
ls_specific_settings: {}
excluded_tools: []
included_optional_tools: []
fixed_tools:
- some_tool
base_modes:
  - interactive
  - editing
default_modes:
tool_timeout: 240
project_serena_folder_location: \"$projectDir/.serena\"
trusted_project_path_patterns:
  - '**'
auth_secret:
projects: []
";

const SECRET: &str = "00000000-0000-4000-8000-000000000000";

struct Fx {
    _tmp: tempfile::TempDir,
    paths: SerenaPaths,
    project: PathBuf,
}

fn fx() -> Fx {
    let tmp = tempfile::tempdir().unwrap();
    let paths = serena_paths(&tmp.path().join("agent"));
    let project = tmp.path().join("repo");
    std::fs::create_dir_all(&project).unwrap();
    Fx {
        _tmp: tmp,
        paths,
        project,
    }
}

fn top(text: &str) -> Mapping {
    match serde_yaml_ng::from_str(text).unwrap() {
        Value::Mapping(m) => m,
        other => panic!("not a mapping: {other:?}"),
    }
}

fn owned(f: &Fx) -> Mapping {
    owned_values(&f.paths, &f.project, SECRET, ManagedLs::default()).unwrap()
}

#[test]
fn owned_keys_take_mur_values_and_the_rest_keep_serenas() {
    let f = fx();
    let out = top(&render(FIXTURE, &owned(&f)).unwrap());
    let tpl = top(FIXTURE);
    for (k, v) in &owned(&f) {
        assert_eq!(out.get(k), Some(v), "owned key {k:?}");
    }
    for (k, v) in &tpl {
        if !OWNED_KEYS.contains(&k.as_str().unwrap()) {
            assert_eq!(out.get(k), Some(v), "template key {k:?}");
        }
    }
    assert_eq!(out.len(), tpl.len(), "no key added or lost");
}

#[test]
fn each_owned_key_appears_once_at_top_level() {
    let f = fx();
    let text = render(FIXTURE, &owned(&f)).unwrap();
    for k in OWNED_KEYS {
        let n = text
            .lines()
            .filter(|l| l.starts_with(&format!("{k}:")))
            .count();
        assert_eq!(n, 1, "{k}");
    }
    // The old block children of an owned key must go with it.
    assert!(!text.contains("some_tool"));
    assert!(!text.contains("'**'"));
}

#[test]
fn template_comments_survive() {
    let f = fx();
    let text = render(FIXTURE, &owned(&f)).unwrap();
    assert!(text.contains("# leading comment"));
    assert!(text.contains("Managed by MUR"));
}

#[test]
fn template_missing_an_owned_key_is_refused() {
    let f = fx();
    let tpl = FIXTURE.replace("auth_secret:\n", "");
    let err = render(&tpl, &owned(&f)).unwrap_err().to_string();
    assert!(err.contains("auth_secret"), "{err}");
}

#[test]
fn values_meet_the_preflight_contract() {
    let f = fx();
    let o = owned(&f);
    assert_eq!(
        o.get("trusted_project_path_patterns"),
        Some(&Value::Sequence(vec![]))
    );
    assert_eq!(o.get("web_dashboard"), Some(&Value::Bool(false)));
    let folder = o["project_serena_folder_location"].as_str().unwrap();
    assert!(folder.ends_with(FOLDER_NAME_PLACEHOLDER), "{folder}");
    assert!(
        Path::new(folder).starts_with(&f.paths.projects_dir),
        "{folder}"
    );
    let cpp = &o["ls_specific_settings"][CPP_LS_ID];
    assert_eq!(cpp["ls_extra_args"][0].as_str(), Some(CLANGD_LOCKDOWN));
    assert!(cpp.get("ls_path").is_none() && cpp.get("ls_base_cmd").is_none());
    assert_eq!(o["projects"][0].as_str(), f.project.to_str());
    assert_eq!(o["auth_secret"].as_str(), Some(SECRET));
}

#[test]
fn written_config_passes_the_runtime_preflight() {
    let f = fx();
    let path = write_config(&f.paths, &f.project, FIXTURE, SECRET, ManagedLs::default()).unwrap();
    assert_eq!(path, f.paths.config_file);
    assert!(project_folder(&f.paths, &f.project).unwrap().is_dir());
    preflight(&f.paths, &f.project).unwrap();
}

#[test]
fn rewrite_replaces_the_previous_config() {
    let f = fx();
    write_config(&f.paths, &f.project, FIXTURE, SECRET, ManagedLs::default()).unwrap();
    let other = "11111111-1111-4111-8111-111111111111";
    write_config(&f.paths, &f.project, FIXTURE, other, None).unwrap();
    let text = std::fs::read_to_string(&f.paths.config_file).unwrap();
    assert_eq!(top(&text)["auth_secret"].as_str(), Some(other));
}

#[cfg(unix)]
#[test]
fn config_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    let path = write_config(&f.paths, &f.project, FIXTURE, SECRET, ManagedLs::default()).unwrap();
    let mode = std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[cfg(unix)]
#[test]
fn a_preflight_failure_is_reported_not_swallowed() {
    let f = fx();
    // The MUR folder is a symlink back into the repo: C4 must refuse.
    let folder = project_folder(&f.paths, &f.project).unwrap();
    std::fs::create_dir_all(folder.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&f.project, &folder).unwrap();
    let err = format!(
        "{:#}",
        write_config(&f.paths, &f.project, FIXTURE, SECRET, ManagedLs::default()).unwrap_err()
    );
    assert!(err.contains("preflight") && err.contains("C4"), "{err}");
}

#[test]
fn template_path_finds_the_nested_resource() {
    let tmp = tempfile::tempdir().unwrap();
    let res = tmp
        .path()
        .join("uv-tools/serena-agent/lib/python3.13/site-packages/serena/resources");
    std::fs::create_dir_all(&res).unwrap();
    std::fs::write(res.join("serena_config.template.yml"), FIXTURE).unwrap();
    assert_eq!(
        template_path(tmp.path()),
        Some(res.join("serena_config.template.yml"))
    );
    assert!(template_path(&tmp.path().join("uv-tools/none")).is_none());
}

#[test]
fn unpinned_template_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let res = tmp.path().join("serena/resources");
    std::fs::create_dir_all(&res).unwrap();
    std::fs::write(res.join("serena_config.template.yml"), FIXTURE).unwrap();
    let err = read_pinned_template(tmp.path()).unwrap_err().to_string();
    assert!(err.contains("sha256"), "{err}");
}

#[test]
fn new_auth_secret_is_a_fresh_uuid() {
    let (a, b) = (new_auth_secret(), new_auth_secret());
    assert_ne!(a, b);
    assert!(uuid::Uuid::parse_str(&a).is_ok());
}

/// The real template from a 3.3 install: set `MUR_SERENA_INSTALL_DIR` to
/// `<mur_home>/tools/serena/<pin>`. Also loads the file with serena's own
/// loader and checks it was not re-saved (#1688).
#[test]
#[ignore = "needs a pinned serena install"]
fn real_template_renders_passes_preflight_and_is_not_rewritten() {
    let dir =
        PathBuf::from(std::env::var_os("MUR_SERENA_INSTALL_DIR").expect("MUR_SERENA_INSTALL_DIR"));
    let tpl = read_pinned_template(&dir).unwrap();
    let f = fx();
    write_config(&f.paths, &f.project, &tpl, SECRET, ManagedLs::default()).unwrap();
    preflight(&f.paths, &f.project).unwrap();

    let python = serena_venv_python(&dir);
    let before = std::fs::read(&f.paths.config_file).unwrap();
    let out = std::process::Command::new(python)
        .args(["-c", "from serena.config.serena_config import SerenaConfig as C; C.from_config_file(generate_if_missing=False)"])
        .env("SERENA_HOME", &f.paths.home)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.contains("re-saving"), "{stderr}");
    assert_eq!(
        std::fs::read(&f.paths.config_file).unwrap(),
        before,
        "serena rewrote the file"
    );
}

/// The venv interpreter of a pinned serena install: the one whose shebang
/// serena's own entry point uses, not uv's bare managed CPython (which also
/// has a `bin/python` but cannot import `serena`).
fn serena_venv_python(install_dir: &Path) -> PathBuf {
    let bin = crate::cmd::code_nav::serena_install::serena_binary_path_in(install_dir);
    let shebang = std::fs::read_to_string(&bin).expect("serena entry point");
    PathBuf::from(
        shebang
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("#!"))
            .expect("serena entry point has a shebang")
            .trim(),
    )
}

/// #1917: with a real serena, each entry's own `SERENA_HOME` is what serena
/// actually reads, and `project_serena_folder_location` resolves inside that
/// home, so two entries can never navigate each other's tree. Asks serena
/// itself rather than trusting our own path arithmetic.
#[test]
#[ignore = "needs a pinned serena install"]
fn real_serena_resolves_every_entrys_paths_inside_its_own_home() {
    use mur_agent_runtime::mcp::serena::serena_paths_for;
    let dir =
        PathBuf::from(std::env::var_os("MUR_SERENA_INSTALL_DIR").expect("MUR_SERENA_INSTALL_DIR"));
    let tpl = read_pinned_template(&dir).unwrap();
    let python = serena_venv_python(&dir);
    let tmp = tempfile::tempdir().unwrap();
    let agent_home = tmp.path().join("agent");
    let project = tmp.path().join("repo");
    std::fs::create_dir_all(&project).unwrap();

    // The probe prints what serena itself resolved, as `key=value` lines.
    let probe = "import sys\n\
        from serena.config.serena_config import SerenaConfig as C, SerenaPaths as P\n\
        c = C.from_config_file(generate_if_missing=False)\n\
        print('home=' + P().serena_user_home_dir)\n\
        print('folder=' + c.get_configured_project_serena_folder(sys.argv[1]))";
    let mut seen = Vec::new();
    for entry in ["serena", "code-nav", "repo_b"] {
        let paths = serena_paths_for(&agent_home, entry).unwrap();
        write_config(&paths, &project, &tpl, SECRET, ManagedLs::default()).unwrap();
        preflight(&paths, &project).unwrap();
        let out = std::process::Command::new(&python)
            .args(["-c", probe])
            .arg(&project)
            .env("SERENA_HOME", &paths.home)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{entry}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout).into_owned();
        let get = |k: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(&format!("{k}=")))
                .unwrap_or_else(|| panic!("{entry}: no `{k}` in {text}"))
                .to_owned()
        };
        let canon = |p: &Path| std::fs::canonicalize(p).unwrap();
        assert_eq!(
            canon(Path::new(&get("home"))),
            canon(&paths.home),
            "{entry}: serena read a different SERENA_HOME"
        );
        let folder = PathBuf::from(get("folder"));
        assert!(
            folder.starts_with(&paths.projects_dir),
            "{entry}: serena put the project folder at {}, outside {}",
            folder.display(),
            paths.projects_dir.display()
        );
        seen.push(folder);
    }
    // Same repo, three entries: three distinct folders.
    seen.sort();
    seen.dedup();
    assert_eq!(seen.len(), 3, "entries shared a project folder: {seen:?}");
}

/// #1917: the `ls_path` MUR writes is what real serena turns into the language
/// server's launch command. Asks serena itself, through the same provider
/// pyright uses (`LanguageServerDependencyProviderUvx`), so a change in how
/// serena reads `ls_specific_settings.python` shows up here. With the key,
/// serena must launch exactly our binary and never fall back to `uvx`.
#[test]
#[ignore = "needs a pinned serena install"]
fn real_serena_launches_the_managed_language_server_from_ls_path() {
    let dir =
        PathBuf::from(std::env::var_os("MUR_SERENA_INSTALL_DIR").expect("MUR_SERENA_INSTALL_DIR"));
    let tpl = read_pinned_template(&dir).unwrap();
    let python = serena_venv_python(&dir);
    let mut f = fx();
    let ls = fake_ls(&mut f);

    // The provider is built exactly as `PyrightServer` builds it.
    let probe = "import json\n\
        import sys\n\
        from serena.config.serena_config import SerenaConfig as C\n\
        from solidlsp.ls_config import LanguageServerId\n\
        from solidlsp.settings import SolidLSPSettings\n\
        from solidlsp.dependency_provider import LanguageServerDependencyProviderUvx as U\n\
        c = C.from_config_file(generate_if_missing=False)\n\
        s = SolidLSPSettings(solidlsp_dir=sys.argv[1], project_data_path=sys.argv[1], \
            ls_specific_settings=c.ls_specific_settings)\n\
        u = U(s.get_ls_specific_settings(LanguageServerId.PYTHON), sys.argv[1], \
            package='pyright', entrypoint='pyright-langserver', default_version='0', \
            version_setting_key='pyright_version', extra_args=('--stdio',))\n\
        print(json.dumps(u.create_launch_command()))";
    let launch = |f: &Fx| -> Vec<String> {
        let out = std::process::Command::new(&python)
            .args(["-c", probe])
            .arg(f._tmp.path().join("scratch"))
            .env("SERENA_HOME", &f.paths.home)
            // Without `ls_path`, serena would look for uv here; nothing is on
            // PATH, so reaching for it is a visible failure, not a silent one.
            .env("PATH", "")
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
        serde_json::from_slice(&out.stdout).expect("a JSON command line")
    };

    write_config(&f.paths, &f.project, &tpl, SECRET, py(&ls)).unwrap();
    preflight(&f.paths, &f.project).unwrap();
    assert_eq!(
        launch(&f),
        vec![ls.to_str().unwrap().to_owned(), "--stdio".to_owned()],
        "serena did not launch the managed pyright from ls_path"
    );

    // Without it serena goes for uv, which is exactly what ls_path avoids
    // (and why a re-run without Python keeps the earlier path).
    write_config(&f.paths, &f.project, &tpl, SECRET, ManagedLs::default()).unwrap();
    let out = std::process::Command::new(&python)
        .args(["-c", probe])
        .arg(f._tmp.path().join("scratch"))
        .env("SERENA_HOME", &f.paths.home)
        .env("PATH", "")
        .output()
        .unwrap();
    assert!(
        !out.status.success(),
        "without ls_path serena should need uv: {}",
        String::from_utf8_lossy(&out.stdout)
    );
}

/// A `pyright-langserver` stand-in inside a tools root the test owns.
fn fake_ls(f: &mut Fx) -> PathBuf {
    let tools = f._tmp.path().join("mur").join("tools");
    let bin = tools.join("pyright/1.1.403/bin/pyright-langserver");
    std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
    std::fs::write(&bin, b"#!/bin/sh\n").unwrap();
    f.paths.tools_dir = tools;
    bin
}

#[test]
fn python_ls_path_is_written_only_when_given() {
    let mut f = fx();
    assert!(
        owned(&f)["ls_specific_settings"]
            .get(PYTHON_LS_ID)
            .is_none()
    );
    let bin = fake_ls(&mut f);
    let o = owned_values(&f.paths, &f.project, SECRET, py(&bin)).unwrap();
    let py = &o["ls_specific_settings"][PYTHON_LS_ID];
    assert_eq!(py[LS_PATH].as_str(), bin.to_str());
    assert!(py.get("ls_base_cmd").is_none());
    // The clangd lock-down is unaffected.
    assert_eq!(
        o["ls_specific_settings"][CPP_LS_ID]["ls_extra_args"][0].as_str(),
        Some(CLANGD_LOCKDOWN)
    );
}

#[test]
fn config_with_python_ls_path_passes_the_runtime_preflight() {
    let mut f = fx();
    let bin = fake_ls(&mut f);
    write_config(&f.paths, &f.project, FIXTURE, SECRET, py(&bin)).unwrap();
    preflight(&f.paths, &f.project).unwrap();
}

#[test]
fn python_ls_path_outside_the_tools_root_fails_preflight() {
    let mut f = fx();
    fake_ls(&mut f);
    let stray = f._tmp.path().join("pyright-langserver");
    std::fs::write(&stray, b"").unwrap();
    let e = write_config(&f.paths, &f.project, FIXTURE, SECRET, py(&stray)).unwrap_err();
    assert!(format!("{e:#}").contains("ls_path"), "{e:#}");
}

#[cfg(unix)]
#[test]
fn write_private_never_follows_a_planted_tmp_symlink() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let victim = t.path().join("victim");
    std::fs::write(&victim, b"keep me").unwrap();
    let path = t.path().join("serena_config.yml");
    std::os::unix::fs::symlink(&victim, path.with_extension("yml.tmp")).unwrap();
    write_private(&path, b"secret: x\n").unwrap();
    assert_eq!(std::fs::read(&victim).unwrap(), b"keep me");
    assert_eq!(std::fs::read(&path).unwrap(), b"secret: x\n");
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, PRIVATE_MODE);
}

#[cfg(unix)]
#[test]
fn write_private_replaces_a_stale_world_readable_tmp() {
    use std::os::unix::fs::PermissionsExt;
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("serena_config.yml");
    let tmp = path.with_extension("yml.tmp");
    std::fs::write(&tmp, b"old").unwrap();
    std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_private(&path, b"new").unwrap();
    let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, PRIVATE_MODE);
    assert!(!tmp.exists());
}

#[test]
fn dropped_block_spans_blank_lines_and_comments() {
    let template = "\
keep: 1
auth_secret:
  - a

# inside the dropped block
  - b

# leads the next key
next: 2
";
    let mut keys = Mapping::new();
    keys.insert("auth_secret".into(), "x".into());
    let out = render_owned(template, &["auth_secret"], &keys, "# owned\n", "fixture").unwrap();
    assert!(!out.contains("- b"), "{out}");
    assert!(out.contains("# leads the next key\nnext: 2\n"), "{out}");
    let m = top(&out);
    assert_eq!(m.get("next"), Some(&Value::from(2)));
    assert_eq!(m.get("auth_secret"), Some(&Value::from("x")));
}

#[test]
fn trailing_blank_lines_after_a_dropped_block_are_kept() {
    let template = "auth_secret: old\n\n";
    let mut keys = Mapping::new();
    keys.insert("auth_secret".into(), "x".into());
    let out = render_owned(template, &["auth_secret"], &keys, "", "fixture").unwrap();
    assert_eq!(top(&out).get("auth_secret"), Some(&Value::from("x")));
}

fn py(bin: &Path) -> ManagedLs<'_> {
    ManagedLs {
        python: Some(bin),
        php: false,
    }
}
