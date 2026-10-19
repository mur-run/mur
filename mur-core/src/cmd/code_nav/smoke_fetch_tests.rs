use super::*;
use std::os::unix::fs::PermissionsExt;

/// A stand-in for serena's interpreter at `interpreter(dir)`, running `body`
/// as a shell script.
fn fake_python(dir: &Path, body: &str) {
    let py = interpreter(dir);
    std::fs::create_dir_all(py.parent().unwrap()).unwrap();
    std::fs::write(&py, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&py, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[test]
fn interpreter_is_the_serena_tool_env_python() {
    assert_eq!(
        interpreter(Path::new("/m/tools/serena/2.0.0.dev0")),
        Path::new("/m/tools/serena/2.0.0.dev0/uv-tools/serena-agent/bin/python")
    );
}

#[test]
fn parse_maps_each_language_to_its_argv_or_reason() {
    let out = "noise\n{\"lua\": {\"ok\": [\"/h/lua-language-server\"]}, \
               \"cpp\": {\"err\": \"FileNotFoundError: no clangd\"}}\n";
    assert_eq!(
        parse(out, &[Lang::Lua, Lang::Cpp, Lang::Java]),
        vec![
            Fetched::Ready(Target {
                lang: Lang::Lua,
                cmd: vec!["/h/lua-language-server".into()],
            }),
            Fetched::Failed {
                lang: Lang::Cpp,
                detail: "FileNotFoundError: no clangd".into(),
            },
            Fetched::Failed {
                lang: Lang::Java,
                detail: "serena's fetch did not report it".into(),
            },
        ]
    );
}

#[test]
fn parse_without_a_report_fails_every_language() {
    let got = parse("Traceback ...\n", &[Lang::Lua]);
    assert_eq!(
        got,
        vec![Fetched::Failed {
            lang: Lang::Lua,
            detail: "serena's fetch printed no report".into(),
        }]
    );
}

#[test]
fn empty_argv_is_not_a_target() {
    let got = parse("{\"lua\": {\"ok\": []}}", &[Lang::Lua]);
    assert!(matches!(got[..], [Fetched::Failed { .. }]), "{got:?}");
}

/// The script runs with the entry's SERENA_HOME, gets the languages by
/// their `--lsp` names, and runs outside the project (serena writes a
/// `cache/` into its root).
#[test]
fn fetch_runs_serena_python_with_the_entry_home() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("serena");
    let home = tmp.path().join("serena-homes/repo");
    fake_python(
        &dir,
        r#"printf '{"lua": {"ok": ["%s/bin/lua", "%s"]}}\n' "$SERENA_HOME" "$3""#,
    );
    let got = fetch(&dir, &home, &[Lang::Lua]);
    assert_eq!(
        got,
        vec![Fetched::Ready(Target {
            lang: Lang::Lua,
            cmd: vec![format!("{}/bin/lua", home.display()), "lua".into()],
        })]
    );
}

#[test]
fn a_script_that_dies_quotes_its_stderr() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().join("serena");
    fake_python(&dir, "echo 'ModuleNotFoundError: solidlsp' >&2; exit 1");
    let got = fetch(&dir, tmp.path(), &[Lang::Lua, Lang::Cpp]);
    assert_eq!(got.len(), 2);
    for f in got {
        let Fetched::Failed { detail, .. } = f else {
            panic!("expected a failure");
        };
        assert!(detail.contains("ModuleNotFoundError: solidlsp"), "{detail}");
    }
}

#[test]
fn a_missing_interpreter_fails_without_panicking() {
    let tmp = tempfile::tempdir().unwrap();
    let got = fetch(&tmp.path().join("absent"), tmp.path(), &[Lang::Lua]);
    assert!(
        matches!(&got[..], [Fetched::Failed { detail, .. }] if detail.contains("serena's interpreter")),
        "{got:?}"
    );
}

#[test]
fn nothing_to_fetch_runs_nothing() {
    assert!(fetch(Path::new("/absent"), Path::new("/absent"), &[]).is_empty());
}
