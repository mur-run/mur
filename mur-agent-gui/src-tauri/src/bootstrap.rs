//! First-launch bootstrap — extract the embedded agent payload to
//! `~/.mur/agents/<name>/`, mint identity (Template mode) or run the
//! P0a.6 rekey ceremony (Clone mode), and create the
//! `~/.local/bin/mur_agent_<name>` symlink.
//!
//! Invoked from `main.rs::run()` BEFORE the sidecar manager spawns
//! the runtime. Idempotent — second launch is a no-op when the
//! agent home already exists with a matching UUID.
//!
//! See spec § 5.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BundleMode {
    Template,
    Clone,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EmbeddedMetadata {
    pub schema_version: u32,
    pub agent_name: String,
    pub display_name: String,
    pub mode: BundleMode,
    pub theme_default: String,
    pub mur_version: String,
}

/// Public entry — call once at app launch. `bundle_resource_dir` is
/// `app.path().resource_dir()` from the Tauri main; passed as an
/// argument so this module is testable without a Tauri context.
pub fn bootstrap_if_needed(bundle_resource_dir: &Path) -> Result<EmbeddedMetadata> {
    let metadata_path = bundle_resource_dir.join("metadata.json");
    let payload_path = bundle_resource_dir.join("agent-payload.tar.gz");

    if !metadata_path.exists() {
        // Dev-mode launch (`cargo tauri dev`) without a packaged
        // payload — fall back to whatever AGENT_NAME the user picked
        // and assume `~/.mur/agents/<name>/` already exists.
        let agent_name =
            std::env::var("MUR_GUI_AGENT_NAME").unwrap_or_else(|_| "template".to_string());
        return Ok(EmbeddedMetadata {
            schema_version: 1,
            agent_name: agent_name.clone(),
            display_name: agent_name,
            mode: BundleMode::Template,
            theme_default: "light".to_string(),
            mur_version: env!("CARGO_PKG_VERSION").to_string(),
        });
    }

    let metadata: EmbeddedMetadata = serde_json::from_str(
        &fs::read_to_string(&metadata_path)
            .with_context(|| format!("read {}", metadata_path.display()))?,
    )
    .with_context(|| format!("parse {}", metadata_path.display()))?;

    let agent_home = mur_home()?.join("agents").join(&metadata.agent_name);

    if agent_home.exists() {
        info!(
            agent_home = %agent_home.display(),
            "agent home already exists; skipping payload extract"
        );
        // P1.6 follow-up: read existing UUID + compare to embedded one
        // for the conflict dialog. v1 just trusts the existing install.
        ensure_symlink(&metadata.agent_name)?;
        return Ok(metadata);
    }

    if !payload_path.exists() {
        bail!(
            "metadata says agent '{}' but payload missing at {}",
            metadata.agent_name,
            payload_path.display()
        );
    }

    info!(
        mode = ?metadata.mode,
        agent = %metadata.agent_name,
        payload = %payload_path.display(),
        target = %agent_home.display(),
        "first-launch payload extract"
    );
    fs::create_dir_all(&agent_home).with_context(|| format!("create {}", agent_home.display()))?;
    extract_tar_gz(&payload_path, &agent_home)?;

    match metadata.mode {
        BundleMode::Template => mint_template_identity(&agent_home)?,
        BundleMode::Clone => run_clone_rekey(&agent_home)?,
    }

    ensure_symlink(&metadata.agent_name)?;
    info!(agent = %metadata.agent_name, "bootstrap complete");
    Ok(metadata)
}

fn mur_home() -> Result<PathBuf> {
    if let Ok(p) = std::env::var("MUR_HOME") {
        return Ok(PathBuf::from(p));
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot resolve home dir"))?;
    Ok(home.join(".mur"))
}

fn extract_tar_gz(archive: &Path, dest: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use tar::Archive;

    let f = fs::File::open(archive).with_context(|| format!("open {}", archive.display()))?;
    let mut a = Archive::new(GzDecoder::new(f));
    a.unpack(dest)
        .with_context(|| format!("unpack into {}", dest.display()))?;
    Ok(())
}

fn mint_template_identity(agent_home: &Path) -> Result<()> {
    use mur_common::identity::AgentIdentity;
    let identity = AgentIdentity::generate();
    identity
        .save(agent_home)
        .with_context(|| "save freshly minted identity")?;
    info!(pubkey = %identity.pubkey_text(), "template mode: minted Ed25519 keypair");
    Ok(())
}

fn run_clone_rekey(_agent_home: &Path) -> Result<()> {
    // P1.6 follow-up: invoke the existing rekey logic in
    // mur_core::cmd::agent_rekey or a corresponding agent_admin
    // function once it's exposed. v1 logs a warning so the install
    // doesn't silently keep the shipped key forever.
    warn!(
        "clone-mode bootstrap: shipped identity preserved (rekey ceremony \
         deferred to follow-up; for v1 use `mur agent rekey <name>` \
         immediately after first launch)"
    );
    Ok(())
}

/// Best-effort: create the `mur_agent_<name>` discovery symlink in
/// `MUR_AGENT_BIN_DIR` (default `~/.local/bin`) so an installed
/// `mur` CLI can `agent list` / `agent send` against this install.
///
/// Resolution order for the symlink target:
/// 1. `MUR_AGENT_RUNTIME_BIN` env (explicit override)
/// 2. The bundled sidecar's absolute path (Tauri puts it next to
///    the GUI executable, e.g. `MyAgent.app/Contents/MacOS/
///    mur-agent-runtime`)
/// 3. Skip — the GUI is the lifecycle owner; CLI integration just
///    isn't available without a separate `mur` install.
///
/// The previous version naively `symlink("mur-agent-runtime", …)`
/// which produced a DANGLING symlink for users who never had a
/// system-wide runtime. That broke `mur agent list` for any
/// GUI-installed agent. See PR #41 review § Critical #1.
fn ensure_symlink(agent_name: &str) -> Result<()> {
    let bin_dir = if let Ok(d) = std::env::var("MUR_AGENT_BIN_DIR") {
        PathBuf::from(d)
    } else {
        dirs::home_dir()
            .ok_or_else(|| anyhow!("home dir"))?
            .join(".local/bin")
    };
    fs::create_dir_all(&bin_dir).with_context(|| format!("create {}", bin_dir.display()))?;
    let symlink = bin_dir.join(format!("mur_agent_{agent_name}"));
    if symlink.exists() {
        return Ok(());
    }

    let runtime_target = if let Ok(v) = std::env::var("MUR_AGENT_RUNTIME_BIN") {
        Some(PathBuf::from(v))
    } else {
        // Resolve the bundled sidecar relative to the current
        // executable. Tauri 2 places `bundle.externalBin` entries
        // next to the main bin on Unix; on macOS specifically
        // they end up in `.app/Contents/MacOS/`.
        std::env::current_exe().ok().and_then(|exe| {
            exe.parent().map(|p| {
                p.join(if cfg!(windows) {
                    "mur-agent-runtime.exe"
                } else {
                    "mur-agent-runtime"
                })
            })
        })
    };

    let Some(target) = runtime_target else {
        warn!(
            agent = %agent_name,
            "skipping CLI discovery symlink: cannot resolve runtime binary path \
             (set MUR_AGENT_RUNTIME_BIN to override)"
        );
        return Ok(());
    };

    if !target.exists() {
        warn!(
            target = %target.display(),
            agent = %agent_name,
            "skipping CLI discovery symlink: target binary not present"
        );
        return Ok(());
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&target, &symlink)
            .with_context(|| format!("symlink {}", symlink.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = (symlink, target); // Windows: junctions need elevation; defer to P2
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn bootstrap_in_dev_mode_returns_default_metadata() {
        let tmp = tempdir_for_test();
        // No metadata.json present — should return synthesised default.
        let meta = bootstrap_if_needed(&tmp).unwrap();
        assert_eq!(meta.mode, BundleMode::Template);
        assert_eq!(meta.theme_default, "light");
    }

    #[test]
    fn bootstrap_extracts_template_payload() {
        let tmp = tempdir_for_test();
        // Write a fake metadata + payload.
        let meta = EmbeddedMetadata {
            schema_version: 1,
            agent_name: "p16-test".to_string(),
            display_name: "P1.6 Test".to_string(),
            mode: BundleMode::Template,
            theme_default: "dark".to_string(),
            mur_version: "test".to_string(),
        };
        fs::write(
            tmp.join("metadata.json"),
            serde_json::to_string(&meta).unwrap(),
        )
        .unwrap();

        // Build a minimal tar.gz with a profile.yaml inside.
        let staging = tmp.join("payload-src");
        fs::create_dir_all(&staging).unwrap();
        fs::write(staging.join("profile.yaml"), "schema: 0\nname: p16-test\n").unwrap();
        let archive = tmp.join("agent-payload.tar.gz");
        {
            let f = fs::File::create(&archive).unwrap();
            let enc = flate2::write::GzEncoder::new(f, flate2::Compression::default());
            let mut tar = tar::Builder::new(enc);
            tar.append_dir_all(".", &staging).unwrap();
            let enc = tar.into_inner().unwrap();
            enc.finish().unwrap(); // flush gzip footer
        }

        // Use a fake MUR_HOME so we don't pollute the real one.
        let fake_mur = tmp.join("mur");
        // SAFETY: test runs serially; env vars are restored at end.
        unsafe {
            env::set_var("MUR_HOME", &fake_mur);
        }
        // Use a fake bin dir so we don't drop a symlink in ~/.local/bin.
        let fake_bin = tmp.join("bin");
        unsafe {
            env::set_var("MUR_AGENT_BIN_DIR", &fake_bin);
        }

        let result = bootstrap_if_needed(&tmp).unwrap();
        assert_eq!(result.agent_name, "p16-test");
        let extracted = fake_mur.join("agents/p16-test/profile.yaml");
        assert!(extracted.exists(), "profile.yaml not extracted");
        let id_pub = fake_mur.join("agents/p16-test/identity.pub");
        assert!(id_pub.exists(), "template identity.pub not minted");

        unsafe {
            env::remove_var("MUR_HOME");
        }
        unsafe {
            env::remove_var("MUR_AGENT_BIN_DIR");
        }
    }

    fn tempdir_for_test() -> PathBuf {
        let d = std::env::temp_dir().join(format!("mur-gui-bootstrap-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&d);
        fs::create_dir_all(&d).unwrap();
        d
    }
}
