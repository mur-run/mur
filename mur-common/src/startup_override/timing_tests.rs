use super::*;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[test]
fn max_age_for_default_stop_timeout_is_680() {
    assert_eq!(OVERRIDE_ACK_WAIT_SECS, 600);
    assert_eq!(override_max_age(15), Duration::from_secs(680));
}

#[test]
fn max_age_grows_with_stop_timeout() {
    assert_eq!(
        override_max_age(45) - override_max_age(15),
        Duration::from_secs(30)
    );
}

/// The workspace root: `mur-common`'s manifest dir is one level below it.
fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("mur-common sits inside the workspace")
        .to_path_buf()
}

fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if path.is_dir() {
            if name == "target" || name == "node_modules" || name.starts_with('.') {
                continue;
            }
            rust_files(&path, out);
        } else if name.ends_with(".rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_other_override_max_age_definition() {
    // Built at runtime so this file does not count as a definition site.
    let needles: Vec<String> = [
        "OVERRIDE_MAX_AGE",
        "RESPAWN_WAIT_SECS",
        "SLOW_START_WAIT_SECS",
        "RESTART_KILL_GRACE_SECS",
    ]
    .iter()
    .map(|n| format!("const {n}"))
    .collect();

    let mut files = Vec::new();
    rust_files(&workspace_root(), &mut files);
    let home = Path::new("mur-common/src/startup_override/timing.rs");

    let mut strays = Vec::new();
    for file in &files {
        let Ok(text) = std::fs::read_to_string(file) else {
            continue;
        };
        let rel = file.strip_prefix(workspace_root()).unwrap_or(file);
        for needle in &needles {
            if text.contains(needle.as_str()) && rel != home {
                strays.push(format!("{}: {needle}", rel.display()));
            }
        }
    }
    assert!(
        strays.is_empty(),
        "restart timing constants must be defined only in {}:\n{}",
        home.display(),
        strays.join("\n")
    );
}
