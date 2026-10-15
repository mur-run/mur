//! #1916: `setup::run` as a whole, against an isolated `MUR_HOME`. The unit
//! tests on `check_slot` cannot show that a refusal happens *before* setup
//! touches anything; these do, by comparing the whole home before and after.

use super::*;
use mur_common::agent::McpServerEntry;
use std::collections::BTreeMap;

/// Every file under `root` with its bytes, so "nothing was written" is a
/// plain equality rather than a list of paths someone remembered to check.
fn snapshot(root: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .map(|e| {
            let bytes = std::fs::read(e.path()).unwrap();
            (e.path().strip_prefix(root).unwrap().to_path_buf(), bytes)
        })
        .collect()
}

struct Home {
    _tmp: tempfile::TempDir,
    _env: mur_common::test_env::EnvGuard,
    mur_home: PathBuf,
    repo: PathBuf,
}

/// An isolated `MUR_HOME` holding agent `carol`, whose profile already has
/// the given user-owned MCP servers, plus an empty repo to point serena at.
fn home_with_servers(names: &[&str]) -> Home {
    let tmp = tempfile::tempdir().unwrap();
    let mur_home = tmp.path().join("mur-home");
    let agent_home = mur_home.join("agents").join("carol");
    std::fs::create_dir_all(&agent_home).unwrap();
    let repo = tmp.path().join("repo");
    std::fs::create_dir_all(&repo).unwrap();
    let mut env = mur_common::test_env::EnvGuard::hold();
    env.set_var("MUR_HOME", &mur_home);

    let mut profile = mur_common::AgentProfile::default_for_tests();
    profile.name = "carol".into();
    for n in names {
        profile.mcp_servers.push(McpServerEntry {
            name: (*n).into(),
            command: "/bin/true".into(),
            ..Default::default()
        });
    }
    std::fs::write(
        agent_home.join("profile.yaml"),
        serde_yaml_ng::to_string(&profile).unwrap(),
    )
    .unwrap();
    Home {
        _tmp: tmp,
        _env: env,
        mur_home,
        repo,
    }
}

fn args(h: &Home, entry: &str) -> Args {
    Args {
        agent: "carol".into(),
        project: Some(h.repo.clone()),
        entry: entry.into(),
        flags: Flags {
            with_serena: true,
            ..Default::default()
        },
    }
}

fn run_setup(h: &Home, entry: &str) -> (Result<()>, String) {
    let mut out = Vec::new();
    let r = run(
        args(h, entry),
        Consent::Given,
        &mut std::io::empty(),
        &mut out,
    );
    (r, String::from_utf8(out).unwrap())
}

/// `code/nav` and `code_nav` both become the tool prefix `mcp__code_nav__`.
/// The refusal must leave the profile, the serena home and the consent
/// manifest exactly as they were: no bytes written anywhere under `MUR_HOME`.
#[test]
fn a_shared_tool_prefix_is_refused_before_anything_is_written() {
    let h = home_with_servers(&["code/nav"]);
    let before = snapshot(&h.mur_home);

    let (r, out) = run_setup(&h, "code_nav");

    let err = format!("{:#}", r.expect_err("a prefix clash must be refused"));
    assert!(err.contains("same tool prefix"), "{err}");
    assert!(err.contains("code/nav"), "names the other server: {err}");
    assert_eq!(snapshot(&h.mur_home), before, "setup wrote before refusing");
    assert!(
        !out.contains("serena entry"),
        "refused before the plan was printed: {out}"
    );
}

/// A user's own non-serena server holding the entry's exact name is refused
/// the same way, and equally early.
#[test]
fn a_user_server_holding_the_entry_name_is_refused_before_anything_is_written() {
    let h = home_with_servers(&["serena"]);
    let before = snapshot(&h.mur_home);

    let (r, _) = run_setup(&h, "serena");

    let err = format!("{:#}", r.expect_err("the slot is taken"));
    assert!(err.contains("not kind: serena"), "{err}");
    assert_eq!(snapshot(&h.mur_home), before, "setup wrote before refusing");
}

/// A name `sanitize_server` would rewrite is refused by the path check, which
/// also runs before the profile is even read.
#[test]
fn an_entry_name_that_sanitizes_to_another_is_refused_before_anything_is_written() {
    for bad in ["serena_", "repo__a", "Repo", "a/b", ""] {
        let h = home_with_servers(&[]);
        let before = snapshot(&h.mur_home);
        let (r, _) = run_setup(&h, bad);
        assert!(r.is_err(), "`{bad}` must be refused");
        assert_eq!(snapshot(&h.mur_home), before, "`{bad}`: wrote anything");
    }
}
