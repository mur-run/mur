use super::*;
use std::path::PathBuf;
use std::time::Instant;

const LIMITS: AstGrepLimits = AstGrepLimits {
    max_results: 200,
    context_lines: 0,
    timeout_secs: 30,
    max_output_bytes: 1024 * 1024,
    max_match_bytes: 2 * 1024,
};

fn dir() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

/// A temp dir that may hold an executable. Sits beside the test binary
/// rather than in `$TMPDIR`, because some sandboxes (MUR's own agent seal)
/// deny exec from the temp dir; wherever the test binary runs, exec works.
fn exec_dir() -> tempfile::TempDir {
    let exe = std::env::current_exe().unwrap();
    tempfile::tempdir_in(exe.parent().unwrap()).unwrap()
}

// ---- pure ----------------------------------------------------------------

#[test]
fn truncate_utf8_respects_char_boundaries() {
    assert_eq!(truncate_utf8("abc", 3), ("abc", false));
    assert_eq!(truncate_utf8("abcd", 3), ("abc", true));
    // "é" is 2 bytes; a cut inside it backs off to the boundary.
    assert_eq!(truncate_utf8("aé", 2), ("a", true));
    assert_eq!(truncate_utf8("", 0), ("", false));
}

fn record(lines: &str) -> Value {
    json!({
        "text": "foo(1, 2)",
        "range": {
            "byteOffset": {"start": 19, "end": 28},
            "start": {"line": 0, "column": 19},
            "end": {"line": 2, "column": 28}
        },
        "file": "/r/a.rs",
        "lines": lines,
        "language": "Rust",
        "metaVariables": {
            "single": {"A": {"text": "1"}},
            "multi": {"R": [{"text": "2"}, {"text": "3"}]},
            "transformed": {}
        }
    })
}

#[test]
fn shape_match_is_one_based_with_metavars() {
    let m = shape_match(&record("fn main(){}"), 1024).unwrap();
    assert_eq!(m["file"], "/r/a.rs");
    assert_eq!(
        (m["line"].as_u64(), m["column"].as_u64()),
        (Some(1), Some(20))
    );
    assert_eq!(
        (m["end_line"].as_u64(), m["end_column"].as_u64()),
        (Some(3), Some(29))
    );
    assert_eq!(m["meta_variables"]["A"], "1");
    assert_eq!(m["meta_variables"]["R"], json!(["2", "3"]));
    assert!(m.get("truncated").is_none());
}

#[test]
fn shape_match_cuts_long_lines_and_says_so() {
    let m = shape_match(&record(&"x".repeat(5000)), 16).unwrap();
    assert_eq!(m["lines"].as_str().unwrap().len(), 16);
    assert_eq!(m["truncated"], true);
}

#[test]
fn non_match_records_are_not_matches() {
    assert!(shape_match(&json!({"hello": 1}), 16).is_none());
    assert!(shape_match(&json!({"file": "a", "range": {}}), 16).is_none());
}

#[test]
fn exit_codes_map_and_quote_stderr() {
    assert!(map_exit(Some(0), "").is_ok());
    assert!(map_exit(Some(1), "").is_ok());
    let e = map_exit(Some(2), "invalid value 'klingon'").unwrap_err();
    assert!(e.contains("exit 2") && e.contains("klingon"), "{e}");
    let e = map_exit(Some(79), "Cannot parse").unwrap_err();
    assert!(
        e.contains("79") && e.contains("did not supply") && e.contains("Cannot parse"),
        "{e}"
    );
    assert!(map_exit(Some(5), "x").unwrap_err().contains("exit 5"));
    assert!(map_exit(None, "").unwrap_err().contains("signal"));
}

// ---- fake binary (unix): the bounds, without needing ast-grep -------------

#[cfg(unix)]
fn fake_bin(d: &tempfile::TempDir, body: &str) -> PathBuf {
    use std::os::unix::fs::PermissionsExt;
    let p = d.path().join("fake-ast-grep");
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_kills_a_silent_child() {
    let d = exec_dir();
    let bin = fake_bin(&d, "exec sleep 30");
    let limits = AstGrepLimits {
        timeout_secs: 1,
        ..LIMITS
    };
    let t = Instant::now();
    let out = run(&bin, &[], d.path(), &limits).await.unwrap();
    assert!(t.elapsed().as_secs() < 10, "not killed: {:?}", t.elapsed());
    assert_eq!(out["truncated"], true);
    assert_eq!(out["truncated_by"], "timeout");
}

#[cfg(unix)]
#[tokio::test]
async fn timeout_bounds_a_child_that_closes_stdout_but_lingers() {
    let d = exec_dir();
    let bin = fake_bin(&d, "exec >/dev/null; exec sleep 30");
    let limits = AstGrepLimits {
        timeout_secs: 1,
        ..LIMITS
    };
    let t = Instant::now();
    let out = run(&bin, &[], d.path(), &limits).await.unwrap();
    assert!(
        t.elapsed().as_secs() < 10,
        "hung on wait: {:?}",
        t.elapsed()
    );
    assert_eq!(out["truncated_by"], "timeout");
}

#[cfg(unix)]
#[tokio::test]
async fn a_grandchild_holding_stderr_cannot_stall_the_tool() {
    let d = exec_dir();
    // `sleep` (not exec'd) outlives the killed shell and keeps both pipes.
    let bin = fake_bin(&d, "sleep 30");
    let limits = AstGrepLimits {
        timeout_secs: 1,
        ..LIMITS
    };
    let t = Instant::now();
    let out = run(&bin, &[], d.path(), &limits).await.unwrap();
    assert!(t.elapsed().as_secs() < 10, "stalled: {:?}", t.elapsed());
    assert_eq!(out["truncated_by"], "timeout");
    assert!(out["warnings"].to_string().contains("stderr stayed open"));
}

#[cfg(unix)]
#[tokio::test]
async fn byte_cap_kills_an_endless_writer() {
    let d = exec_dir();
    let rec = r#"{"text":"a","range":{"start":{"line":0,"column":0},"end":{"line":0,"column":1}},"file":"f"}"#;
    let bin = fake_bin(&d, &format!("while :; do echo '{rec}'; done"));
    let limits = AstGrepLimits {
        max_output_bytes: 64 * 1024,
        max_results: 100_000,
        ..LIMITS
    };
    let t = Instant::now();
    let out = run(&bin, &[], d.path(), &limits).await.unwrap();
    assert!(t.elapsed().as_secs() < 10);
    assert_eq!(out["truncated_by"], "max_output_bytes");
    let n = out["count"].as_u64().unwrap();
    assert!(
        n > 0 && n * (rec.len() as u64 + 1) <= 64 * 1024,
        "count {n}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn max_results_stops_reading() {
    let d = exec_dir();
    let rec = r#"{"text":"a","range":{"start":{"line":0,"column":0},"end":{"line":0,"column":1}},"file":"f"}"#;
    let bin = fake_bin(&d, &format!("while :; do echo '{rec}'; done"));
    let limits = AstGrepLimits {
        max_results: 3,
        ..LIMITS
    };
    let out = run(&bin, &[], d.path(), &limits).await.unwrap();
    assert_eq!(out["count"], 3);
    assert_eq!(out["truncated_by"], "max_results");
}

#[cfg(unix)]
#[tokio::test]
async fn config_exit_is_an_error_with_stderr() {
    let d = exec_dir();
    let bin = fake_bin(&d, "echo 'Cannot parse configuration' >&2; exit 79");
    let e = run(&bin, &[], d.path(), &LIMITS).await.unwrap_err();
    assert!(
        e.contains("79") && e.contains("Cannot parse configuration"),
        "{e}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn stderr_reaches_the_agent_on_success() {
    let d = exec_dir();
    let bin = fake_bin(
        &d,
        "echo 'Warning: Pattern contains an ERROR node' >&2; exit 1",
    );
    let out = run(&bin, &[], d.path(), &LIMITS).await.unwrap();
    assert_eq!(out["count"], 0);
    assert_eq!(out["truncated"], false);
    assert!(out["warnings"][0].as_str().unwrap().contains("ERROR node"));
}

// ---- live: the pinned binary, end to end through `call` --------------------

/// A mur home with the pinned binary at its real location, or `None` = skip.
fn live_home() -> Option<tempfile::TempDir> {
    let Some(bin) = std::env::var_os("MUR_AST_GREP_BIN").map(PathBuf::from) else {
        assert!(
            std::env::var_os("MUR_REQUIRE_AST_GREP").is_none(),
            "MUR_REQUIRE_AST_GREP is set but MUR_AST_GREP_BIN is not"
        );
        eprintln!("skip: MUR_AST_GREP_BIN not set; ast-grep live run not checked");
        return None;
    };
    let home = exec_dir();
    let dest = super::super::binary_path(home.path());
    std::fs::create_dir_all(dest.parent().unwrap()).unwrap();
    std::fs::copy(&bin, &dest).unwrap();
    Some(home)
}

const HOSTILE: &str = "customLanguages:\n  e:\n    libraryPath: ./x.dylib\n    extensions: [rs]\n";

fn repo_with(src: &str) -> tempfile::TempDir {
    let r = dir();
    std::fs::write(r.path().join("a.rs"), src).unwrap();
    r
}

fn args(pattern: &str, repo: &tempfile::TempDir) -> Value {
    json!({"pattern": pattern, "paths": [repo.path().to_str().unwrap()], "lang": "rust"})
}

#[tokio::test]
async fn live_hostile_sgconfig_in_repo_and_cwd_ancestor_never_loads() {
    let Some(home) = live_home() else { return };
    let repo = repo_with("fn main() { let a = 1; }\n");
    std::fs::write(repo.path().join("sgconfig.yml"), HOSTILE).unwrap();
    // Ancestors of the MUR cwd (`<home>/runtime/ast-grep`).
    std::fs::create_dir_all(home.path().join("runtime")).unwrap();
    std::fs::write(home.path().join("runtime/sgconfig.yml"), HOSTILE).unwrap();
    std::fs::write(home.path().join("sgconfig.yml"), HOSTILE).unwrap();
    let out = call(home.path(), &args("let $A = 1", &repo)).await.unwrap();
    assert_eq!(out["count"], 1, "{out}");
    assert_eq!(out["matches"][0]["meta_variables"]["A"], "a");
    assert_eq!(out["matches"][0]["line"], 1);
}

#[tokio::test]
async fn live_minified_line_is_capped_and_killed() {
    let Some(home) = live_home() else { return };
    // One 40 KB line with 10k matches: uncapped this is hundreds of MB.
    let body = "1+".repeat(10_000);
    let repo = repo_with(&format!("fn f() -> i32 {{ {body}1 }}\n"));
    let t = Instant::now();
    let out = call(home.path(), &args("1", &repo)).await.unwrap();
    assert!(t.elapsed().as_secs() < 20, "{:?}", t.elapsed());
    assert_eq!(out["truncated"], true, "{}", out["truncated_by"]);
    assert_eq!(out["truncated_by"], "max_output_bytes");
    for m in out["matches"].as_array().unwrap() {
        assert!(m["lines"].as_str().unwrap().len() <= 2 * 1024);
        assert_eq!(m["truncated"], true);
    }
}

#[tokio::test]
async fn live_malformed_pattern_returns_the_warning() {
    let Some(home) = live_home() else { return };
    let repo = repo_with("fn main() {}\n");
    let out = call(home.path(), &args("fn (", &repo)).await.unwrap();
    assert_eq!(out["count"], 0);
    let w = out["warnings"].to_string();
    assert!(w.contains("ERROR node"), "{w}");
}

#[tokio::test]
async fn live_unsupported_lang_is_an_error() {
    let Some(home) = live_home() else { return };
    let repo = repo_with("fn main() {}\n");
    let mut a = args("x", &repo);
    a["lang"] = "klingon".into();
    let e = call(home.path(), &a).await.unwrap_err();
    assert!(e.contains("exit 2") && e.contains("klingon"), "{e}");
}

#[tokio::test]
async fn missing_binary_is_a_clear_error() {
    let home = dir();
    let repo = repo_with("fn main() {}\n");
    let e = call(home.path(), &args("x", &repo)).await.unwrap_err();
    assert!(
        e.contains("not installed") && e.contains(super::super::AST_GREP_PINNED_VERSION),
        "{e}"
    );
}
