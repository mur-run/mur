//! `mur update` self-update implementation.
//!
//! The update flow is driven by `run()`. Network and platform-specific code is
//! split into submodules to keep each file under the 800-line rule and to make
//! unit testing possible.

pub mod release;
pub mod resign;
pub mod source;
pub mod swap;

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy)]
pub struct UpdateOptions {
    pub check_only: bool,
    /// After a successful upgrade, restart agents running a stale binary
    /// (honoring `update.restart_exclude`).
    pub restart_agents: bool,
}

pub fn run(opts: UpdateOptions) -> Result<()> {
    let src = source::detect();
    // Homebrew installs are upgraded through brew itself so the Cellar and
    // formula bookkeeping stay consistent — run it instead of self-swapping.
    if src == source::InstallSource::Homebrew {
        if opts.check_only {
            println!("Installed via Homebrew. Run: brew upgrade mur");
            return Ok(());
        }
        println!("Installed via Homebrew — running: brew upgrade mur");
        let status = std::process::Command::new("brew")
            .args(["upgrade", "mur"])
            .status()
            .context("failed to run brew — upgrade manually: brew upgrade mur")?;
        if !status.success() {
            anyhow::bail!("brew upgrade mur failed ({status})");
        }
        // brew pour leaves the binaries ad-hoc signed (relocation invalidates
        // any CI signature) — re-sign with the stable identity so keychain
        // grants survive, then walk the deploy checklist (#849/#866).
        // No extracted runtime here: brew just refreshed the keg, so the
        // sibling fallback finds the current one.
        resign::post_upgrade(opts.restart_agents, None)?;
        return Ok(());
    }
    if let Some(hint) = src.upgrade_hint() {
        println!("{hint}");
        return Ok(());
    }

    let release =
        release::fetch_latest().context("Could not check for updates. Are you online?")?;
    let current = env!("CARGO_PKG_VERSION");
    let latest = release::strip_v_prefix(&release.tag_name);

    if !release::is_newer(current, latest)? {
        println!("Already up to date (v{current})");
        if let Some(nudge) = hub_staleness_nudge(current) {
            println!("{nudge}");
        }
        return Ok(());
    }

    println!("New version available: v{current} → v{latest}");
    if opts.check_only {
        return Ok(());
    }

    let asset_name = release::asset_name_for_host().ok_or_else(|| {
        anyhow::anyhow!(
            "No prebuilt binary for {os}/{arch}. Install from source: cargo install mur",
            os = std::env::consts::OS,
            arch = std::env::consts::ARCH,
        )
    })?;
    let asset = release::select_asset(&release, asset_name)?;

    let client = reqwest::blocking::Client::builder()
        .user_agent(concat!("mur-update/", env!("CARGO_PKG_VERSION")))
        .timeout(std::time::Duration::from_secs(120))
        .build()?;

    println!("Downloading {asset_name}…");
    let bin_bytes = client
        .get(&asset.browser_download_url)
        .send()?
        .error_for_status()?
        .bytes()?
        .to_vec();

    let checksums_asset = release::select_asset(&release, "checksums.txt")?;
    let checksums_txt = client
        .get(&checksums_asset.browser_download_url)
        .send()?
        .error_for_status()?
        .text()?;
    let expected = release::checksum_for(&checksums_txt, asset_name)
        .ok_or_else(|| anyhow::anyhow!("no checksum entry for {asset_name}"))?;
    let actual = release::sha256_hex(&bin_bytes);
    if !actual.eq_ignore_ascii_case(&expected) {
        anyhow::bail!("Checksum verification FAILED. Aborting.");
    }

    let target = swap::current_exe().context(
        "Cannot determine install location. Please reinstall via: \
         curl -fsSL https://mur.run/install.sh | sh",
    )?;
    let tmp_dir = tempfile::tempdir()?;
    let tmp_bin = tmp_dir.path().join(if cfg!(windows) {
        "mur.new.exe"
    } else {
        "mur.new"
    });
    release::extract_binary(
        asset_name,
        &bin_bytes,
        if cfg!(windows) { "mur.exe" } else { "mur" },
        &tmp_bin,
    )?;

    #[cfg(unix)]
    {
        // The tarball ships the agent runtime too — hand it to post_upgrade so
        // the launchd copy is refreshed from THIS release, not from whatever
        // stale sibling is installed (the brew keg lags `mur update`).
        let tmp_runtime = tmp_dir.path().join("mur-agent-runtime.new");
        let fresh_runtime =
            release::extract_binary(asset_name, &bin_bytes, "mur-agent-runtime", &tmp_runtime)
                .ok()
                .map(|_| tmp_runtime);
        swap::swap(&tmp_bin, &target)?;
        println!("Updated to v{latest}");
        refresh_siblings(asset_name, &bin_bytes, &target);
        resign::post_upgrade(opts.restart_agents, fresh_runtime.as_deref())?;
    }
    #[cfg(windows)]
    {
        swap::spawn_windows_swap_helper(&tmp_bin, &target)?;
        println!("Update staged; closing now so it can take effect…");
    }

    if let Some(nudge) = hub_staleness_nudge(latest) {
        println!("{nudge}");
    }

    drop(tmp_dir);
    Ok(())
}

/// Binaries that ship in the same tarball as `mur` and must move with it.
///
/// `mur-agent-runtime` is deliberately absent: `resign::post_upgrade` owns
/// that one, because it also has to reach the launchd copy and the brew keg,
/// not just the sibling next to `mur`.
#[cfg(unix)]
const SIBLING_BINARIES: &[&str] = &["murmurd", "mur-mcp-server", "mur-research-gateway"];

/// Replace every installed sibling of `mur` from this release.
///
/// Without this, `mur update` swapped ONLY `mur`: the daemon, the MCP server
/// and the research gateway stayed on whatever version first installed them,
/// forever. Version skew across binaries that share on-disk formats and a
/// protocol is not a theoretical risk — a gateway pinned to an old crate is
/// how `mur hook stats` ended up bucketing records under a version nobody was
/// running.
///
/// Only files that ALREADY exist next to `mur` are touched: a machine that
/// never installed the research gateway must not silently gain one.
/// Failures are reported and skipped — a sibling that cannot be replaced must
/// not turn a completed `mur` upgrade into an error.
#[cfg(unix)]
fn refresh_siblings(asset_name: &str, bin_bytes: &[u8], target: &std::path::Path) {
    let Some(dir) = target.parent() else {
        return;
    };
    for name in SIBLING_BINARIES {
        let dest = dir.join(name);
        if !dest.is_file() {
            continue;
        }
        // Stage INSIDE the install dir, not the temp dir: `rename(2)` is only
        // atomic within one filesystem, and $TMPDIR is not guaranteed to share
        // one with the install prefix.
        let staged = dir.join(format!(".{name}.new"));
        match release::extract_binary(asset_name, bin_bytes, name, &staged)
            .and_then(|()| swap::swap(&staged, &dest))
        {
            Ok(()) => println!("  updated {}", dest.display()),
            Err(e) => {
                let _ = std::fs::remove_file(&staged);
                eprintln!(
                    "warning: could not update {} ({e:#}) — it stays on the previous version",
                    dest.display()
                );
            }
        }
    }
}

/// Best-effort: if MUR Hub is installed and older than `cli_version`, return a
/// one-line nudge. The Hub records its version in `~/.mur/host_path`
/// (line 2; format `"<exe_path>\n<version>"`, written by the Hub on launch).
/// The Hub owns its own update — we only inform, never touch the `.app`.
/// Any error (no Hub, unreadable file, unparseable version) → `None`.
fn hub_staleness_nudge(cli_version: &str) -> Option<String> {
    let home = std::env::var_os("MUR_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".mur")))?;
    let contents = std::fs::read_to_string(home.join("host_path")).ok()?;
    stale_hub_nudge_from(&contents, cli_version)
}

/// Pure core of [`hub_staleness_nudge`]: parse the `host_path` contents and, if
/// the recorded Hub version is older than `cli_version`, format the nudge.
fn stale_hub_nudge_from(host_path_contents: &str, cli_version: &str) -> Option<String> {
    let hub_version = host_path_contents.lines().nth(1)?.trim();
    if hub_version.is_empty() {
        return None;
    }
    // Hub strictly older than the CLI we just landed on → nudge.
    if release::is_newer(hub_version, cli_version).ok()? {
        Some(format!(
            "ℹ MUR Hub v{hub_version} is installed and out of date — \
             open it to auto-update."
        ))
    } else {
        None
    }
}

/// Best-effort, after an upgrade: name the mur-compress writer versions that
/// were active in the last two days yet are older than this CLI. Those are
/// long-lived processes outside `mur update`'s reach — the model gateway is
/// the usual culprit — still executing pre-upgrade crates. One line, informed
/// by the shared stats ledger; any read/parse problem stays silent.
pub(crate) fn warn_stale_compress_writers() {
    let home = crate::paths::mur_root(None);
    let Ok(s) = std::fs::read_to_string(home.join("compress").join("stats.json")) else {
        return;
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&s) else {
        return;
    };
    let today = chrono::Local::now().date_naive();
    let days = [
        today.to_string(),
        today.pred_opt().map(|d| d.to_string()).unwrap_or_default(),
    ];
    let stale = stale_writer_versions(&v, &days, env!("CARGO_PKG_VERSION"));
    if !stale.is_empty() {
        println!(
            "ℹ mur-compress writers on older crates were active recently: {} — \
             a long-lived service outside this update (e.g. mur-model-gateway) \
             is still on old code; rebuild/restart it to pick up current behavior.",
            stale.join(", ")
        );
    }
}

/// True when `ver` trails `current` by at least a full minor.
///
/// Patch-level lag is not worth a notice, and firing on it makes the notice
/// worthless. Two things guarantee patch lag with nothing wrong:
///
/// - `mur-model-gateway` pins `mur-compress` to an exact release tag, so every
///   release leaves it exactly one patch behind *by construction*. Nothing is
///   stale; the pin simply cannot lead a release it was cut before.
/// - The CLI's own earlier buckets from the same day. Upgrading through
///   2.68.0 → .1 → .2 → .4 in one day left four buckets dated today, and the
///   first three got reported as "a long-lived service outside this update"
///   when they were this very binary an hour earlier.
///
/// The signal worth keeping is the one that genuinely fired: the gateway
/// writing as 2.61.0 while the CLI was 2.68.2 — seven minors of drift, and a
/// real behavioural gap.
fn lags_by_a_minor(ver: &str, current: &str) -> bool {
    let (Ok(v), Ok(c)) = (
        semver::Version::parse(release::strip_v_prefix(ver)),
        semver::Version::parse(release::strip_v_prefix(current)),
    ) else {
        return false; // unparseable bucket key — say nothing rather than guess
    };
    (c.major, c.minor) > (v.major, v.minor)
}

/// Pure core of [`warn_stale_compress_writers`]: versions from the stats
/// ledger's `buckets[version][date]` slices that recorded compressions on any
/// of `days` and trail `current` by at least a minor (see [`lags_by_a_minor`]).
fn stale_writer_versions(stats: &serde_json::Value, days: &[String], current: &str) -> Vec<String> {
    let Some(buckets) = stats.get("buckets").and_then(|b| b.as_object()) else {
        return Vec::new();
    };
    let mut out: Vec<String> = buckets
        .iter()
        .filter(|(ver, _)| lags_by_a_minor(ver, current))
        .filter(|(_, per_day)| {
            days.iter().any(|d| {
                per_day
                    .get(d)
                    .and_then(|s| s.get("compressions"))
                    .and_then(|c| c.as_u64())
                    .unwrap_or(0)
                    > 0
            })
        })
        .map(|(ver, _)| ver.clone())
        .collect();
    out.sort();
    out
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use super::refresh_siblings;
    use super::{stale_hub_nudge_from, stale_writer_versions};

    const HOST_PATH: &str = "/Applications/MUR Hub.app/Contents/MacOS/mur-hub-gui\n2.26.0";

    #[test]
    fn nudges_when_hub_older() {
        assert!(stale_hub_nudge_from(HOST_PATH, "2.27.0").is_some());
    }

    #[test]
    fn silent_when_hub_equal_or_newer() {
        assert!(stale_hub_nudge_from(HOST_PATH, "2.26.0").is_none());
        assert!(stale_hub_nudge_from(HOST_PATH, "2.25.0").is_none());
    }

    #[test]
    fn silent_on_malformed_or_missing_version() {
        assert!(stale_hub_nudge_from("/only/a/path", "2.27.0").is_none());
        assert!(stale_hub_nudge_from("/path\n   ", "2.27.0").is_none());
        assert!(stale_hub_nudge_from("", "2.27.0").is_none());
    }

    /// The field shape: an old gateway bucket active today, current-version
    /// buckets active too, and a dormant ancient bucket that must stay out.
    #[test]
    fn stale_writer_versions_names_active_old_buckets_only() {
        let stats = serde_json::json!({
            "buckets": {
                "2.61.0": { "2026-08-05": { "compressions": 1500 } },
                "2.66.0": { "2026-08-05": { "compressions": 40 } },
                "2.40.1": { "2026-07-01": { "compressions": 9999 } },
                "not-a-version": { "2026-08-05": { "compressions": 5 } }
            }
        });
        let days = ["2026-08-05".to_string(), "2026-08-04".to_string()];
        assert_eq!(
            stale_writer_versions(&stats, &days, "2.66.0"),
            vec!["2.61.0".to_string()],
        );
    }

    /// `mur-model-gateway` pins `mur-compress` to an exact release tag, so it
    /// is one patch behind the moment the next release lands — by construction,
    /// with nothing to fix. Warning about it turned the notice into a fixture
    /// that appeared after every release and meant nothing.
    #[test]
    fn stale_writer_versions_ignores_a_patch_behind() {
        let stats = serde_json::json!({
            "buckets": { "2.68.3": { "2026-08-11": { "compressions": 1431 } } }
        });
        let days = ["2026-08-11".to_string(), "2026-08-10".to_string()];
        assert!(stale_writer_versions(&stats, &days, "2.68.4").is_empty());
    }

    /// Upgrading through 2.68.0 → .1 → .2 → .4 in one day leaves four buckets
    /// dated today. The first three are this very binary an hour ago, not "a
    /// long-lived service outside this update".
    #[test]
    fn stale_writer_versions_ignores_todays_own_upgrade_path() {
        let stats = serde_json::json!({
            "buckets": {
                "2.68.0": { "2026-08-11": { "compressions": 21 } },
                "2.68.1": { "2026-08-11": { "compressions": 35 } },
                "2.68.2": { "2026-08-11": { "compressions": 11 } },
                "2.61.0": { "2026-08-11": { "compressions": 496 } }
            }
        });
        let days = ["2026-08-11".to_string()];
        assert_eq!(
            stale_writer_versions(&stats, &days, "2.68.4"),
            vec!["2.61.0".to_string()],
            "the seven-minor drift is the whole point; only the patch noise goes"
        );
    }

    #[test]
    fn stale_writer_versions_silent_on_missing_buckets() {
        let days = ["2026-08-05".to_string()];
        assert!(stale_writer_versions(&serde_json::json!({}), &days, "2.66.0").is_empty());
        assert!(
            stale_writer_versions(&serde_json::json!({"buckets": 3}), &days, "2.66.0").is_empty()
        );
    }

    /// `mur update` used to swap only `mur`, leaving `murmurd` /
    /// `mur-mcp-server` / `mur-research-gateway` on whatever version first
    /// installed them. Siblings that ARE installed must move with it — and
    /// ones that are not must stay absent, so an update never installs a
    /// binary the user never chose.
    #[cfg(unix)]
    #[test]
    fn siblings_are_replaced_in_place_and_never_created() {
        fn tgz(files: &[(&str, &[u8])]) -> Vec<u8> {
            let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
            {
                let mut tar = tar::Builder::new(&mut gz);
                for (name, data) in files {
                    let mut h = tar::Header::new_gnu();
                    h.set_size(data.len() as u64);
                    h.set_mode(0o755);
                    h.set_cksum();
                    tar.append_data(&mut h, name, *data).unwrap();
                }
                tar.finish().unwrap();
            }
            gz.finish().unwrap()
        }

        let dir = tempfile::tempdir().unwrap();
        let mur = dir.path().join("mur");
        std::fs::write(&mur, b"NEW-MUR").unwrap();
        // Installed → must be replaced.
        std::fs::write(dir.path().join("murmurd"), b"OLD").unwrap();
        std::fs::write(dir.path().join("mur-mcp-server"), b"OLD").unwrap();
        // NOT installed → must stay that way.
        let gateway = dir.path().join("mur-research-gateway");

        let bytes = tgz(&[
            ("mur", b"NEW-MUR"),
            ("murmurd", b"NEW-DAEMON"),
            ("mur-mcp-server", b"NEW-MCP"),
            ("mur-research-gateway", b"NEW-GATEWAY"),
        ]);
        refresh_siblings("mur.tar.gz", &bytes, &mur);

        assert_eq!(
            std::fs::read(dir.path().join("murmurd")).unwrap(),
            b"NEW-DAEMON"
        );
        assert_eq!(
            std::fs::read(dir.path().join("mur-mcp-server")).unwrap(),
            b"NEW-MCP"
        );
        assert!(!gateway.exists(), "must not install what was never there");
        assert!(
            !dir.path().join(".murmurd.new").exists(),
            "staging file must not survive"
        );
    }

    /// A sibling missing from the archive must not delete or truncate the
    /// installed copy — the upgrade leaves it alone and says so.
    #[cfg(unix)]
    #[test]
    fn a_sibling_absent_from_the_archive_is_left_intact() {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut tar = tar::Builder::new(&mut gz);
            let data = b"NEW-MUR";
            let mut h = tar::Header::new_gnu();
            h.set_size(data.len() as u64);
            h.set_mode(0o755);
            h.set_cksum();
            tar.append_data(&mut h, "mur", &data[..]).unwrap();
            tar.finish().unwrap();
        }
        let bytes = gz.finish().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let mur = dir.path().join("mur");
        std::fs::write(&mur, b"NEW-MUR").unwrap();
        std::fs::write(dir.path().join("murmurd"), b"KEEP-ME").unwrap();

        refresh_siblings("mur.tar.gz", &bytes, &mur);

        assert_eq!(
            std::fs::read(dir.path().join("murmurd")).unwrap(),
            b"KEEP-ME"
        );
        assert!(!dir.path().join(".murmurd.new").exists());
    }
}
