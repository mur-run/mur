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
/// Only the re-signing is macOS-specific. The checklist and the restart leg
/// apply everywhere: an upgrade changes every binary's hash on any platform,
/// which stales the running agents and their SHA-pinned MCP servers alike.
pub fn post_upgrade(restart_agents: bool) -> Result<()> {
    #[cfg(target_os = "macos")]
    macos::resign_installed_binaries()?;

    print_checklist();

    if restart_agents {
        crate::cmd::agent::restart_stale_excluding(&restart_exclusions())?;
    }
    Ok(())
}

fn load_config() -> mur_common::config::Config {
    let home = crate::paths::mur_root(None);
    mur_common::config::Config::load_or_default(&home.join("config.yaml"))
}

fn restart_exclusions() -> Vec<String> {
    load_config().update.restart_exclude
}

fn print_checklist() {
    println!();
    println!("To finish the deploy:");
    println!("  1. Restart agents still running the old binary:");
    println!("       mur agent restart --stale        (or rerun with --restart-agents)");
    println!("  2. Upgraded binaries have new hashes — refresh MCP pins per agent:");
    println!("       mur agent mcp pin <agent> <server>");
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
    use std::path::PathBuf;
    use std::process::Command;

    use anyhow::{Context, Result, bail};

    use super::{SIGN_TARGETS, is_adhoc_output, pick_identity};

    /// Re-sign every installed MUR binary with the configured identity. No
    /// identity configured → warn loudly and leave the install ad-hoc.
    pub(super) fn resign_installed_binaries() -> Result<()> {
        let identity = pick_identity(
            config_identity().as_deref(),
            std::env::var("MUR_CODESIGN_IDENTITY").ok().as_deref(),
        );

        let Some(id) = identity else {
            println!("⚠ No codesign identity configured — installed binaries stay ad-hoc signed.");
            println!(
                "  macOS keychain grants will NOT survive this upgrade: agents using \
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

        let mut targets = discover_targets();
        refresh_local_runtime_copy(&mut targets);
        if targets.is_empty() {
            println!("⚠ no installed MUR binaries found to re-sign");
            return Ok(());
        }
        sign_all(&id, &targets)?;
        verify_not_adhoc(&targets)?;
        println!(
            "✓ re-signed {} binaries with '{id}' — keychain grants survive this upgrade",
            targets.len()
        );
        Ok(())
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
    /// `~/.local/bin/mur-agent-runtime`. Refresh it from the newly-installed
    /// runtime before signing — a stale copy is the old-version + SIGKILL trap.
    fn refresh_local_runtime_copy(targets: &mut Vec<PathBuf>) {
        let Some(copy) = local_runtime_copy_path() else {
            return;
        };
        if !copy.exists() {
            return; // this machine doesn't use the copy layout
        }
        let source = targets.iter().find(|p| {
            p.file_name().is_some_and(|n| n == "mur-agent-runtime")
                && p.canonicalize().ok() != copy.canonicalize().ok()
        });
        match source {
            Some(src) => match std::fs::copy(src, &copy) {
                Ok(_) => println!("✓ refreshed {} from {}", copy.display(), src.display()),
                Err(e) => println!(
                    "⚠ could not refresh {} ({e}) — it may be running; re-signing the existing copy",
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
