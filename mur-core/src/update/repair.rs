//! Runtime self-repair for `mur update` when the CLI is already current.
//!
//! A release `mur` refuses to launch a `mur-agent-runtime` that is not signed
//! by MUR's Developer ID (`mur_common::binary_attestation`). Before this
//! module, `mur update` only compared the CLI's own version, so a release CLI
//! sitting next to a swapped (e.g. locally built, ad-hoc signed) runtime
//! answered "Already up to date" and left every agent launch failing. Now the
//! up-to-date path verifies the installed runtimes and, if any fails,
//! restores them from the archive of the CLI's OWN version — not the latest,
//! so a pre-release CLI never gets a runtime from a different release.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::release;

/// File name of the agent runtime inside the release archive and on disk.
pub const RUNTIME_BIN: &str = "mur-agent-runtime";

/// True when runtime attestation is enforced for this build — the only case
/// where a bad runtime signature breaks agent launches, and so the only case
/// worth checking or repairing.
pub fn enforced() -> bool {
    mur_common::binary_attestation::IS_EMBEDDED_RELEASE && cfg!(target_os = "macos")
}

/// The installed runtimes a release `mur` may launch: the sibling next to the
/// real `mur` executable and the launchd copy in `~/.local/bin`. Only existing
/// files, deduplicated by their real path (the two are often the same file).
pub fn installed_runtimes() -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(exe) = std::env::current_exe()
        && let Ok(real) = exe.canonicalize()
        && let Some(dir) = real.parent()
    {
        candidates.push(dir.join(RUNTIME_BIN));
    }
    if let Some(home) = dirs::home_dir() {
        candidates.push(home.join(".local/bin").join(RUNTIME_BIN));
    }
    dedup_existing(candidates)
}

fn dedup_existing(candidates: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = Vec::new();
    let mut out = Vec::new();
    for p in candidates {
        let Ok(real) = p.canonicalize() else { continue };
        if real.is_file() && !seen.contains(&real) {
            seen.push(real);
            out.push(p);
        }
    }
    out
}

/// Every path whose signature check fails, paired with the reason.
pub fn find_invalid<F>(paths: &[PathBuf], verify: F) -> Vec<(PathBuf, String)>
where
    F: Fn(&Path) -> Result<(), String>,
{
    paths
        .iter()
        .filter_map(|p| verify(p).err().map(|e| (p.clone(), e)))
        .collect()
}

/// Replace `dest` with a copy of `src`, staged in `dest`'s own directory so
/// the final `rename(2)` stays on one filesystem. Renaming (instead of
/// writing in place) leaves running processes on the old inode, and replaces
/// a symlink at `dest` rather than writing through it into whatever it
/// points at (e.g. a developer's build tree).
pub fn replace_from(src: &Path, dest: &Path) -> Result<()> {
    let dir = dest
        .parent()
        .with_context(|| format!("{} has no parent directory", dest.display()))?;
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(RUNTIME_BIN);
    let staged = dir.join(format!(".{name}.repair"));
    let result = (|| -> Result<()> {
        std::fs::copy(src, &staged)
            .with_context(|| format!("could not stage {}", staged.display()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755))?;
        }
        std::fs::rename(&staged, dest)
            .with_context(|| format!("could not replace {}", dest.display()))?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&staged);
    }
    result
}

/// Outcome of a check/repair pass, for the caller's summary line.
#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every installed runtime passed.
    Healthy,
    /// `--check`: problems found and reported, nothing touched.
    Reported(usize),
    /// Every failing runtime was replaced and now passes.
    Repaired(usize),
}

/// Testable core. `fetch` produces a fresh runtime file and is called at most
/// once, only when something fails and `check_only` is false. Every replaced
/// path is verified again; a replacement that still fails is an error, never
/// reported as repaired.
pub fn check_or_repair_with<V, F>(
    paths: &[PathBuf],
    check_only: bool,
    verify: V,
    fetch: F,
) -> Result<Outcome>
where
    V: Fn(&Path) -> Result<(), String>,
    F: FnOnce() -> Result<PathBuf>,
{
    let invalid = find_invalid(paths, &verify);
    if invalid.is_empty() {
        return Ok(Outcome::Healthy);
    }
    for (path, reason) in &invalid {
        println!(
            "⚠ {} failed signature verification: {reason}",
            path.display()
        );
    }
    if check_only {
        println!("  Run `mur update` to restore the release runtime.");
        return Ok(Outcome::Reported(invalid.len()));
    }
    let fresh = fetch()?;
    for (path, _) in &invalid {
        replace_from(&fresh, path)?;
        verify(path).map_err(|e| {
            anyhow::anyhow!(
                "{} still fails signature verification after repair: {e}",
                path.display()
            )
        })?;
        println!("✓ restored {}", path.display());
    }
    Ok(Outcome::Repaired(invalid.len()))
}

/// Production entry: verify the installed runtimes with the real attestation
/// check and, unless `check_only`, restore failures from the `version`
/// release archive. No-op (and no network) when attestation is not enforced
/// or every runtime already passes.
pub fn check_or_repair(version: &str, check_only: bool) -> Result<Outcome> {
    if !enforced() {
        return Ok(Outcome::Healthy);
    }
    let tmp = tempfile::tempdir()?;
    let fresh = tmp.path().join(format!("{RUNTIME_BIN}.fresh"));
    check_or_repair_with(
        &installed_runtimes(),
        check_only,
        |p| mur_common::binary_attestation::verify_runtime_signature(p).map_err(|e| e.to_string()),
        || {
            println!("  Restoring {RUNTIME_BIN} from the v{version} release…");
            fetch_runtime(version, &fresh)?;
            Ok(fresh.clone())
        },
    )
}

/// Homebrew installs: brew owns the keg, so only point at the brew fix.
pub fn report_for_brew() {
    if !enforced() {
        return;
    }
    let invalid = find_invalid(&installed_runtimes(), |p| {
        mur_common::binary_attestation::verify_runtime_signature(p).map_err(|e| e.to_string())
    });
    for (path, reason) in &invalid {
        println!(
            "⚠ {} failed signature verification: {reason}",
            path.display()
        );
    }
    if !invalid.is_empty() {
        println!("  Run `brew reinstall mur` to restore the release runtime.");
    }
}

/// Download the `version` archive for this host, verify its checksum, and
/// extract the runtime to `dest`.
fn fetch_runtime(version: &str, dest: &Path) -> Result<()> {
    let release = release::fetch_release(version)
        .with_context(|| format!("could not fetch release v{version}. Are you online?"))?;
    let asset_name = release::asset_name_for_host()
        .ok_or_else(|| anyhow::anyhow!("no prebuilt archive for this platform"))?;
    let bytes = release::download_verified(&release, asset_name)?;
    release::extract_binary(asset_name, &bytes, RUNTIME_BIN, dest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn write(path: &Path, body: &str) {
        std::fs::write(path, body).unwrap();
    }

    /// "Signed" means the file body is exactly "good".
    fn verify_body(p: &Path) -> Result<(), String> {
        match std::fs::read_to_string(p).unwrap_or_default().as_str() {
            "good" => Ok(()),
            other => Err(format!("bad body {other:?}")),
        }
    }

    #[test]
    fn healthy_runtimes_never_fetch() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join(RUNTIME_BIN);
        write(&rt, "good");
        let fetched = Cell::new(false);
        let out = check_or_repair_with(&[rt], false, verify_body, || {
            fetched.set(true);
            unreachable!()
        })
        .unwrap();
        assert_eq!(out, Outcome::Healthy);
        assert!(!fetched.get());
    }

    #[test]
    fn check_only_reports_without_touching_files() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join(RUNTIME_BIN);
        write(&rt, "adhoc");
        let out = check_or_repair_with(std::slice::from_ref(&rt), true, verify_body, || {
            unreachable!("check-only must not download")
        })
        .unwrap();
        assert_eq!(out, Outcome::Reported(1));
        assert_eq!(std::fs::read_to_string(&rt).unwrap(), "adhoc");
    }

    #[test]
    fn repairs_only_the_failing_runtime() {
        let dir = tempfile::tempdir().unwrap();
        let sibling = dir.path().join("sibling");
        let copy = dir.path().join("copy");
        let fresh = dir.path().join("fresh");
        write(&sibling, "good");
        write(&copy, "adhoc");
        write(&fresh, "good");
        let out =
            check_or_repair_with(&[sibling.clone(), copy.clone()], false, verify_body, || {
                Ok(fresh.clone())
            })
            .unwrap();
        assert_eq!(out, Outcome::Repaired(1));
        assert_eq!(std::fs::read_to_string(&copy).unwrap(), "good");
        assert!(!dir.path().join(".copy.repair").exists());
    }

    #[test]
    fn a_replacement_that_still_fails_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join(RUNTIME_BIN);
        let fresh = dir.path().join("fresh");
        write(&rt, "adhoc");
        write(&fresh, "also-bad");
        let err =
            check_or_repair_with(&[rt], false, verify_body, || Ok(fresh.clone())).unwrap_err();
        assert!(err.to_string().contains("still fails"), "{err}");
    }

    #[test]
    fn fetch_failure_propagates_and_leaves_file_alone() {
        let dir = tempfile::tempdir().unwrap();
        let rt = dir.path().join(RUNTIME_BIN);
        write(&rt, "adhoc");
        let err = check_or_repair_with(std::slice::from_ref(&rt), false, verify_body, || {
            anyhow::bail!("offline")
        })
        .unwrap_err();
        assert!(err.to_string().contains("offline"));
        assert_eq!(std::fs::read_to_string(&rt).unwrap(), "adhoc");
    }

    #[cfg(unix)]
    #[test]
    fn replace_from_replaces_a_symlink_not_its_target() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("dev-build");
        let link = dir.path().join(RUNTIME_BIN);
        let fresh = dir.path().join("fresh");
        write(&target, "dev");
        write(&fresh, "good");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        replace_from(&fresh, &link).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "dev");
        assert!(!std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert_eq!(std::fs::read_to_string(&link).unwrap(), "good");
    }

    #[test]
    fn dedup_drops_missing_and_same_real_file() {
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a");
        write(&a, "x");
        let got = dedup_existing(vec![a.clone(), dir.path().join("missing"), a.clone()]);
        assert_eq!(got, vec![a]);
    }
}
