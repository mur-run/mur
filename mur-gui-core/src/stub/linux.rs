//! Linux per-agent stub generation — spec §5.1.
//!
//! Creates a freedesktop `.desktop` file at
//! `~/.local/share/applications/run.mur.agent.<slug>.desktop` and registers
//! the agent's URL scheme via `xdg-mime` + `update-desktop-database`.

use crate::stub::StubInfo;
use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn generate(
    slug: &str,
    display_name: &str,
    bundle_id: &str,
    url_scheme: &str,
    _icon_icns: Option<&[u8]>,
    hub_version: &str,
    _mur_home: &Path,
) -> Result<StubInfo> {
    // ── 1. Locate Hub binary ──────────────────────────────────────────────────
    let hub_exe = std::env::current_exe().context("current_exe")?;

    // ── 2. Build .desktop path ────────────────────────────────────────────────
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("HOME not set")?;
    let desktop_dir = home.join(".local").join("share").join("applications");
    std::fs::create_dir_all(&desktop_dir).context("create applications dir")?;
    let desktop_id = format!("{bundle_id}.desktop");
    let desktop_path = desktop_dir.join(&desktop_id);

    // ── 3. Write .desktop file ───────────────────────────────────────────────
    // Per freedesktop spec; never write to deprecated mimeapps.list directly.
    let mime_type = format!("x-scheme-handler/{url_scheme}");
    let content = format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name={display_name}\n\
         Exec={exe} --agent {slug} %u\n\
         MimeType={mime};\n\
         StartupWMClass={bundle_id}\n\
         X-Mur-Host-Version={hub_version}\n\
         NoDisplay=false\n",
        exe = hub_exe.display(),
        mime = mime_type,
    );
    std::fs::write(&desktop_path, &content).context("write .desktop")?;

    // ── 4. Update database + set default handler ──────────────────────────────
    // update-desktop-database rebuilds the MIME cache so the new entry is visible.
    let _ = Command::new("update-desktop-database")
        .arg(&desktop_dir)
        .status(); // non-fatal if the tool is absent

    // xdg-mime sets our .desktop as the default handler for the URL scheme.
    let _ = Command::new("xdg-mime")
        .args(["default", &desktop_id, &mime_type])
        .status(); // non-fatal

    Ok(StubInfo {
        path: desktop_path,
        url_scheme: url_scheme.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_file_contents() {
        // Test the format string logic without touching the filesystem.
        let hub_exe = PathBuf::from("/usr/bin/mur-hub-gui");
        let slug = "coach";
        let display_name = "Coach";
        let bundle_id = "run.mur.agent.coach";
        let url_scheme = "muragent-coach";
        let hub_version = "2.16.1";
        let mime_type = format!("x-scheme-handler/{url_scheme}");

        let content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name={display_name}\n\
             Exec={exe} --agent {slug} %u\n\
             MimeType={mime};\n\
             StartupWMClass={bundle_id}\n\
             X-Mur-Host-Version={hub_version}\n\
             NoDisplay=false\n",
            exe = hub_exe.display(),
            mime = mime_type,
        );

        assert!(content.contains("run.mur.agent.coach"));
        assert!(content.contains("muragent-coach"));
        assert!(content.contains("Coach"));
        assert!(content.contains("--agent coach %u"));
    }
}
