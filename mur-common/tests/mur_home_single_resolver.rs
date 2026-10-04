//! Guard for #1696: MUR's data root is resolved in exactly one place,
//! `mur_common::home`. A call site that joins `.mur` onto a home directory by
//! hand ignores `MUR_HOME` and silently splits the user's store between
//! `$MUR_HOME` and `~/.mur`. This scans every workspace crate's `src/` and
//! fails on any new such join.

use std::path::{Path, PathBuf};

/// Crates that cannot depend on `mur-common` and keep a local copy of the
/// same rule (documented at the copy).
const LOCAL_COPY_ALLOWED: &[&str] = &["mur-browser/src/paths.rs", "mur-agent-launcher/src/main.rs"];

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("mur-common has a parent dir")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "tests" || name == "target" {
                continue;
            }
            rust_files(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs") {
            out.push(p);
        }
    }
}

/// True when `line` joins `.mur` (or a `.mur/...` subpath) as a path segment.
fn joins_dot_mur(line: &str) -> bool {
    line.contains(r#"join(".mur")"#) || line.contains(r#"join(".mur/"#)
}

#[test]
fn no_hand_rolled_mur_home() {
    let root = workspace_root();
    let mut offenders = Vec::new();
    let Ok(rd) = std::fs::read_dir(&root) else {
        panic!("cannot read workspace root {}", root.display());
    };
    for e in rd.flatten() {
        let src = e.path().join("src");
        let is_mur_crate = e
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with("mur-"));
        if !is_mur_crate || !src.is_dir() {
            continue;
        }
        let mut files = Vec::new();
        rust_files(&src, &mut files);
        for f in files {
            let rel = f
                .strip_prefix(&root)
                .unwrap_or(&f)
                .to_string_lossy()
                .replace('\\', "/");
            if LOCAL_COPY_ALLOWED.contains(&rel.as_str())
                || rel.ends_with("_tests.rs")
                || rel.ends_with("/tests.rs")
            {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&f) else {
                continue;
            };
            // Windows checkouts may carry CRLF; normalise so the test-module
            // boundary below matches on every platform.
            let text = text.replace("\r\n", "\n");
            // Unit tests live in a trailing `#[cfg(test)] mod tests` and build
            // fixture trees under a tempdir; stop scanning there. Split on the
            // module, not the bare attribute, so production code that follows
            // a stray `#[cfg(test)]` item is still checked.
            let body = text.split("#[cfg(test)]\nmod tests").next().unwrap_or("");
            for (i, line) in body.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("//") {
                    continue;
                }
                if joins_dot_mur(line) {
                    offenders.push(format!("{rel}:{}: {}", i + 1, line.trim()));
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "resolve MUR's data root with `mur_common::home` (honours MUR_HOME), \
         not by joining `.mur` onto a home dir (#1696):\n{}",
        offenders.join("\n")
    );
}

#[test]
fn test_module_boundary_survives_crlf() {
    let text = "fn a() {}\r\n#[cfg(test)]\r\nmod tests {\r\n    x.join(\".mur\")\r\n}\r\n"
        .replace("\r\n", "\n");
    let body = text.split("#[cfg(test)]\nmod tests").next().unwrap_or("");
    assert!(
        !joins_dot_mur(body),
        "test module must be excluded: {body:?}"
    );
}

#[test]
fn detector_matches_the_bug_shape() {
    assert!(joins_dot_mur(r#"home.join(".mur").join("index")"#));
    assert!(joins_dot_mur(r#"home.join(".mur/inbox")"#));
    assert!(!joins_dot_mur(r#"root.join(".mur-versions.yaml")"#));
    assert!(!joins_dot_mur(r#"mur_home.join("index")"#));
}
