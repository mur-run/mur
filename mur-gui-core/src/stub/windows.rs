//! Windows per-agent stub generation — spec §5.1.
//!
//! Creates a `.lnk` shortcut in the user's Start Menu Programs folder and
//! attempts to register the agent's URL scheme as the default handler.
//!
//! COM integration for `IApplicationAssociationRegistration` is a TODO —
//! the .lnk is created and functional; only the protocol handler default
//! registration is deferred.

use crate::stub::StubInfo;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn generate(
    slug: &str,
    display_name: &str,
    _bundle_id: &str,
    url_scheme: &str,
    _icon_icns: Option<&[u8]>,
    hub_version: &str,
    _mur_home: &Path,
) -> Result<StubInfo> {
    // ── 1. Locate the Hub executable ─────────────────────────────────────────
    let hub_exe = std::env::current_exe().context("current_exe")?;

    // ── 2. Build .lnk path ───────────────────────────────────────────────────
    let appdata = std::env::var_os("APPDATA")
        .map(PathBuf::from)
        .context("APPDATA not set")?;
    let lnk_dir = appdata
        .join("Microsoft")
        .join("Windows")
        .join("Start Menu")
        .join("Programs")
        .join("MuR Agents");
    std::fs::create_dir_all(&lnk_dir).context("create MuR Agents dir")?;
    let safe_name: String = display_name
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '-' })
        .collect();
    let lnk_path = lnk_dir.join(format!("{safe_name}.lnk"));

    // ── 3. Create .lnk via PowerShell ────────────────────────────────────────
    let ps_script = format!(
        "$s=(New-Object -Com WScript.Shell).CreateShortcut('{lnk}');\
         $s.TargetPath='{exe}';\
         $s.Arguments='--agent {slug}';\
         $s.Description='MuR Agent: {display_name}';\
         $s.Save()",
        lnk = lnk_path.to_string_lossy().replace('\'', "''"),
        exe = hub_exe.to_string_lossy().replace('\'', "''"),
        slug = slug,
        display_name = display_name,
    );
    let status = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", &ps_script])
        .status()
        .context("spawn powershell")?;
    if !status.success() {
        anyhow::bail!("PowerShell .lnk creation failed (exit {:?})", status.code());
    }

    // ── 4. Register URL scheme default handler ────────────────────────────────
    // TODO(M-export-4-windows-com): IApplicationAssociationRegistration
    // COM integration pending. The .lnk above is already functional for
    // Start Menu launch; URL scheme activation (`muragent-<slug>://`) requires
    // the app to also be registered in HKCU\Software\Classes\muragent-<slug>.
    tracing::warn!(
        "stub(windows): URL scheme {} default handler registration via COM is pending \
         (TODO M-export-4-windows-com). .lnk shortcut created at {}",
        url_scheme,
        lnk_path.display()
    );

    let _ = hub_version; // stored in stub metadata in future

    Ok(StubInfo {
        path: lnk_path,
        url_scheme: url_scheme.to_string(),
    })
}
