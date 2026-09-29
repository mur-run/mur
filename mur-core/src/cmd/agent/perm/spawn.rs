//! `mur agent perm` process-spawn grants: binaries and directories.

use anyhow::{Result, bail};

use super::super::{load_profile_for_edit, save_profile};
use super::warn_if_running;

pub fn cmd_perm_allow_spawn(name: &str, binary: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile
        .entitlements
        .processes
        .spawn
        .allowed
        .iter()
        .any(|b| b == binary)
    {
        profile
            .entitlements
            .processes
            .spawn
            .allowed
            .push(binary.to_string());
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_deny_spawn(name: &str, binary: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if let Err(near) = remove_spawn(&mut profile.entitlements.processes.spawn.allowed, binary) {
        let mut msg =
            format!("'{binary}' is not in the spawn allowlist of '{name}'; nothing removed");
        for n in near {
            msg.push_str(&format!("\n  near match (stored value, escaped): {n:?}"));
        }
        bail!(msg);
    }
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

/// Drop `binary` from the spawn allowlist. The match is exact, so a miss is
/// an error rather than a silent no-op: an entry whose stored value differs
/// by an invisible byte (a YAML-folded newline, a doubled space) would
/// otherwise look revoked while staying granted. On a miss, returns entries
/// equal to `binary` once whitespace is collapsed, so the caller can show
/// the real stored value.
fn remove_spawn(allowed: &mut Vec<String>, binary: &str) -> Result<(), Vec<String>> {
    let before = allowed.len();
    allowed.retain(|b| b != binary);
    if allowed.len() != before {
        return Ok(());
    }
    let squash = |s: &str| s.split_whitespace().collect::<Vec<_>>().join(" ");
    let want = squash(binary);
    Err(allowed
        .iter()
        .filter(|b| squash(b) == want)
        .cloned()
        .collect())
}

/// Grant the build lane: every executable under `dir` becomes spawnable.
///
/// The binary allowlist cannot express a toolchain that compiles its own
/// executables — a Rust build execs `target/debug/build/<crate>-<hash>/
/// build-script-build`, proc-macro shims and freshly linked test binaries,
/// at paths that do not exist until the build creates them. Print what the
/// grant means rather than accepting it silently: this is a wider door than
/// naming one binary, and the operator should see that in the terminal.
pub fn cmd_perm_allow_spawn_dir(name: &str, dir: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let dirs = &mut profile.entitlements.processes.spawn.allowed_dirs;
    if !dirs.iter().any(|d| d == dir) {
        dirs.push(dir.to_string());
    }
    save_profile(&path, &mut profile)?;
    println!("build lane: '{name}' may now exec anything under {dir}");
    println!(
        "  filesystem and network entitlements still bound what that code can reach — \
         check them with `mur agent perm show {name}`"
    );
    warn_if_running(name);
    Ok(())
}

pub fn cmd_perm_deny_spawn_dir(name: &str, dir: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile
        .entitlements
        .processes
        .spawn
        .allowed_dirs
        .retain(|d| d != dir);
    save_profile(&path, &mut profile)?;
    warn_if_running(name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::remove_spawn;

    /// A deny-spawn that matches nothing must fail, and must surface the
    /// YAML-folded entry the operator actually meant.
    #[test]
    fn remove_spawn_reports_misses_and_whitespace_near_matches() {
        let folded = "/x/Google Chrome for\n Testing.app/bin".to_string();
        let mut allowed = vec!["/usr/bin/git".to_string(), folded.clone()];
        assert_eq!(remove_spawn(&mut allowed, "/usr/bin/git"), Ok(()));
        let near = remove_spawn(&mut allowed, "/x/Google Chrome for Testing.app/bin");
        assert_eq!(near, Err(vec![folded.clone()]));
        assert_eq!(remove_spawn(&mut allowed, "/nope"), Err(vec![]));
        assert_eq!(allowed, vec![folded], "a miss removes nothing");
    }
}
