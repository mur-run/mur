use super::*;
use std::os::unix::fs::PermissionsExt;

fn manifest(home: &Path, python: bool, php: bool, ts: bool) -> Manifest {
    let tools = node_ls_install::tools_dir(home);
    let mut m = Manifest::default();
    if python {
        m.pyright = Some(super::super::pyright_install::Record {
            version: "1.1.403".into(),
            exclude_newer: String::new(),
            bin: home.join("tools/pyright/bin/pyright-langserver"),
        });
    }
    if php {
        m.intelephense = Some(node_ls_install::Record::for_tools(
            &serena_ls_cmd::INTELEPHENSE,
            &tools,
        ));
    }
    if ts {
        m.typescript = Some(node_ls_install::Record::for_tools(
            &serena_ls_cmd::TYPESCRIPT,
            &tools,
        ));
    }
    m
}

#[test]
fn targets_are_the_managed_installs_launched_as_serena_would() {
    let home = Path::new("/h/.mur");
    let m = manifest(home, true, true, true);
    let langs = [Lang::Python, Lang::Php, Lang::Lua, Lang::TypeScript];
    let t = targets(&m, &langs, home);
    let tools = node_ls_install::tools_dir(home);
    let mut ts = serena_ls_cmd::canonical_base_cmd(serena_ls_cmd::TS_LS_ID, &tools).unwrap();
    ts.push("--stdio".into());
    assert_eq!(t.len(), 3, "lua is serena-fetched, not launched: {t:?}");
    assert_eq!(
        t[0].cmd,
        ["/h/.mur/tools/pyright/bin/pyright-langserver", "--stdio"]
    );
    assert_eq!(t[1].lang, Lang::Php);
    assert_eq!(t[1].cmd[0], "node");
    assert_eq!(
        t[2],
        Target {
            lang: Lang::TypeScript,
            cmd: ts
        }
    );
}

/// A language enabled for the entry but not pre-installed has nothing to
/// launch; it must not become a target pointing at a missing file.
#[test]
fn an_enabled_language_without_an_install_is_not_a_target() {
    let home = Path::new("/h/.mur");
    let t = targets(
        &manifest(home, false, false, false),
        &[Lang::Python, Lang::Php],
        home,
    );
    assert!(t.is_empty(), "{t:?}");
}

#[test]
fn the_last_parseable_stdout_line_is_the_report() {
    let out = b"some log line\n{\"status\":\"ok\"}\n";
    assert_eq!(parse_report(out, b""), Report::Ok);
}

#[test]
fn a_probe_that_died_reports_its_last_stderr_line() {
    let r = parse_report(b"", b"warning: x\nthread panicked at foo\n\n");
    assert_eq!(
        r,
        Report::Failed {
            detail: "the smoke probe died without a report: thread panicked at foo".into()
        }
    );
}

#[test]
fn nothing_to_launch_prints_nothing() {
    let mut out = Vec::new();
    verify("a", Path::new("/w"), &[], &[], &mut out).unwrap();
    assert!(out.is_empty());
}

#[test]
fn only_unmanaged_languages_succeeds_and_says_so() {
    let mut out = Vec::new();
    verify("a", Path::new("/w"), &[], &[Lang::Lua], &mut out).unwrap();
    let out = String::from_utf8(out).unwrap();
    assert!(out.contains("not launched: lua"), "{out}");
}

/// The hint names the directory the seal refuses — the server's own and its
/// `#!` interpreter's — so the user can grant exactly that.
#[test]
fn blocked_dirs_name_the_binary_and_its_shebang_interpreter() {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let py_dir = root.join("venv/bin");
    let ls_dir = root.join("ls/bin");
    std::fs::create_dir_all(&py_dir).unwrap();
    std::fs::create_dir_all(&ls_dir).unwrap();
    let py = py_dir.join("python");
    std::fs::write(&py, "").unwrap();
    let ls = ls_dir.join("pyright-langserver");
    std::fs::write(&ls, format!("#!{}\nimport sys\n", py.display())).unwrap();
    std::fs::set_permissions(&ls, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cmd = vec![ls.to_string_lossy().into_owned(), "--stdio".into()];
    let path = std::ffi::OsString::from("/usr/bin");

    let all = blocked_dirs(&cmd, &path, |_| false);
    assert_eq!(all, vec![ls_dir.clone(), py_dir.clone()]);

    let only_py = blocked_dirs(&cmd, &path, |p| p.starts_with(&ls_dir));
    assert_eq!(only_py, vec![py_dir]);

    assert!(blocked_dirs(&cmd, &path, |_| true).is_empty());
}
