//! Post-upgrade re-signing + deploy checklist (macOS; #849/#866).
//!
//! Keychain grants bind to the code-signing identity. Fresh installs are
//! ad-hoc signed (a new CDHash per build), so every upgrade makes macOS treat
//! each binary as a brand-new application: grants die, and Hub/launchd-spawned
//! agents — which cannot show an authorization prompt — fail silently.
//! Re-signing with a stable identity right after the upgrade keeps the
//! identity, and therefore the grants, across upgrades.
//!
//! Only the re-signing is macOS-specific — the deploy checklist and the
//! stale-agent restart run on every platform.

use anyhow::Result;

/// Binary names eligible for post-upgrade re-signing: everything that resolves
/// `keychain:` SecretRefs plus the SHA-pinned MCP gateway.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub const SIGN_TARGETS: &[&str] = &[
    "mur",
    "murmurd",
    "mur-mcp-server",
    "mur-research-gateway",
    "mur-agent-runtime",
];

/// Run the post-upgrade leg: re-sign (macOS only — see the module docs), print
/// the deploy checklist, optionally restart stale agents.
///
/// `fresh_runtime` is the `mur-agent-runtime` extracted from the release
/// archive, when it shipped one — the authoritative source for refreshing the
/// launchd copy (installed siblings can lag the self-update).
///
/// Only the re-signing is macOS-specific. The checklist and the restart legs
/// (agents + daemon) apply everywhere: an upgrade changes every binary's hash
/// on any platform, which stales every long-lived process running the old one.
#[cfg_attr(not(target_os = "macos"), allow(unused_variables))]
pub fn post_upgrade(restart_agents: bool, fresh_runtime: Option<&std::path::Path>) -> Result<()> {
    #[cfg(target_os = "macos")]
    macos::resign_installed_binaries(fresh_runtime)?;

    let daemon_running = crate::cmd::murmurd::murmurd_running();
    print_checklist(restart_agents, daemon_running);

    let agents_result = if restart_agents {
        crate::cmd::agent::restart_stale_excluding(&restart_exclusions())
    } else {
        Ok(())
    };
    // The daemon is a long-lived process like any agent: without a restart it
    // keeps executing pre-upgrade code indefinitely. Runs even when the agent
    // leg failed — a half-finished deploy should not also strand the daemon
    // on the old binary. Best-effort: a daemon that will not come back must
    // not turn a completed binary upgrade into an error.
    if restart_agents && daemon_running {
        println!("restarting murmurd onto the upgraded binary…");
        if let Err(e) = crate::cmd::murmurd::cmd_murmurd_restart() {
            eprintln!("warning: murmurd restart failed: {e:#} — run `mur daemon restart` manually");
        }
    }
    crate::update::warn_stale_compress_writers();
    agents_result
}

fn load_config() -> mur_common::config::Config {
    let home = crate::paths::mur_root(None);
    mur_common::config::Config::load_or_default(&home.join("config.yaml"))
}

fn restart_exclusions() -> Vec<String> {
    load_config().update.restart_exclude
}

/// Printed only when `--restart-agents` was NOT passed — with it, both legs
/// run automatically and a checklist would just narrate them. The old advice
/// to re-pin MCP servers after an upgrade is gone on purpose: the bundled
/// server re-pins itself at agent start (`mur-agent-runtime/src/mcp_repin.rs`,
/// #793) and third-party pins cover binaries an upgrade never touches.
fn print_checklist(restart_agents: bool, daemon_running: bool) {
    if restart_agents {
        return;
    }
    println!();
    println!("To finish the deploy:");
    println!("  1. Restart agents still running the old binary:");
    println!("       mur agent restart --stale        (or rerun with --restart-agents)");
    if daemon_running {
        println!("  2. Move the running daemon onto the new binary:");
        println!("       mur daemon restart");
    }
}

/// Identity precedence: config beats env; empty strings count as unset.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn pick_identity(config: Option<&str>, env: Option<&str>) -> Option<String> {
    let non_empty = |s: Option<&str>| {
        s.map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
    };
    non_empty(config).or_else(|| non_empty(env))
}

/// `codesign -dv` prints `Signature=adhoc` for ad-hoc binaries — the exact
/// condition that makes keychain grants die on the next upgrade.
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) fn is_adhoc_output(codesign_dv_output: &str) -> bool {
    codesign_dv_output
        .lines()
        .any(|l| l.trim() == "Signature=adhoc")
}

#[cfg(target_os = "macos")]
mod macos {
    use std::collections::BTreeSet;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use anyhow::{Context, Result, bail};

    use super::{SIGN_TARGETS, is_adhoc_output, pick_identity};

    /// Re-sign every installed MUR binary with the configured identity. No
    /// identity configured → warn loudly and leave signatures as shipped.
    pub(super) fn resign_installed_binaries(fresh_runtime: Option<&Path>) -> Result<()> {
        let identity = pick_identity(
            config_identity().as_deref(),
            std::env::var("MUR_CODESIGN_IDENTITY").ok().as_deref(),
        );

        // Refresh the launchd runtime copy BEFORE the identity gate: shipping
        // the freshly downloaded runtime must not depend on whether a signing
        // identity is configured. With none configured (the default), the old
        // order discarded the release runtime entirely and left every service
        // on the previous — possibly ad-hoc — binary across every update.
        let mut targets = discover_targets();
        refresh_local_runtime_copy(&mut targets, fresh_runtime);

        let Some(id) = identity else {
            println!(
                "⚠ No codesign identity configured — installed binaries keep their current \
                 signatures."
            );
            println!(
                "  macOS keychain grants will NOT survive an ad-hoc rebuild: agents using \
                 `keychain:` secrets"
            );
            println!(
                "  may fail silently until re-authorized in the foreground (#866). To fix \
                 permanently, set"
            );
            println!(
                "  `update.codesign_identity` in ~/.mur/config.yaml (or MUR_CODESIGN_IDENTITY)."
            );
            return Ok(());
        };

        if targets.is_empty() {
            println!("⚠ no installed MUR binaries found to re-sign");
            return Ok(());
        }
        let (release_signed, to_sign) = split_release_signed(targets);
        if !release_signed.is_empty() {
            println!(
                "✓ kept the MUR Developer ID signature on {} binaries (a local re-sign would \
                 break runtime attestation)",
                release_signed.len()
            );
        }
        if !to_sign.is_empty() {
            sign_all(&id, &to_sign)?;
            verify_not_adhoc(&to_sign)?;
            println!(
                "✓ re-signed {} binaries with '{id}' — keychain grants survive this upgrade",
                to_sign.len()
            );
        }
        Ok(())
    }

    /// Partition `targets` into (already release-signed, needs local re-sign).
    ///
    /// A binary that already satisfies the MUR Developer ID requirement is
    /// strictly better than anything a local identity produces: it is stable
    /// across upgrades (all the re-sign feature exists to guarantee) AND it
    /// passes runtime attestation. `codesign --force`-ing it with a local
    /// identity would strip the team signature and fail every later
    /// `mur agent start/restart` / A2A dial on release builds. Dev builds
    /// embed no team ID (`verify_runtime_signature` vacuously passes), so
    /// there everything stays eligible for re-signing.
    fn split_release_signed(targets: Vec<PathBuf>) -> (Vec<PathBuf>, Vec<PathBuf>) {
        use mur_common::binary_attestation as attest;
        if !attest::IS_EMBEDDED_RELEASE {
            return (Vec::new(), targets);
        }
        targets
            .into_iter()
            .partition(|p| attest::verify_runtime_signature(p).is_ok())
    }

    fn config_identity() -> Option<String> {
        super::load_config().update.codesign_identity
    }

    /// Every existing SIGN_TARGETS binary, canonicalized (sign the real file,
    /// not a symlink): siblings of the current executable, the brew keg's bin
    /// dir (`mur-agent-runtime` lives there unlinked), and the launchd COPY at
    /// `~/.local/bin/mur-agent-runtime`.
    fn discover_targets() -> Vec<PathBuf> {
        let mut dirs: Vec<PathBuf> = Vec::new();
        if let Ok(exe) = std::env::current_exe()
            && let Ok(real) = exe.canonicalize()
            && let Some(dir) = real.parent()
        {
            dirs.push(dir.to_path_buf());
        }
        if let Some(keg_bin) = brew_keg_bin() {
            dirs.push(keg_bin);
        }

        let mut seen: BTreeSet<PathBuf> = BTreeSet::new();
        for dir in dirs {
            for name in SIGN_TARGETS {
                let p = dir.join(name);
                if let Ok(real) = p.canonicalize()
                    && real.is_file()
                {
                    seen.insert(real);
                }
            }
        }
        if let Some(copy) = local_runtime_copy_path()
            && copy.is_file()
        {
            seen.insert(copy);
        }
        seen.into_iter().collect()
    }

    fn brew_keg_bin() -> Option<PathBuf> {
        let out = Command::new("brew")
            .args(["--prefix", "mur"])
            .output()
            .ok()?;
        if !out.status.success() {
            return None;
        }
        let prefix = String::from_utf8(out.stdout).ok()?;
        let bin = PathBuf::from(prefix.trim()).join("bin");
        bin.is_dir().then(|| bin.canonicalize().ok()).flatten()
    }

    fn local_runtime_copy_path() -> Option<PathBuf> {
        dirs::home_dir().map(|h| h.join(".local/bin/mur-agent-runtime"))
    }

    /// The launchd services run a COPY (not a symlink) of the runtime at
    /// `~/.local/bin/mur-agent-runtime`. Refresh it from the runtime shipped
    /// with this release when available, else from a freshly-installed sibling
    /// — a stale copy is the old-version trap.
    fn refresh_local_runtime_copy(targets: &mut Vec<PathBuf>, fresh_runtime: Option<&Path>) {
        let Some(copy) = local_runtime_copy_path() else {
            return;
        };
        if !copy.exists() {
            // This machine doesn't use the copy layout (keg installs run the
            // keg binary, refreshed by `brew upgrade`) — but say so instead of
            // silently discarding a runtime we were handed.
            if fresh_runtime.is_some() {
                println!(
                    "ℹ no runtime copy at {} — this install runs the keg/sibling binary directly",
                    copy.display()
                );
            }
            return;
        }
        let source: Option<PathBuf> = fresh_runtime.map(PathBuf::from).or_else(|| {
            targets
                .iter()
                .find(|p| {
                    p.file_name().is_some_and(|n| n == "mur-agent-runtime")
                        && p.canonicalize().ok() != copy.canonicalize().ok()
                })
                .cloned()
        });
        match source {
            Some(src) => match replace_file(&src, &copy) {
                Ok(_) => println!("✓ refreshed {} from {}", copy.display(), src.display()),
                Err(e) => println!(
                    "⚠ could not refresh {} ({e}) — re-signing the existing copy",
                    copy.display()
                ),
            },
            None => println!(
                "⚠ no freshly-installed mur-agent-runtime found to refresh {} from — \
                 re-signing the existing (possibly stale) copy",
                copy.display()
            ),
        }
        if !targets.contains(&copy) {
            targets.push(copy);
        }
    }

    /// Copy `src` next to `dst`, then rename over it. Overwriting `dst` in
    /// place would SIGKILL every process executing that inode; the rename
    /// leaves running processes on the old inode instead.
    fn replace_file(src: &Path, dst: &Path) -> std::io::Result<()> {
        let tmp = dst.with_extension("new");
        std::fs::copy(src, &tmp)?;
        std::fs::rename(&tmp, dst)
    }

    /// `codesign --force -s <identity>` each target. Fail loud with every
    /// failure listed — a partially-signed install is exactly the silent state
    /// this feature exists to prevent.
    fn sign_all(identity: &str, targets: &[PathBuf]) -> Result<()> {
        let mut failures: Vec<String> = Vec::new();
        for path in targets {
            let out = Command::new("codesign")
                .args(["--force", "-s", identity])
                .arg(path)
                .output()
                .with_context(|| format!("run codesign on {}", path.display()))?;
            if !out.status.success() {
                failures.push(format!(
                    "{}: {}",
                    path.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                ));
            }
        }
        if !failures.is_empty() {
            bail!(
                "codesign failed for {} of {} binaries:\n  {}",
                failures.len(),
                targets.len(),
                failures.join("\n  ")
            );
        }
        Ok(())
    }

    /// Post-sign verification: `codesign -dv` must succeed and must not report
    /// `Signature=adhoc` for any target.
    fn verify_not_adhoc(targets: &[PathBuf]) -> Result<()> {
        let mut bad: Vec<String> = Vec::new();
        for path in targets {
            let out = Command::new("codesign")
                .args(["-dv"])
                .arg(path)
                .output()
                .with_context(|| format!("run codesign -dv on {}", path.display()))?;
            let stderr = String::from_utf8_lossy(&out.stderr);
            if !out.status.success() {
                bad.push(format!("{}: unsigned ({})", path.display(), stderr.trim()));
            } else if is_adhoc_output(&stderr) {
                bad.push(format!("{}: still ad-hoc after signing", path.display()));
            }
        }
        if !bad.is_empty() {
            bail!("signature verification failed:\n  {}", bad.join("\n  "));
        }
        Ok(())
    }

    #[cfg(test)]
    mod macos_tests {
        use super::replace_file;

        #[test]
        fn replace_file_swaps_content_and_leaves_no_temp() {
            let dir = tempfile::tempdir().unwrap();
            let src = dir.path().join("src");
            let dst = dir.path().join("mur-agent-runtime");
            std::fs::write(&src, b"NEW").unwrap();
            std::fs::write(&dst, b"OLD").unwrap();
            replace_file(&src, &dst).unwrap();
            assert_eq!(std::fs::read(&dst).unwrap(), b"NEW");
            assert!(!dir.path().join("mur-agent-runtime.new").exists());
        }

        #[test]
        fn dev_builds_never_treat_targets_as_release_signed() {
            // Without an embedded team ID, `verify_runtime_signature` passes
            // vacuously on EVERY path — which must not be read as "already
            // release-signed", or dev builds would skip re-signing entirely.
            if mur_common::binary_attestation::IS_EMBEDDED_RELEASE {
                return; // release CI exercises the real partition
            }
            let t = vec![std::path::PathBuf::from("/bin/ls")];
            let (release_signed, to_sign) = super::split_release_signed(t.clone());
            assert!(release_signed.is_empty());
            assert_eq!(to_sign, t);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_precedence_config_beats_env_and_blanks_are_unset() {
        assert_eq!(
            pick_identity(Some("Developer ID: A"), Some("Developer ID: B")).as_deref(),
            Some("Developer ID: A")
        );
        assert_eq!(
            pick_identity(None, Some("Developer ID: B")).as_deref(),
            Some("Developer ID: B")
        );
        assert_eq!(
            pick_identity(Some("  "), Some("Developer ID: B")).as_deref(),
            Some("Developer ID: B"),
            "blank config must fall through to env"
        );
        assert_eq!(pick_identity(Some(""), Some("")), None);
        assert_eq!(pick_identity(None, None), None);
    }

    #[test]
    fn adhoc_detection_matches_codesign_dv_shape() {
        let adhoc = "Executable=/opt/homebrew/bin/mur\nIdentifier=mur\n\
                     Format=Mach-O thin (arm64)\nSignature=adhoc\nInfo.plist=not bound";
        let devid = "Executable=/opt/homebrew/bin/mur\nIdentifier=mur\n\
                     Signature size=8968\nAuthority=Developer ID Application: X (TEAM)";
        assert!(is_adhoc_output(adhoc));
        assert!(!is_adhoc_output(devid));
        // `Signature size=…` must not substring-match `Signature=adhoc`.
        assert!(!is_adhoc_output("Signature size=8968"));
    }
}
