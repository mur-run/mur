use super::ensure_git_hook;
use std::fs;
use std::sync::atomic::{AtomicUsize, Ordering};

static HOOK_TEST_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// Build a temp dir that looks like a git repo (just needs `.git/hooks`).
fn temp_repo() -> std::path::PathBuf {
    let n = HOOK_TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let base = std::env::temp_dir().join(format!("mur-hook-test-{}-{}", std::process::id(), n));
    fs::create_dir_all(base.join(".git").join("hooks")).unwrap();
    base
}

#[test]
fn creates_hook_with_shebang_when_absent() {
    let repo = temp_repo();
    let installed = ensure_git_hook(&repo, true).unwrap();
    assert!(installed, "should report it installed the hook");

    let hook = repo.join(".git/hooks/post-commit");
    let body = fs::read_to_string(&hook).unwrap();
    assert!(body.starts_with("#!/bin/sh"), "must start with shebang");
    assert!(body.contains("# mur auto-index"), "must contain marker");
    assert!(body.contains("project index"), "must run project index");

    fs::remove_dir_all(&repo).ok();
}

#[test]
fn is_idempotent_on_second_call() {
    let repo = temp_repo();
    assert!(ensure_git_hook(&repo, true).unwrap());
    // Second call must be a no-op (marker already present).
    let installed_again = ensure_git_hook(&repo, true).unwrap();
    assert!(!installed_again, "second call should return false");

    let body = fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();
    assert_eq!(
        body.matches("# mur auto-index").count(),
        1,
        "marker must appear exactly once"
    );

    fs::remove_dir_all(&repo).ok();
}

#[test]
fn appends_to_existing_hook_without_clobbering() {
    let repo = temp_repo();
    let hook = repo.join(".git/hooks/post-commit");
    fs::write(&hook, "#!/bin/sh\necho existing-user-hook\n").unwrap();

    let installed = ensure_git_hook(&repo, true).unwrap();
    assert!(installed);

    let body = fs::read_to_string(&hook).unwrap();
    assert!(
        body.contains("echo existing-user-hook"),
        "must preserve the pre-existing hook content"
    );
    assert!(
        body.contains("# mur auto-index"),
        "must append marker block"
    );

    fs::remove_dir_all(&repo).ok();
}

#[test]
fn hook_carries_no_path_and_resolves_main_repo() {
    let repo = temp_repo();
    ensure_git_hook(&repo, true).unwrap();
    let body = fs::read_to_string(repo.join(".git/hooks/post-commit")).unwrap();
    assert!(
        body.contains("--main-repo"),
        "must let mur resolve the repo"
    );
    assert!(
        !body.contains(&repo.display().to_string()),
        "must not bake the repo path into the hook"
    );
    assert!(
        body.contains("# hook-version: 2"),
        "must stamp the block version"
    );
    fs::remove_dir_all(&repo).ok();
}

#[test]
fn upgrades_legacy_hardcoded_block_in_place() {
    let repo = temp_repo();
    let hook = repo.join(".git/hooks/post-commit");
    // What older mur wrote: the absolute path baked in, no version line.
    let legacy = format!(
        "#!/bin/sh\necho before\n\n# mur auto-index\nMUR_BIN=\"$(command -v mur || true)\"\nif [ -n \"$MUR_BIN\" ]; then\n  \"$MUR_BIN\" project index --path \"{}\" --quiet --background\nfi\necho after\n",
        repo.display()
    );
    fs::write(&hook, &legacy).unwrap();

    assert!(
        ensure_git_hook(&repo, true).unwrap(),
        "a legacy block must be reported as changed"
    );
    let body = fs::read_to_string(&hook).unwrap();
    assert!(
        body.contains("echo before") && body.contains("echo after"),
        "user lines around the block must survive"
    );
    assert!(!body.contains("--path"), "hardcoded path must be gone");
    assert!(body.contains("--main-repo"));
    assert_eq!(
        body.matches("# mur auto-index").count(),
        1,
        "exactly one block"
    );
    // Now current: a further call must be a no-op.
    assert!(!ensure_git_hook(&repo, true).unwrap());

    fs::remove_dir_all(&repo).ok();
}

#[test]
fn returns_false_when_not_a_git_repo() {
    let base = std::env::temp_dir().join(format!("mur-hook-nogit-{}", std::process::id()));
    fs::create_dir_all(&base).unwrap();
    // No .git/hooks dir → ensure_git_hook returns Ok(false).
    assert!(!ensure_git_hook(&base, true).unwrap());
    fs::remove_dir_all(&base).ok();
}

/// A real `git init` repo (the hooks-dir resolution asks git itself).
fn real_repo(tag: &str) -> std::path::PathBuf {
    let n = HOOK_TEST_COUNTER.fetch_add(1, Ordering::SeqCst);
    let base =
        std::env::temp_dir().join(format!("mur-hook-real-{tag}-{}-{}", std::process::id(), n));
    fs::create_dir_all(&base).unwrap();
    git(&base, &["init", "-q"]);
    base
}

fn git(dir: &std::path::Path, args: &[&str]) {
    let ok = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .status()
        .unwrap()
        .success();
    assert!(ok, "git {args:?} failed");
}

/// #1672: with `core.hooksPath` set to a tracked in-tree dir (husky-style),
/// git never reads `.git/hooks`. The hook must not land there, and the
/// versioned dir must not be rewritten behind the user's back.
#[test]
fn honours_core_hooks_path_and_never_writes_an_unread_dir() {
    let repo = real_repo("hookspath");
    fs::create_dir_all(repo.join(".husky")).unwrap();
    git(&repo, &["config", "core.hooksPath", ".husky"]);

    assert!(
        !ensure_git_hook(&repo, true).unwrap(),
        "an in-tree hooks dir must be reported, not silently written"
    );
    assert!(
        !repo.join(".git/hooks/post-commit").exists(),
        "must not write to .git/hooks, which git does not read here"
    );
    assert!(
        !repo.join(".husky/post-commit").exists(),
        "must not modify a versioned hooks dir"
    );
    fs::remove_dir_all(&repo).ok();
}

/// #1672: in a linked worktree `.git` is a file; the hook belongs in the
/// common git dir's hooks, which is where git looks.
#[test]
fn installs_into_the_common_hooks_dir_from_a_linked_worktree() {
    let repo = real_repo("wt-main");
    git(
        &repo,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@t",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    let wt = repo.with_file_name(format!(
        "{}-wt",
        repo.file_name().unwrap().to_string_lossy()
    ));
    git(&repo, &["worktree", "add", "-q", wt.to_str().unwrap()]);
    assert!(wt.join(".git").is_file());

    assert!(ensure_git_hook(&wt, true).unwrap(), "must install");
    assert!(
        fs::read_to_string(repo.join(".git/hooks/post-commit"))
            .unwrap()
            .contains("# mur auto-index"),
        "hook must land in the common hooks dir git actually runs"
    );
    fs::remove_dir_all(&wt).ok();
    fs::remove_dir_all(&repo).ok();
}
