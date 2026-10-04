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
    owned_values(&f.paths, &f.project, SECRET).unwrap()
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
    let path = write_config(&f.paths, &f.project, FIXTURE, SECRET).unwrap();
    assert_eq!(path, f.paths.config_file);
    assert!(project_folder(&f.paths, &f.project).unwrap().is_dir());
    preflight(&f.paths, &f.project).unwrap();
}

#[test]
fn rewrite_replaces_the_previous_config() {
    let f = fx();
    write_config(&f.paths, &f.project, FIXTURE, SECRET).unwrap();
    let other = "11111111-1111-4111-8111-111111111111";
    write_config(&f.paths, &f.project, FIXTURE, other).unwrap();
    let text = std::fs::read_to_string(&f.paths.config_file).unwrap();
    assert_eq!(top(&text)["auth_secret"].as_str(), Some(other));
}

#[cfg(unix)]
#[test]
fn config_is_owner_only() {
    use std::os::unix::fs::PermissionsExt;
    let f = fx();
    let path = write_config(&f.paths, &f.project, FIXTURE, SECRET).unwrap();
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
        write_config(&f.paths, &f.project, FIXTURE, SECRET).unwrap_err()
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
    write_config(&f.paths, &f.project, &tpl, SECRET).unwrap();
    preflight(&f.paths, &f.project).unwrap();

    let python = walkdir::WalkDir::new(&dir)
        .max_depth(4)
        .into_iter()
        .filter_map(|e| e.ok())
        .map(|e| e.into_path())
        .find(|p| p.ends_with("bin/python"))
        .expect("serena venv python");
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
