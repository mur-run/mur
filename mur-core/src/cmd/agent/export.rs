//! `mur agent export` — package an agent as a portable bundle.
//!
//! Default format is `.muragent` (signed v2, see spec §6). Legacy `.murpkg`
//! continues to be available via `--format=pkg`, and the self-contained
//! binary build via `--format=bin`. The GUI `.app` export is dispatched
//! separately in `dispatch.rs` (the `gui` format never reaches this fn).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use mur_common::AgentProfile;
use mur_common::agent::NotificationTarget;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};

use super::resolve_mur_home;

pub fn cmd_export(name: &str, out: &str, format: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    match format {
        "muragent" => export_muragent(name, &agent_home, Path::new(out))?,
        "pkg" => {
            mur_agent_runtime::export::pkg::export_to_pkg(&agent_home, Path::new(out))?;
            println!("Exported '{name}' to {out}");
        }
        "bin" => export_bin(name, &agent_home, Path::new(out))?,
        other => bail!("unsupported export format '{other}' (use muragent, pkg, bin, or gui)"),
    }
    Ok(())
}

fn export_muragent(name: &str, agent_home: &Path, out: &Path) -> Result<()> {
    let profile_path = agent_home.join("profile.yaml");
    let profile_yaml = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let profile: AgentProfile = serde_yaml_ng::from_str(&profile_yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;

    let identity = AgentIdentity::load(agent_home)
        .with_context(|| format!("load agent identity from {}", agent_home.display()))?;

    let mur_version = env!("CARGO_PKG_VERSION");
    let mut manifest = build_manifest_from_profile(&profile, mur_version);

    let mut export_profile = profile.clone();
    let removed = sanitize_profile_for_export(&mut export_profile);
    manifest.sanitized.removed_fields = removed;
    let sanitized_yaml =
        serde_yaml_ng::to_string(&export_profile).context("serialize sanitized profile")?;

    let mut writer = MuragentWriter::new(manifest, sanitized_yaml, identity);

    let icon_base = agent_home.join("icon");
    for (filename, tar_name) in &[
        ("icon.icns", "icon.icns"),
        ("icon.ico", "icon.ico"),
        ("icon-512.png", "icon-512.png"),
    ] {
        let path = icon_base.join(filename);
        if path.exists() {
            let data = fs::read(&path).with_context(|| format!("read {}", path.display()))?;
            writer.add_icon(tar_name, data);
        }
    }

    let voice_yaml_path = agent_home.join("voice.yaml");
    if voice_yaml_path.exists() {
        let voice = fs::read_to_string(&voice_yaml_path)
            .with_context(|| format!("read {}", voice_yaml_path.display()))?;
        writer.set_voice_yaml(voice);
    }

    writer
        .write(out)
        .with_context(|| format!("write .muragent to {}", out.display()))?;

    println!("Exported '{name}' → {}", out.display());
    Ok(())
}

/// Strip identity-private and secretful targets from the profile copy
/// that will land inside the `.muragent`. Returns the list of removed
/// field paths (consumed by the manifest's `sanitized.removed_fields`).
fn sanitize_profile_for_export(profile: &mut AgentProfile) -> Vec<String> {
    let mut removed = vec!["identity.private_key".to_string()];

    let strip_secretful =
        |targets: &mut Vec<NotificationTarget>, path: &str, out: &mut Vec<String>| {
            let before = targets.len();
            targets.retain(|t| !is_secretful(t));
            if targets.len() != before {
                out.push(path.to_string());
            }
        };
    strip_secretful(
        &mut profile.notifications.on_task_complete,
        "notifications.on_task_complete[secret]",
        &mut removed,
    );
    strip_secretful(
        &mut profile.notifications.on_error,
        "notifications.on_error[secret]",
        &mut removed,
    );
    strip_secretful(
        &mut profile.notifications.on_shutdown,
        "notifications.on_shutdown[secret]",
        &mut removed,
    );

    if profile.transport.socket.auth.is_some() {
        removed.push("transport.socket.auth".to_string());
        profile.transport.socket.auth = None;
    }
    if profile.model_ref.is_some() {
        removed.push("model_ref".to_string());
        profile.model_ref = None;
    }
    removed
}

fn is_secretful(t: &NotificationTarget) -> bool {
    matches!(
        t,
        NotificationTarget::Webhook { .. }
            | NotificationTarget::Slack { .. }
            | NotificationTarget::Webpush { .. }
            | NotificationTarget::Email { .. }
    )
}

/// Drive `cargo build -p mur-agent-runtime --release --features=embedded-agent`
/// with MUR_EXPORT_AGENT_DIR=<agent_home>, then copy the built binary to `out`.
fn export_bin(name: &str, agent_home: &Path, out: &Path) -> Result<()> {
    let target_dir = std::env::temp_dir().join(format!("mur-export-{name}-{}", std::process::id()));
    fs::create_dir_all(&target_dir).with_context(|| format!("create {}", target_dir.display()))?;

    let manifest_dir = locate_runtime_manifest_dir().context("locate mur-agent-runtime crate")?;

    let status = std::process::Command::new("cargo")
        .args([
            "build",
            "--release",
            "--features=embedded-agent",
            "--manifest-path",
        ])
        .arg(manifest_dir.join("Cargo.toml"))
        .env("MUR_EXPORT_AGENT_DIR", agent_home)
        .env("CARGO_TARGET_DIR", &target_dir)
        .status()
        .context("invoke cargo build")?;
    if !status.success() {
        bail!("cargo build failed: {status}");
    }
    let built = target_dir.join("release").join(if cfg!(windows) {
        "mur-agent-runtime.exe"
    } else {
        "mur-agent-runtime"
    });
    fs::copy(&built, out)
        .with_context(|| format!("copy {} -> {}", built.display(), out.display()))?;
    println!("Built self-contained agent binary at {}", out.display());
    Ok(())
}

#[cfg(test)]
mod sanitize_tests {
    use super::*;
    use mur_common::AgentProfile;

    #[test]
    fn sanitize_strips_model_ref() {
        let mut p = AgentProfile::default_for_tests();
        p.model_ref = Some("anthropic_opus_4_7".into());
        let removed = sanitize_profile_for_export(&mut p);
        assert!(p.model_ref.is_none(), "model_ref must be stripped");
        assert!(
            removed.iter().any(|r| r == "model_ref"),
            "removed list must record model_ref, got {removed:?}"
        );
    }
}

fn locate_runtime_manifest_dir() -> Result<PathBuf> {
    if let Some(p) = std::env::var_os("MUR_AGENT_RUNTIME_MANIFEST_DIR") {
        return Ok(PathBuf::from(p));
    }
    let exe = std::env::current_exe().context("current_exe")?;
    let mut cur = exe.parent().map(|p| p.to_path_buf());
    while let Some(d) = cur {
        let candidate = d.join("mur-agent-runtime").join("Cargo.toml");
        if candidate.exists() {
            return Ok(d.join("mur-agent-runtime"));
        }
        cur = d.parent().map(|p| p.to_path_buf());
    }
    bail!(
        "could not locate mur-agent-runtime crate (set MUR_AGENT_RUNTIME_MANIFEST_DIR to override)"
    )
}
