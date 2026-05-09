//! `mur agent export` — package an agent as a portable bundle (pkg) or a
//! self-contained binary (bin).

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::resolve_mur_home;

pub fn cmd_export(name: &str, out: &str, format: &str) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    match format {
        "pkg" => {
            mur_agent_runtime::export::pkg::export_to_pkg(&agent_home, Path::new(out))?;
            println!("Exported '{name}' to {out}");
        }
        "bin" => export_bin(name, &agent_home, Path::new(out))?,
        other => bail!("unsupported export format '{other}' (use pkg or bin)"),
    }
    Ok(())
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

fn locate_runtime_manifest_dir() -> Result<PathBuf> {
    // Honour an explicit override (used by tests).
    if let Some(p) = std::env::var_os("MUR_AGENT_RUNTIME_MANIFEST_DIR") {
        return Ok(PathBuf::from(p));
    }
    // Walk up from the mur binary to the workspace and locate
    // `mur-agent-runtime/Cargo.toml`.
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
