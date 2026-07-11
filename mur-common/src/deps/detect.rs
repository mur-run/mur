//! Cross-platform, side-effect-free detection of a `ProgramDep`.

use crate::deps::{DepStatus, DetectMethod, ProgramDep, VersionCheck};
use std::path::{Path, PathBuf};

/// Detect whether `dep` is present. Never installs; the `version` arm runs a
/// bounded subprocess only to read a version string.
pub fn detect(dep: &ProgramDep, mur_home: &Path) -> DepStatus {
    match &dep.detect {
        DetectMethod::File { file } => {
            if expand_path(file, mur_home).exists() {
                DepStatus::Present
            } else {
                DepStatus::Missing
            }
        }
        DetectMethod::Command { command } => {
            if command_on_path(command) {
                DepStatus::Present
            } else {
                DepStatus::Missing
            }
        }
        DetectMethod::Version { version } => detect_version(version),
    }
}

/// Expand `~`/`$MUR_HOME` and resolve a bare relative path against `mur_home`
/// (so `aura/lightpanda` and `~/.mur/aura/lightpanda` both work).
pub fn expand_path(raw: &str, mur_home: &Path) -> PathBuf {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix("~/")
        && let Some(home) = std::env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }
    let p = PathBuf::from(s);
    if p.is_absolute() { p } else { mur_home.join(p) }
}

/// True if `cmd` resolves to an executable on `PATH` (Windows: also tries
/// PATHEXT-style `.exe`/`.cmd`).
pub fn command_on_path(cmd: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    let exts: &[&str] = if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let candidate = dir.join(format!("{cmd}{ext}"));
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

fn detect_version(v: &VersionCheck) -> DepStatus {
    let mut parts = v.command.split_whitespace();
    let bin = match parts.next() {
        Some(b) => b,
        None => return DepStatus::Missing,
    };
    let args: Vec<&str> = parts.collect();
    let out = std::process::Command::new(bin).args(&args).output();
    let out = match out {
        Ok(o) => o,
        Err(_) => return DepStatus::Missing,
    };
    let text = String::from_utf8_lossy(&out.stdout);
    match first_semver(&text) {
        Some(found) if semver_ge(&found, &v.min) => DepStatus::Present,
        Some(found) => DepStatus::PresentWrongVersion { found },
        // present but unparseable → report as present-unknown (never auto-action)
        None => DepStatus::PresentWrongVersion {
            found: "unknown".into(),
        },
    }
}

/// First `MAJOR.MINOR.PATCH` (or `MAJOR.MINOR`) run in `s`.
fn first_semver(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let cand = &s[start..i];
            if cand.contains('.') {
                return Some(cand.trim_end_matches('.').to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

fn semver_ge(a: &str, b: &str) -> bool {
    let pa: Vec<u64> = a.split('.').filter_map(|x| x.parse().ok()).collect();
    let pb: Vec<u64> = b.split('.').filter_map(|x| x.parse().ok()).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::{DepStatus, DetectMethod, ProgramDep};
    use std::path::Path;

    fn dep(detect: DetectMethod) -> ProgramDep {
        ProgramDep {
            name: "x".into(),
            detect,
            reason: "r".into(),
            hint: None,
            registry: None,
            recipe: None,
        }
    }

    #[test]
    fn file_detect_present_and_absent() {
        let tmp = std::env::temp_dir().join(format!("murdep_{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("aura")).unwrap();
        std::fs::write(tmp.join("aura/lightpanda"), b"x").unwrap();
        // mur_home-relative via the literal "aura/lightpanda" is expanded against mur_home
        let present = dep(DetectMethod::File {
            file: "aura/lightpanda".into(),
        });
        assert_eq!(detect(&present, &tmp), DepStatus::Present);
        let absent = dep(DetectMethod::File {
            file: "aura/nope".into(),
        });
        assert_eq!(detect(&absent, &tmp), DepStatus::Missing);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn command_detect_missing_for_bogus() {
        let d = dep(DetectMethod::Command {
            command: "definitely-not-a-real-binary-xyz".into(),
        });
        assert_eq!(detect(&d, Path::new("/")), DepStatus::Missing);
    }
}
