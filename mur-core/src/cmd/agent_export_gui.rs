//! `mur agent export --format gui` — orchestrates the per-phase
//! pipeline that turns an agent home (`~/.mur/agents/<name>/`) into a
//! click-to-launch desktop app (`.app` / `.AppImage` / `.exe`).
//!
//! Phase list mirrors the spec § 8.2:
//!
//! Retired from CLI dispatch (--format=gui redirects to .muragent), preserved
//! as a reference for the Plan 3 Hub distribution pipeline.
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

#![allow(dead_code)]

use anyhow::{Context, Result, anyhow, bail};
use mur_common::bundle::{BundleMode, EmbeddedMetadata};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;
use tracing::{info, warn};

mod build;
mod prepare;

#[allow(unused_imports)]
pub use build::*;
#[allow(unused_imports)]
pub use prepare::*;

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

/// Apple notarytool credentials. Read from environment by
/// `phase_9_notarize`; passed into the pure `notarize_args` helper
/// so the helper itself is unit-testable without real creds.
pub struct NotarizeCreds {
    pub apple_id: String,
    pub team_id: String,
    pub password: String,
}

/// Build the argv vector for `xcrun notarytool submit ...`. Pure
/// function — no IO. Tested in `mur-core/tests/agent_export_macos.rs`.
pub fn notarize_args(zip_path: &Path, creds: &NotarizeCreds) -> Vec<String> {
    vec![
        "notarytool".to_string(),
        "submit".to_string(),
        zip_path.to_string_lossy().into_owned(),
        "--apple-id".to_string(),
        creds.apple_id.clone(),
        "--team-id".to_string(),
        creds.team_id.clone(),
        "--password".to_string(),
        creds.password.clone(),
        "--wait".to_string(),
        "--output-format".to_string(),
        "json".to_string(),
    ]
}

/// Build the argv vector for `xcrun stapler staple <bundle>`. Pure
/// function — no IO.
pub fn staple_args(bundle: &Path) -> Vec<String> {
    vec![
        "stapler".to_string(),
        "staple".to_string(),
        bundle.to_string_lossy().into_owned(),
    ]
}

/// Build the argv vector for `spctl --assess --type execute --verbose=4 <bundle>`.
/// Pure function — no IO.
pub fn assess_args(bundle: &Path) -> Vec<String> {
    vec![
        "--assess".to_string(),
        "--type".to_string(),
        "execute".to_string(),
        "--verbose=4".to_string(),
        bundle.to_string_lossy().into_owned(),
    ]
}

// BundleMode + EmbeddedMetadata moved to mur_common::bundle so the
// reader (mur-agent-gui's bootstrap module) can never drift from
// the writer (this file).

/// Public entry point — invoked from `cmd::agent::cmd_export` when
/// `--format gui` is selected.
pub fn run(opts: ExportGuiOptions) -> Result<()> {
    let started = Instant::now();
    info!(
        "mur agent export --format gui agent={} out={}",
        opts.agent_name,
        opts.out.display()
    );

    // SECURITY: Clone mode embeds identity.{key,pub} in the payload
    // and currently does NOT run the rekey ceremony on first launch
    // (bootstrap::run_clone_rekey is a stub). Until the ceremony
    // lands, refuse to ship clone-mode bundles to anyone other than
    // the operator producing them — require MUR_ALLOW_UNSAFE_CLONE=1.
    if opts.clone_identity && std::env::var("MUR_ALLOW_UNSAFE_CLONE").is_err() {
        bail!(
            "clone-mode bundle would ship the source agent's private key without \
             performing the rekey ceremony on the recipient's first launch \
             (rekey ceremony is currently a stub).\n\
             \n\
             If this is your own machine and you understand the risk, set\n\
             MUR_ALLOW_UNSAFE_CLONE=1 to proceed. Use template mode (default)\n\
             for any export shared beyond yourself."
        );
    }
    if opts.clone_identity {
        warn!(
            "clone-mode export proceeding under MUR_ALLOW_UNSAFE_CLONE — \
             recipient's first launch will NOT rotate the shipped key"
        );
    }

    let staging = staging_dir(&opts.agent_name)?;
    info!("staging dir: {}", staging.display());

    phase_1_prereq_check(&opts)?;
    let mode = phase_2_prepare_payload(&opts, &staging)?;
    phase_3_prepare_theme(&opts, &staging)?;
    phase_4_rewrite_tauri_conf(&opts, &staging, mode)?;

    // RAII guard: restore tauri.conf.json on any exit path including
    // panic. Explicit restore in the wrapper below remains for the
    // common success path; the guard catches everything else.
    let _guard = ConfRestoreGuard;

    // Wrap phases 5-13 so we always restore tauri.conf.json on the way
    // out, even on error. Phases 5 (build_sidecar — cargo) and 6
    // (build_frontend — npm) share no inputs/outputs so they run in
    // parallel scoped threads; saves ~30–60 s wall-clock per export.
    let result = (|| -> Result<()> {
        std::thread::scope(|s| -> Result<()> {
            let opts_5 = &opts;
            let staging_5 = staging.as_path();
            let opts_6 = &opts;
            let staging_6 = staging.as_path();
            let h5 = s.spawn(move || phase_5_build_sidecar(opts_5, staging_5));
            let h6 = s.spawn(move || phase_6_build_frontend(opts_6, staging_6));
            h5.join()
                .map_err(|_| anyhow!("build_sidecar thread panicked"))??;
            h6.join()
                .map_err(|_| anyhow!("build_frontend thread panicked"))??;
            Ok(())
        })?;
        phase_7_tauri_build(&opts, &staging)?;
        phase_8_codesign(&opts, &staging)?;
        phase_9_notarize(&opts, &staging)?;
        phase_10_staple(&opts, &staging)?;
        phase_11_assess(&opts, &staging)?;
        let bundle = phase_12_package(&opts, &staging)?;
        phase_13_move_to_out(&opts, &bundle)?;
        Ok(())
    })();

    result?;
    drop(_guard); // explicit drop on success — same effect as auto-drop

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

// ─── helpers ──────────────────────────────────────────────────────

fn staging_dir(agent_name: &str) -> Result<PathBuf> {
    let root = dirs::cache_dir()
        .ok_or_else(|| anyhow!("cannot resolve cache dir"))?
        .join("mur/export-gui");
    sweep_stale_staging(&root, agent_name);
    let dir = root.join(format!("{agent_name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Best-effort sweep of `~/.cache/mur/export-gui/<agent>-<pid>/`
/// directories left by previous runs that crashed or were killed.
/// Never bails — if a sibling export is racing, just leaves it.
fn sweep_stale_staging(root: &Path, current_agent: &str) {
    let Ok(entries) = std::fs::read_dir(root) else {
        return;
    };
    let prefix = format!("{current_agent}-");
    let our_pid = std::process::id();
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        // Match `<current_agent>-<pid>` or anything from a prior
        // version that no longer matches a live process.
        let Some(suffix) = name.strip_prefix(&prefix) else {
            continue;
        };
        let Ok(pid) = suffix.parse::<u32>() else {
            continue;
        };
        if pid == our_pid {
            continue;
        }
        // POSIX kill(0, pid) returns 0 if alive, ESRCH if dead.
        #[cfg(unix)]
        {
            // SAFETY: kill(0, pid) is a POSIX liveness probe.
            let alive = unsafe { libc::kill(pid as i32, 0) } == 0;
            if alive {
                continue;
            }
        }
        let _ = std::fs::remove_dir_all(entry.path());
        tracing::debug!(stale = name, "sweeping leftover staging dir");
    }
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

/// Re-tar `src` into `dst`, omitting any entry whose path basename
/// matches a name that could leak the source agent's identity.
/// Used by template-mode export to ensure the recipient's bootstrap
/// must mint fresh keys.
fn strip_identity_from_tarball(src: &Path, dst: &Path) -> Result<()> {
    use flate2::read::GzDecoder;
    use flate2::write::GzEncoder;
    use std::io::Read;

    const STRIP: &[&str] = &["identity.key", "identity.pub", "rotations.jsonl"];

    let in_file = std::fs::File::open(src).with_context(|| format!("open {}", src.display()))?;
    let mut decoder = tar::Archive::new(GzDecoder::new(in_file));

    let out_file =
        std::fs::File::create(dst).with_context(|| format!("create {}", dst.display()))?;
    let encoder = GzEncoder::new(out_file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    for entry in decoder.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        let basename = path
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or_default();
        if STRIP.contains(&basename) {
            tracing::debug!(stripped = %path.display(), "template mode: omitting from payload");
            continue;
        }
        let mut header = entry.header().clone();
        let size = header.size()?;
        let mut buf = Vec::with_capacity(size as usize);
        entry.read_to_end(&mut buf)?;
        builder.append_data(&mut header, &path, buf.as_slice())?;
    }

    let encoder = builder.into_inner()?;
    encoder.finish()?;
    Ok(())
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
mod tests;
