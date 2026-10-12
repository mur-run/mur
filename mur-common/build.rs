use std::process::Command;

fn main() {
    emit_git_rerun_paths();
    let sha =
        git_output(&["rev-parse", "--short=12", "HEAD"]).unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=MUR_GIT_SHA={sha}");

    // Without rerun-if-env-changed, flipping the env vars later would keep
    // the stale consts baked into the crate — a silent attestation gap.
    println!("cargo:rerun-if-env-changed=MUR_EMBED_RELEASE_MARKER");
    println!("cargo:rerun-if-env-changed=MUR_APPLE_TEAM_ID");
    let marker = std::env::var("MUR_EMBED_RELEASE_MARKER").is_ok();
    let team_id = std::env::var("MUR_APPLE_TEAM_ID").unwrap_or_default();
    if marker && team_id.is_empty() {
        panic!("MUR_EMBED_RELEASE_MARKER=1 requires MUR_APPLE_TEAM_ID to be set");
    }
    println!(
        "cargo:rustc-env=MUR_EMBEDDED_RELEASE={}",
        if marker { "1" } else { "0" }
    );
    println!("cargo:rustc-env=MUR_APPLE_TEAM_ID={team_id}");
}

/// Re-run when this checkout's HEAD moves (a new commit changes the sha).
///
/// Paths come from `git rev-parse` instead of `../.git/...`: in a linked
/// worktree `.git` is a *file*, so a hard-coded `../.git/HEAD` never exists and
/// Cargo treats a missing rerun-if-changed path as permanently dirty — the
/// script (and every crate downstream of mur-common) rebuilt on every build.
///
/// Tracked: the per-worktree `HEAD` (branch switch / detach) and its reflog
/// `logs/HEAD` (appended on every commit, reset, rebase here). Deliberately not
/// the shared `refs/` dir: other worktrees' commits would dirty this one.
/// Only existing paths are emitted, for the same missing-path reason. Outside
/// a git checkout nothing is emitted and the sha stays "unknown".
fn emit_git_rerun_paths() {
    let Some(git_dir) = git_output(&["rev-parse", "--absolute-git-dir"]) else {
        return;
    };
    let git_dir = std::path::PathBuf::from(git_dir);
    let mut tracked = vec![git_dir.join("HEAD")];
    let reflog = git_dir.join("logs").join("HEAD");
    if reflog.is_file() {
        tracked.push(reflog);
    } else if let (Some(common), Some(branch_ref)) = (
        git_output(&["rev-parse", "--path-format=absolute", "--git-common-dir"]),
        git_output(&["symbolic-ref", "-q", "HEAD"]),
    ) {
        // Reflog disabled (core.logAllRefUpdates=false): fall back to the
        // branch's loose ref plus packed-refs.
        let common = std::path::PathBuf::from(common);
        tracked.push(common.join(branch_ref));
        tracked.push(common.join("packed-refs"));
    }
    for path in tracked.iter().filter(|p| p.exists()) {
        println!("cargo:rerun-if-changed={}", path.display());
    }
}

fn git_output(args: &[&str]) -> Option<String> {
    Command::new("git")
        .args(args)
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}
