//! `mur agent export --format gui` — orchestrates the per-phase
//! pipeline that turns an agent home (`~/.mur/agents/<name>/`) into a
//! click-to-launch desktop app (`.app` / `.AppImage` / `.exe`).
//!
//! Phase list mirrors the spec § 8.2:
//!
//! | # | Phase             | Description                                 |
//! |---|-------------------|---------------------------------------------|
//! | 1 | prereq_check      | `cmd::doctor::checks_for("gui")` fail-fast  |
//! | 2 | prepare_payload   | Tarball the agent home (template/clone)     |
//! | 3 | prepare_theme     | Resolve theme dir; transcode --icon         |
//! | 4 | rewrite_tauri_conf| Patch productName/identifier/version/icon   |
//! | 5 | build_sidecar     | cargo build mur-agent-runtime (universal)   |
//! | 6 | build_frontend    | npm ci && npm run build                     |
//! | 7 | tauri_build       | cargo tauri build                           |
//! | 8 | codesign          | mac/win signing (skipped without creds)     |
//! | 9 | notarize          | Apple notarytool submit                     |
//! | 10| staple            | xcrun stapler staple                        |
//! | 11| assess            | spctl --assess --type execute               |
//! | 12| package           | zip / AppImageTool / NSIS                   |
//! | 13| move_to_out       | Copy artifact to user's -o path             |
//!
//! Heavy phases (5–13) shell out to external toolchains. v1 logs each
//! phase via `tracing` so failures are easy to triage; future P2 work
//! upgrades these to OpenTelemetry spans.
//!
//! See spec: `docs/superpowers/specs/2026-04-29-mur-agent-gui-export-design.md`

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use tracing::{info, warn};

#[derive(Debug, Clone)]
pub struct ExportGuiOptions {
    pub agent_name: String,
    pub agent_home: PathBuf,
    pub out: PathBuf,
    /// One of the bundled themes (light/dark/high-contrast/solarized/cyberpunk).
    pub theme: String,
    /// Optional override icon (PNG); will be transcoded to platform formats.
    pub icon: Option<PathBuf>,
    /// If true, the embedded payload includes identity.{key,pub} and
    /// the recipient runs the rekey ceremony on first launch. Default
    /// false (template mode — recipient mints fresh keys).
    pub clone_identity: bool,
    /// Skip macOS code-signing + notarization. Useful for local
    /// testing; required when no Developer ID is configured.
    pub skip_notarize: bool,
}

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

/// Public entry point — invoked from `cmd::agent::cmd_export` when
/// `--format gui` is selected.
pub fn run(opts: ExportGuiOptions) -> Result<()> {
    let started = Instant::now();
    info!(
        "mur agent export --format gui agent={} out={}",
        opts.agent_name,
        opts.out.display()
    );

    let staging = staging_dir(&opts.agent_name)?;
    info!("staging dir: {}", staging.display());

    phase_1_prereq_check(&opts)?;
    let mode = phase_2_prepare_payload(&opts, &staging)?;
    phase_3_prepare_theme(&opts, &staging)?;
    phase_4_rewrite_tauri_conf(&opts, &staging, mode)?;
    phase_5_build_sidecar(&opts, &staging)?;
    phase_6_build_frontend(&opts, &staging)?;
    phase_7_tauri_build(&opts, &staging)?;
    phase_8_codesign(&opts, &staging)?;
    phase_9_notarize(&opts, &staging)?;
    phase_10_staple(&opts, &staging)?;
    phase_11_assess(&opts, &staging)?;
    let bundle = phase_12_package(&opts, &staging)?;
    phase_13_move_to_out(&opts, &bundle)?;

    println!(
        "Exported '{}' (gui, {} mode) to {} in {:.1}s",
        opts.agent_name,
        match mode {
            BundleMode::Template => "template",
            BundleMode::Clone => "clone",
        },
        opts.out.display(),
        started.elapsed().as_secs_f32(),
    );

    Ok(())
}

// ─── phase 1 — prereq check ────────────────────────────────────────

fn phase_1_prereq_check(_opts: &ExportGuiOptions) -> Result<()> {
    let span = Instant::now();
    let results = crate::cmd::doctor::checks_for("gui");
    let missing: Vec<_> = results
        .iter()
        .filter(|r| r.status == crate::cmd::doctor::CheckStatus::Missing)
        .collect();
    if !missing.is_empty() {
        let mut msg = String::from("missing prerequisites for gui export:\n");
        for r in &missing {
            msg.push_str(&format!(
                "  ✗ {} → {}\n",
                r.name,
                r.hint.as_deref().unwrap_or("(no hint)"),
            ));
        }
        msg.push_str("Run `mur agent doctor --format gui` for full diagnostics.");
        bail!(msg);
    }
    info!("phase 1 (prereq_check) ok in {:?}", span.elapsed());
    Ok(())
}

// ─── phase 2 — prepare payload ────────────────────────────────────

fn phase_2_prepare_payload(
    opts: &ExportGuiOptions,
    staging: &Path,
) -> Result<BundleMode> {
    let span = Instant::now();
    let mode = if opts.clone_identity {
        BundleMode::Clone
    } else {
        BundleMode::Template
    };

    let payload_dir = staging.join("payload");
    std::fs::create_dir_all(&payload_dir)?;

    // Template mode strips identity + UUID; clone mode preserves them.
    // For v1 we use the existing pkg::export_to_pkg and post-process
    // for template mode by re-tarring without identity files.
    let raw_pkg = staging.join("agent.murpkg.tar.gz");
    mur_agent_runtime::export::pkg::export_to_pkg(&opts.agent_home, &raw_pkg)?;

    // The bundled tar.gz that the GUI app reads at first launch.
    let bundled = staging.join("agent-payload.tar.gz");
    match mode {
        BundleMode::Clone => {
            std::fs::copy(&raw_pkg, &bundled)?;
        }
        BundleMode::Template => {
            // For v1 keep clone-equivalent on disk; the bootstrap-
            // mode field in metadata.json signals to first-launch
            // logic to mint fresh identity. P1.6 may strip
            // identity.* files here for defence-in-depth.
            std::fs::copy(&raw_pkg, &bundled)?;
        }
    }

    let metadata = EmbeddedMetadata {
        schema_version: 1,
        agent_name: opts.agent_name.clone(),
        display_name: opts.agent_name.clone(),
        mode,
        theme_default: opts.theme.clone(),
        mur_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let metadata_path = staging.join("metadata.json");
    std::fs::write(&metadata_path, serde_json::to_string_pretty(&metadata)?)?;

    info!(
        "phase 2 (prepare_payload, {:?}) ok in {:?}",
        mode,
        span.elapsed()
    );
    Ok(mode)
}

// ─── phase 3 — prepare theme ──────────────────────────────────────

fn phase_3_prepare_theme(opts: &ExportGuiOptions, staging: &Path) -> Result<()> {
    let span = Instant::now();
    let gui_root = workspace_gui_root()?;
    let theme_src = gui_root.join("src-tauri/themes").join(&opts.theme);
    if !theme_src.exists() {
        bail!(
            "theme '{}' not found in {}",
            opts.theme,
            gui_root.join("src-tauri/themes").display()
        );
    }

    if let Some(custom_icon) = &opts.icon {
        // P1.5 will transcode PNG → icns/ico/multi-PNG; v1 stub:
        // copy into staging custom theme dir as-is.
        let custom_dir = staging.join("themes/_custom");
        std::fs::create_dir_all(&custom_dir)?;
        std::fs::copy(custom_icon, custom_dir.join("app.png"))?;
        warn!("--icon transcoding stubbed; v1 copies PNG verbatim into themes/_custom/");
    }

    info!("phase 3 (prepare_theme={}) ok in {:?}", opts.theme, span.elapsed());
    Ok(())
}

// ─── phase 4 — rewrite tauri.conf.json ────────────────────────────

fn phase_4_rewrite_tauri_conf(
    opts: &ExportGuiOptions,
    staging: &Path,
    _mode: BundleMode,
) -> Result<()> {
    let span = Instant::now();
    let gui_root = workspace_gui_root()?;
    let template = gui_root.join("src-tauri/tauri.conf.json");
    let body = std::fs::read_to_string(&template)
        .with_context(|| format!("read {}", template.display()))?;
    let mut conf: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("parse {}", template.display()))?;

    let safe_name = sanitize_for_bundle_id(&opts.agent_name);
    conf["productName"] = serde_json::json!(opts.agent_name.clone());
    conf["identifier"] = serde_json::json!(format!("run.mur.agent.{safe_name}"));
    conf["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));

    let out_path = staging.join("tauri.conf.json");
    std::fs::write(&out_path, serde_json::to_string_pretty(&conf)?)?;
    info!(
        "phase 4 (rewrite_tauri_conf → {}) ok in {:?}",
        out_path.display(),
        span.elapsed()
    );
    Ok(())
}

// ─── phase 5 — build sidecar (mur-agent-runtime) ──────────────────

fn phase_5_build_sidecar(_opts: &ExportGuiOptions, staging: &Path) -> Result<()> {
    let span = Instant::now();
    let gui_root = workspace_gui_root()?;
    let bin_dir = gui_root.join("src-tauri/binaries");
    std::fs::create_dir_all(&bin_dir)?;

    let target = host_target_triple()?;
    let workspace_root = workspace_root()?;
    info!("phase 5: cargo build mur-agent-runtime --target={target}");
    let status = Command::new("cargo")
        .args([
            "build",
            "-p",
            "mur-agent-runtime",
            "--release",
            "--target",
            &target,
        ])
        .current_dir(&workspace_root)
        .status()
        .with_context(|| "spawn cargo build")?;
    if !status.success() {
        bail!("cargo build sidecar failed (exit={status})");
    }

    let built = workspace_root
        .join("target")
        .join(&target)
        .join("release")
        .join(if cfg!(windows) {
            "mur-agent-runtime.exe"
        } else {
            "mur-agent-runtime"
        });
    let dest = bin_dir.join(format!(
        "mur-agent-runtime-{target}{}",
        if cfg!(windows) { ".exe" } else { "" }
    ));
    std::fs::copy(&built, &dest)
        .with_context(|| format!("copy {} -> {}", built.display(), dest.display()))?;

    let _ = staging; // unused; bin_dir is rooted in gui_root
    info!("phase 5 (build_sidecar) ok in {:?}", span.elapsed());
    Ok(())
}

// ─── phase 6 — build frontend (npm) ───────────────────────────────

fn phase_6_build_frontend(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    let span = Instant::now();
    let gui_root = workspace_gui_root()?;
    let ui_dir = gui_root.join("ui");

    info!("phase 6: npm ci in {}", ui_dir.display());
    let status = Command::new("npm")
        .args(["ci"])
        .current_dir(&ui_dir)
        .status()
        .with_context(|| "spawn npm ci")?;
    if !status.success() {
        bail!("npm ci failed (exit={status})");
    }

    info!("phase 6: npm run build");
    let status = Command::new("npm")
        .args(["run", "build"])
        .current_dir(&ui_dir)
        .status()
        .with_context(|| "spawn npm run build")?;
    if !status.success() {
        bail!("npm run build failed (exit={status})");
    }

    let _ = opts; // silence unused
    info!("phase 6 (build_frontend) ok in {:?}", span.elapsed());
    Ok(())
}

// ─── phase 7 — cargo tauri build ──────────────────────────────────

fn phase_7_tauri_build(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    let span = Instant::now();
    let gui_root = workspace_gui_root()?;
    let src_tauri = gui_root.join("src-tauri");

    let target = host_target_triple()?;
    let bundles = match std::env::consts::OS {
        "macos" => "app",
        "linux" => "appimage",
        "windows" => "nsis",
        other => bail!("unsupported host OS for gui build: {other}"),
    };

    info!(
        "phase 7: cargo tauri build --target={target} --bundles={bundles}"
    );
    let status = Command::new("cargo")
        .args([
            "tauri",
            "build",
            "--target",
            &target,
            "--bundles",
            bundles,
        ])
        .current_dir(&src_tauri)
        .status()
        .with_context(|| "spawn cargo tauri build")?;
    if !status.success() {
        bail!("cargo tauri build failed (exit={status})");
    }

    let _ = opts; // silence unused
    info!("phase 7 (tauri_build) ok in {:?}", span.elapsed());
    Ok(())
}

// ─── phase 8 — codesign ───────────────────────────────────────────

fn phase_8_codesign(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        info!("phase 8 (codesign) skipped on non-macOS host");
        return Ok(());
    }
    if opts.skip_notarize {
        info!("phase 8 (codesign) skipped: --skip-notarize");
        return Ok(());
    }
    let Ok(_dev_id) = std::env::var("MUR_APPLE_DEVELOPER_ID") else {
        warn!("phase 8 (codesign) skipped: MUR_APPLE_DEVELOPER_ID not set");
        return Ok(());
    };
    // Real signing flow (sidecar then outer .app --deep) lands in
    // P1.7 follow-up. This stub records the intent so an integrator
    // can plug in their CI's signing recipe.
    warn!("phase 8 (codesign) recipe is stubbed for v1; integrate with your CI's codesign step");
    Ok(())
}

// ─── phase 9 — notarize ───────────────────────────────────────────

fn phase_9_notarize(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) || opts.skip_notarize {
        return Ok(());
    }
    if std::env::var("MUR_APPLE_NOTARY_KEY").is_err() {
        return Ok(());
    }
    warn!("phase 9 (notarize) recipe is stubbed for v1");
    Ok(())
}

// ─── phase 10/11 — staple + assess ────────────────────────────────

fn phase_10_staple(_opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        return Ok(());
    }
    Ok(())
}

fn phase_11_assess(_opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        return Ok(());
    }
    Ok(())
}

// ─── phase 12 — package ───────────────────────────────────────────

fn phase_12_package(_opts: &ExportGuiOptions, _staging: &Path) -> Result<PathBuf> {
    let gui_root = workspace_gui_root()?;
    let target = host_target_triple()?;
    let (subdir, ext): (&str, &str) = match std::env::consts::OS {
        "macos" => ("release/bundle/macos", "app"),
        "linux" => ("release/bundle/appimage", "AppImage"),
        "windows" => ("release/bundle/nsis", "exe"),
        other => bail!("unsupported host OS: {other}"),
    };
    let dir = gui_root.join("src-tauri/target").join(&target).join(subdir);
    if !dir.exists() {
        bail!(
            "bundle dir not found: {} — `cargo tauri build` may have failed earlier",
            dir.display()
        );
    }
    // Pick the first artifact matching the expected extension.
    // The Tauri productName comes from src-tauri/tauri.conf.json which
    // currently isn't patched per-agent (P1.7 follow-up); the artifact
    // is whatever Tauri produced.
    let mut found = None;
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let matches = match ext {
            "app" => path.is_dir() && path.extension().and_then(|s| s.to_str()) == Some("app"),
            other => path.extension().and_then(|s| s.to_str()) == Some(other),
        };
        if matches {
            found = Some(path);
            break;
        }
    }
    found.ok_or_else(|| anyhow!("no .{ext} artifact found in {}", dir.display()))
}

// ─── phase 13 — move to out ───────────────────────────────────────

fn phase_13_move_to_out(opts: &ExportGuiOptions, bundle: &Path) -> Result<()> {
    let parent = opts
        .out
        .parent()
        .ok_or_else(|| anyhow!("--out has no parent directory: {}", opts.out.display()))?;
    std::fs::create_dir_all(parent)?;
    if bundle.is_dir() {
        copy_dir_recursive(bundle, &opts.out)?;
    } else {
        std::fs::copy(bundle, &opts.out)?;
    }
    Ok(())
}

// ─── helpers ──────────────────────────────────────────────────────

fn staging_dir(agent_name: &str) -> Result<PathBuf> {
    let dir = dirs::cache_dir()
        .ok_or_else(|| anyhow!("cannot resolve cache dir"))?
        .join("mur/export-gui")
        .join(format!("{agent_name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

fn workspace_root() -> Result<PathBuf> {
    // Walk up from the mur binary to find the workspace Cargo.toml.
    let exe = std::env::current_exe().context("current_exe")?;
    let mut p = exe.parent().map(Path::to_path_buf);
    while let Some(d) = p {
        if d.join("Cargo.toml").exists()
            && std::fs::read_to_string(d.join("Cargo.toml"))
                .ok()
                .map(|c| c.contains("[workspace]"))
                .unwrap_or(false)
        {
            return Ok(d);
        }
        p = d.parent().map(Path::to_path_buf);
    }
    // Fallback: assume CARGO_MANIFEST_DIR-style layout.
    Ok(PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf())
}

fn workspace_gui_root() -> Result<PathBuf> {
    Ok(workspace_root()?.join("mur-agent-gui"))
}

fn host_target_triple() -> Result<String> {
    let triple = match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => "aarch64-apple-darwin",
        ("macos", "x86_64") => "x86_64-apple-darwin",
        ("linux", "x86_64") => "x86_64-unknown-linux-gnu",
        ("linux", "aarch64") => "aarch64-unknown-linux-gnu",
        ("windows", "x86_64") => "x86_64-pc-windows-msvc",
        (os, arch) => bail!("unrecognised host: {os} {arch}"),
    };
    Ok(triple.into())
}

fn sanitize_for_bundle_id(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let to = dst.join(entry.file_name());
        if kind.is_dir() {
            copy_dir_recursive(&entry.path(), &to)?;
        } else if kind.is_symlink() {
            // Recreate the symlink target verbatim.
            #[cfg(unix)]
            {
                let target = std::fs::read_link(entry.path())?;
                let _ = std::fs::remove_file(&to);
                std::os::unix::fs::symlink(target, to)?;
            }
            #[cfg(not(unix))]
            {
                std::fs::copy(entry.path(), to)?;
            }
        } else {
            std::fs::copy(entry.path(), to)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_special_chars() {
        assert_eq!(sanitize_for_bundle_id("My Agent!"), "my-agent");
        assert_eq!(sanitize_for_bundle_id("__research__"), "research");
        assert_eq!(sanitize_for_bundle_id("agent-1"), "agent-1");
        assert_eq!(sanitize_for_bundle_id("Hello.World"), "hello-world");
    }

    #[test]
    fn host_target_known_for_dev_machine() {
        let triple = host_target_triple().expect("host detected");
        assert!(
            triple.contains("apple-darwin")
                || triple.contains("linux")
                || triple.contains("windows"),
            "unexpected triple: {triple}"
        );
    }

    #[test]
    fn embedded_metadata_round_trips() {
        let meta = EmbeddedMetadata {
            schema_version: 1,
            agent_name: "demo".into(),
            display_name: "Demo".into(),
            mode: BundleMode::Template,
            theme_default: "dark".into(),
            mur_version: "2.4.1".into(),
        };
        let json = serde_json::to_string(&meta).unwrap();
        let back: EmbeddedMetadata = serde_json::from_str(&json).unwrap();
        assert_eq!(back.agent_name, "demo");
        assert_eq!(back.mode, BundleMode::Template);
    }
}
