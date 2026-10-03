use super::*;
use serde_json::json;
use std::ffi::OsString;

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn abs(d: &tempfile::TempDir) -> String {
    d.path().to_str().unwrap().to_owned()
}

#[test]
fn binary_path_is_versioned_under_mur_home() {
    let home = Path::new("h");
    let p = binary_path(home);
    assert!(
        p.starts_with(
            home.join("tools")
                .join("ast-grep")
                .join(AST_GREP_PINNED_VERSION)
        )
    );
}

#[test]
fn missing_binary_resolves_to_none() {
    let home = dir();
    assert_eq!(resolve_binary(home.path()), None);
}

#[test]
fn directory_at_binary_path_is_not_a_binary() {
    let home = dir();
    std::fs::create_dir_all(binary_path(home.path())).unwrap();
    assert_eq!(resolve_binary(home.path()), None);
}

#[cfg(unix)]
#[test]
fn non_executable_file_is_not_a_binary() {
    use std::os::unix::fs::PermissionsExt;
    let home = dir();
    let p = binary_path(home.path());
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, b"").unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(resolve_binary(home.path()), None);
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(resolve_binary(home.path()), Some(p));
}

#[test]
fn isolation_rewrites_a_tampered_sgconfig_to_empty() {
    let home = dir();
    let iso = ensure_isolation(home.path()).unwrap();
    std::fs::write(&iso.sgconfig, b"customLanguages: {}\n").unwrap();
    let iso = ensure_isolation(home.path()).unwrap();
    assert_eq!(std::fs::read(&iso.sgconfig).unwrap(), b"");
    assert!(iso.cwd.starts_with(home.path()));
}

#[test]
fn relative_path_is_rejected() {
    let err = check_path("src").unwrap_err();
    assert!(err.contains("absolute"), "{err}");
}

#[test]
fn missing_path_is_rejected() {
    let d = dir();
    let err = check_path(d.path().join("nope").to_str().unwrap()).unwrap_err();
    assert!(err.contains("not accessible"), "{err}");
}

#[cfg(unix)]
#[test]
fn symlinked_path_is_canonicalized() {
    let d = dir();
    let real = d.path().join("real");
    std::fs::create_dir(&real).unwrap();
    let link = d.path().join("link");
    std::os::unix::fs::symlink(&real, &link).unwrap();
    let got = check_path(link.to_str().unwrap()).unwrap();
    assert_eq!(got, std::fs::canonicalize(&real).unwrap());
}

#[test]
fn parse_requires_pattern_and_paths() {
    let d = dir();
    assert!(parse_args(&json!({ "paths": [abs(&d)] })).is_err());
    assert!(parse_args(&json!({ "pattern": "  ", "paths": [abs(&d)] })).is_err());
    assert!(parse_args(&json!({ "pattern": "x" })).is_err());
    assert!(parse_args(&json!({ "pattern": "x", "paths": [] })).is_err());
}

#[test]
fn parse_rejects_bad_fields() {
    let d = dir();
    let base = |extra: Value| {
        let mut v = json!({ "pattern": "x", "paths": [abs(&d)] });
        v.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        parse_args(&v)
    };
    assert!(base(json!({ "strictness": "loose" })).is_err());
    assert!(base(json!({ "lang": "rust; rm" })).is_err());
    assert!(base(json!({ "lang": "" })).is_err());
    assert!(base(json!({ "globs": "*.rs" })).is_err());
    assert!(base(json!({ "globs": [""] })).is_err());
    assert!(base(json!({ "max_results": -1 })).is_err());
    assert!(base(json!({ "context": "3" })).is_err());
    assert!(base(json!({ "pattern": "a\u{0}b" })).is_err());
    assert!(base(json!({ "pattern": "x".repeat(MAX_PATTERN_BYTES + 1) })).is_err());
}

#[test]
fn parse_accepts_full_call() {
    let d = dir();
    let a = parse_args(&json!({
        "pattern": "fn $N($$$A)", "paths": [abs(&d)], "lang": "rust",
        "globs": ["*.rs", "!target/**"], "strictness": "smart",
        "max_results": 50, "context": 2,
    }))
    .unwrap();
    assert_eq!(a.lang.as_deref(), Some("rust"));
    assert_eq!(a.globs.len(), 2);
    assert_eq!((a.max_results, a.context), (Some(50), Some(2)));
    assert_eq!(a.paths, vec![std::fs::canonicalize(d.path()).unwrap()]);
}

fn args_with(pattern: &str, path: PathBuf) -> SearchArgs {
    SearchArgs {
        pattern: pattern.into(),
        paths: vec![path],
        lang: Some("rust".into()),
        globs: vec!["!target/**".into()],
        strictness: Some("ast".into()),
        max_results: None,
        context: None,
    }
}

#[test]
fn argv_always_carries_config_and_ends_paths_after_separator() {
    let a = args_with("-x", PathBuf::from("/r/-dir"));
    let v = build_argv(&a, Path::new("/m/sgconfig.yml"), 0);
    let s: Vec<String> = v.iter().map(|o| o.to_string_lossy().into_owned()).collect();
    assert_eq!(s[0], "run");
    assert_eq!(s[1], "--config=/m/sgconfig.yml");
    assert!(s.contains(&"--pattern=-x".to_owned()));
    assert!(s.contains(&"--json=stream".to_owned()));
    assert!(
        !s.iter().any(|x| x.starts_with("--context")),
        "context 0 adds no flag"
    );
    let sep = s.iter().position(|x| x == "--").unwrap();
    assert_eq!(&s[sep + 1..], ["/r/-dir"]);
    // Every agent-supplied value before `--` is glued to its flag.
    assert!(s[..sep].iter().skip(1).all(|x| x.starts_with("--")));
}

#[test]
fn argv_includes_context_when_positive() {
    let a = args_with("x", PathBuf::from("/r"));
    let v = build_argv(&a, Path::new("/m/c.yml"), 3);
    assert!(v.contains(&OsString::from("--context=3")));
}

#[test]
fn schema_requires_pattern_and_paths() {
    let t = tool();
    assert_eq!(t.name, TOOL_NAME);
    assert_eq!(
        t.input_schema.required,
        Some(vec!["pattern".into(), "paths".into()])
    );
}

/// Live check of the argv against the real pinned binary. Runs when the
/// binary is reachable via `MUR_AST_GREP_BIN`; `MUR_REQUIRE_AST_GREP=1`
/// (set in CI) turns "not found" into a failure instead of a skip.
#[test]
fn argv_runs_against_pinned_binary() {
    let Some(bin) = std::env::var_os("MUR_AST_GREP_BIN").map(PathBuf::from) else {
        assert!(
            std::env::var_os("MUR_REQUIRE_AST_GREP").is_none(),
            "MUR_REQUIRE_AST_GREP is set but MUR_AST_GREP_BIN is not"
        );
        eprintln!("skip: MUR_AST_GREP_BIN not set; ast-grep live argv check not run");
        return;
    };
    let home = dir();
    let iso = ensure_isolation(home.path()).unwrap();
    let repo = dir();
    let sub = repo.path().join("-dir");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(sub.join("a.rs"), "fn main() { let a = 1; }\n").unwrap();
    // The cwd here deliberately holds a hostile sgconfig (the case where the
    // MUR-cwd layer has failed), so only `--config` stands between it and a
    // dlopen. ast-grep stops at the nearest sgconfig.yml, which is why the
    // real MUR cwd (with its own empty file) cannot be used to prove this.
    let hostile_cwd = dir();
    std::fs::write(
        hostile_cwd.path().join("sgconfig.yml"),
        "customLanguages:\n  e:\n    libraryPath: ./x.dylib\n    extensions: [rs]\n",
    )
    .unwrap();
    let mut a = args_with("let $A = 1", check_path(sub.to_str().unwrap()).unwrap());
    a.globs.clear();
    let argv = build_argv(&a, &iso.sgconfig, 0);
    let run = |argv: &[OsString]| {
        std::process::Command::new(&bin)
            .args(argv)
            .current_dir(hostile_cwd.path())
            .output()
            .unwrap()
    };

    // Red: without `--config` the fixture must load (exit 79), or this test
    // proves nothing.
    let without: Vec<OsString> = argv
        .iter()
        .filter(|x| !x.to_string_lossy().starts_with("--config="))
        .cloned()
        .collect();
    assert_eq!(
        run(&without).status.code(),
        Some(79),
        "fixture is not hostile"
    );

    let out = run(&argv);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let first: Value = serde_json::from_str(stdout.lines().next().unwrap()).unwrap();
    assert_eq!(first["text"], "let a = 1;");
}
