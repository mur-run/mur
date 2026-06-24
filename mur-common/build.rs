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
}
