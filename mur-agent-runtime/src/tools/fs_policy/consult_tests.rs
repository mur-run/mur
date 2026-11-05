//! Channel consult C4 (§D4, R4): with a consult scope bound, a read must pass
//! the asking agent's effective policy as well as this agent's own. With none
//! bound, both read gates are unchanged.

use super::*;
use crate::task_runner::consult_scope::{ConsultScope, sync_scope};

/// `<tmp>` canonicalized, with an answerer (`tw`) and an asker (`pm`) home.
struct Pair {
    _tmp: tempfile::TempDir,
    root: PathBuf,
    tw_home: PathBuf,
    pm_home: PathBuf,
}

fn pair() -> Pair {
    let tmp = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let tw_home = root.join("mur/agents/tw");
    let pm_home = root.join("mur/agents/pm");
    for d in [&tw_home, &pm_home] {
        std::fs::create_dir_all(d).unwrap();
    }
    Pair {
        _tmp: tmp,
        root,
        tw_home,
        pm_home,
    }
}

fn roots(ps: &[&Path]) -> Vec<String> {
    ps.iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect()
}

fn ent(read: &[&Path], deny: &[&Path]) -> FilesystemEntitlement {
    FilesystemEntitlement {
        read: roots(read),
        write: vec![],
        deny: roots(deny),
    }
}

/// A real file under `root`, canonicalized.
fn file(root: &Path, rel: &str) -> PathBuf {
    let p = root.join(rel);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(&p, "x").unwrap();
    std::fs::canonicalize(p).unwrap()
}

/// The asker's scope, built the way its own file tools are (§D3.6).
fn asker_scope(home: &Path, profile_fs: FilesystemEntitlement) -> Arc<ConsultScope> {
    let (fs, chain) = file_tool_policy(profile_fs, home);
    Arc::new(ConsultScope::new("pm", fs, chain))
}

/// The answerer's own effective policy.
fn answerer(
    home: &Path,
    profile_fs: FilesystemEntitlement,
) -> (FilesystemEntitlement, LaunchChain) {
    file_tool_policy(profile_fs, home)
}

type LaunchChain = crate::sandbox::launch_chain::LaunchChain;

#[test]
fn asker_lacks_a_path_the_answerer_has() {
    let p = pair();
    let internal = p.root.join("docs/internal");
    let target = file(&internal, "x.md");
    let (fs, chain) = answerer(&p.tw_home, ent(&[&internal], &[]));
    let scope = asker_scope(&p.pm_home, ent(&[], &[]));
    assert!(
        check_read_entitlement(&fs, &target, &chain).is_ok(),
        "the answerer's own turn reads it"
    );
    sync_scope(scope, || {
        let err = check_read_entitlement(&fs, &target, &chain).expect_err("refused in the consult");
        let msg = format!("{err}");
        assert!(msg.contains("outside this consult's read scope"), "{msg}");
        assert!(msg.contains("`pm`"), "names the asker: {msg}");
        assert_eq!(
            check_read_refusal(&fs, &target, &chain),
            Err(ReadRefusal::OutsideConsult)
        );
    });
}

#[test]
fn asker_has_a_path_the_answerer_lacks() {
    let p = pair();
    let secret = p.root.join("secret");
    let target = file(&secret, "x");
    let (fs, chain) = answerer(&p.tw_home, ent(&[], &[]));
    let scope = asker_scope(&p.pm_home, ent(&[&secret], &[]));
    sync_scope(scope, || {
        let err = check_read_entitlement(&fs, &target, &chain).expect_err("still refused");
        // The answerer's own refusal, not the consult one: the asker's grant
        // never widens what the answerer may read.
        assert!(format!("{err}").contains("path not entitled"), "{err}");
        assert_eq!(
            check_read_refusal(&fs, &target, &chain),
            Err(ReadRefusal::NoGrant)
        );
    });
}

#[test]
fn implicit_grant_on_the_asker_side() {
    let p = pair();
    // `<mur_home>/artifacts/pm`: implicit for pm's own tools, never in its profile.
    let artifacts = p.root.join("mur/artifacts/pm");
    let target = file(&artifacts, "r.md");
    let (fs, chain) = answerer(&p.tw_home, ent(&[&p.root.join("mur/artifacts")], &[]));
    let scope = asker_scope(&p.pm_home, ent(&[], &[]));
    sync_scope(scope, || {
        assert!(check_read_entitlement(&fs, &target, &chain).is_ok());
        assert_eq!(check_read_refusal(&fs, &target, &chain), Ok(()));
    });
}

#[test]
fn asker_deny_wins() {
    let p = pair();
    let proj = p.root.join("proj");
    let private = proj.join("private");
    let open = file(&proj, "open/a.md");
    let hidden = file(&private, "x");
    let (fs, chain) = answerer(&p.tw_home, ent(&[&proj], &[]));
    let scope = asker_scope(&p.pm_home, ent(&[&proj], &[&private]));
    sync_scope(scope, || {
        assert!(check_read_entitlement(&fs, &open, &chain).is_ok());
        assert!(check_read_entitlement(&fs, &hidden, &chain).is_err());
        assert_eq!(
            check_read_refusal(&fs, &hidden, &chain),
            Err(ReadRefusal::OutsideConsult)
        );
    });
}

#[test]
fn worktree_of_a_granted_checkout_for_both_sides() {
    let Some((_tmp, main, wt)) = super::tests::repo_with_worktree() else {
        eprintln!("skipping: git unavailable");
        return;
    };
    let p = pair();
    let target = std::fs::canonicalize(&wt).unwrap().join("f.txt");
    let (fs, chain) = answerer(&p.tw_home, ent(&[&main], &[]));
    // Both grant only the main checkout; the worktree hop holds for each side.
    sync_scope(asker_scope(&p.pm_home, ent(&[&main], &[])), || {
        assert!(check_read_entitlement(&fs, &target, &chain).is_ok());
    });
    // An asker without the checkout gets no hop.
    sync_scope(asker_scope(&p.pm_home, ent(&[], &[])), || {
        assert!(check_read_entitlement(&fs, &target, &chain).is_err());
    });
}

/// With no scope bound both gates are the answerer's own decision.
#[test]
fn no_scope_bound_is_unchanged() {
    let p = pair();
    let internal = p.root.join("docs/internal");
    let target = file(&internal, "x.md");
    let (fs, chain) = answerer(&p.tw_home, ent(&[&p.root], &[]));
    assert!(crate::task_runner::consult_scope::current().is_none());
    assert!(check_read_entitlement(&fs, &target, &chain).is_ok());
    assert_eq!(check_read_refusal(&fs, &target, &chain), Ok(()));
}
