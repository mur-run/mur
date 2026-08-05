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

#[cfg(test)]
mod tests {
    use super::stale_hub_nudge_from;

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
}
