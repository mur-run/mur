use std::process::Command;

fn main() {
    // Re-run when HEAD moves (a new commit changes the sha).
    // Note: in a git *worktree* `.git` is a file, not a directory, so these
    // paths may not exist — Cargo degrades gracefully (notify-only signal; last
    // known SHORT_SHA / "unknown" is reused, which is acceptable).
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-changed=../.git/refs");
    let sha = Command::new("git")
        .args(["rev-parse", "--short=12", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".to_string());
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
