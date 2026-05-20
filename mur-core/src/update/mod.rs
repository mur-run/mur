//! `mur update` self-update implementation.
//!
//! The update flow is driven by `run()`. Network and platform-specific code is
//! split into submodules to keep each file under the 800-line rule and to make
//! unit testing possible.

pub mod release;
pub mod source;
pub mod swap;

use anyhow::{Context, Result};

#[derive(Debug, Clone, Copy)]
pub struct UpdateOptions {
    pub check_only: bool,
}

pub fn run(opts: UpdateOptions) -> Result<()> {
    let src = source::detect();
    if let Some(hint) = src.upgrade_hint() {
        println!("{hint}");
        return Ok(());
    }

    let release = release::fetch_latest().context("Could not check for updates. Are you online?")?;
    let current = env!("CARGO_PKG_VERSION");
    let latest = release::strip_v_prefix(&release.tag_name);

    if !release::is_newer(current, latest)? {
        println!("Already up to date (v{current})");
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
    let tmp_bin = tmp_dir.path().join(if cfg!(windows) { "mur.new.exe" } else { "mur.new" });
    release::extract_binary(asset_name, &bin_bytes, &tmp_bin)?;

    #[cfg(unix)]
    {
        swap::swap(&tmp_bin, &target)?;
        println!("Updated to v{latest}");
    }
    #[cfg(windows)]
    {
        swap::spawn_windows_swap_helper(&tmp_bin, &target)?;
        println!("Update staged; closing now so it can take effect…");
    }

    drop(tmp_dir);
    Ok(())
}
