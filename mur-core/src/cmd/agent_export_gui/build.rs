use super::*;

// ─── phase 5 — build sidecar (mur-agent-runtime) ──────────────────

pub(super) fn phase_5_build_sidecar(_opts: &ExportGuiOptions, staging: &Path) -> Result<()> {
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

pub(super) fn phase_6_build_frontend(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
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

pub(super) fn phase_7_tauri_build(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
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

    // Tauri's `cargo tauri build` doesn't always rebundle resources
    // (metadata.json / agent-payload.tar.gz / themes/) when the
    // existing bundle output is fresher than its inputs — it may
    // reuse a stale bundle from a prior export. Clean the bundle
    // dir up front to guarantee a fresh layout per agent.
    let bundle_root = src_tauri
        .join("target")
        .join(&target)
        .join("release/bundle");
    let _ = std::fs::remove_dir_all(&bundle_root);

    info!("phase 7: cargo tauri build --target={target} --bundles={bundles}");
    let status = Command::new("cargo")
        .args(["tauri", "build", "--target", &target, "--bundles", bundles])
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

pub(super) fn phase_8_codesign(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) {
        info!("phase 8 (codesign) skipped on non-macOS host");
        return Ok(());
    }
    if opts.skip_notarize {
        info!("phase 8 (codesign) skipped: --skip-notarize");
        return Ok(());
    }
    let Ok(dev_id) = std::env::var("MUR_APPLE_DEVELOPER_ID") else {
        warn!("phase 8 (codesign) skipped: MUR_APPLE_DEVELOPER_ID not set");
        return Ok(());
    };
    let bundle = locate_bundle()?;
    let entitlements = workspace_gui_root()?.join("src-tauri/entitlements.plist");

    // Sign the embedded sidecar first, then the outer app --deep.
    let sidecar_glob = bundle.join("Contents/Resources/_up_/mur-agent-runtime");
    if sidecar_glob.exists() {
        codesign_one(&sidecar_glob, &dev_id, &entitlements)?;
    }
    codesign_one(&bundle, &dev_id, &entitlements)?;
    info!("phase 8 (codesign) ok");
    Ok(())
}

fn codesign_one(path: &Path, dev_id: &str, entitlements: &Path) -> Result<()> {
    let status = Command::new("codesign")
        .args([
            "--force",
            "--options",
            "runtime",
            "--timestamp",
            "--sign",
            dev_id,
            "--entitlements",
            &entitlements.to_string_lossy(),
            "--deep",
            &path.to_string_lossy(),
        ])
        .status()
        .with_context(|| format!("spawn codesign for {}", path.display()))?;
    if !status.success() {
        bail!("codesign failed for {} (exit={status})", path.display());
    }
    Ok(())
}

// ─── phase 9 — notarize ───────────────────────────────────────────

pub(super) fn phase_9_notarize(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) || opts.skip_notarize {
        return Ok(());
    }
    // Triple-env contract: same skip-on-missing rule as phase_8.
    // The notarize step needs the Apple ID, the team ID, and an
    // app-specific password (Apple does not accept the user's
    // primary password).  We expect:
    //   MUR_APPLE_NOTARY_KEY    — app-specific password
    //   MUR_APPLE_NOTARY_USER   — apple ID email
    //   MUR_APPLE_TEAM_ID       — 10-char team ID
    let Ok(password) = std::env::var("MUR_APPLE_NOTARY_KEY") else {
        warn!("phase 9 (notarize) skipped: MUR_APPLE_NOTARY_KEY not set");
        return Ok(());
    };
    let apple_id = std::env::var("MUR_APPLE_NOTARY_USER")
        .context("MUR_APPLE_NOTARY_USER (Apple ID email) required for notarization")?;
    let team_id = std::env::var("MUR_APPLE_TEAM_ID")
        .context("MUR_APPLE_TEAM_ID required for notarization")?;

    let bundle = locate_bundle()?;
    // notarytool wants a flat zip, not the .app directly.
    let zip_path = bundle.with_extension("zip");
    let zip_status = Command::new("ditto")
        .args([
            "-c",
            "-k",
            "--keepParent",
            &bundle.to_string_lossy(),
            &zip_path.to_string_lossy(),
        ])
        .status()
        .context("spawn ditto for notarize zip")?;
    if !zip_status.success() {
        bail!("ditto zip failed (exit={zip_status})");
    }

    let creds = NotarizeCreds {
        apple_id,
        team_id,
        password,
    };
    let args = notarize_args(&zip_path, &creds);
    let status = Command::new("xcrun")
        .args(&args)
        .status()
        .context("spawn xcrun notarytool submit")?;
    if !status.success() {
        bail!("notarytool submit failed (exit={status})");
    }
    info!("phase 9 (notarize) ok");
    Ok(())
}

// ─── phase 10/11 — staple + assess ────────────────────────────────

pub(super) fn phase_10_staple(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) || opts.skip_notarize {
        return Ok(());
    }
    // Skip cleanly when notarize was skipped for missing creds —
    // there's nothing for stapler to attach.
    if std::env::var("MUR_APPLE_NOTARY_KEY").is_err() {
        return Ok(());
    }
    let bundle = locate_bundle()?;
    let args = staple_args(&bundle);
    let status = Command::new("xcrun")
        .args(&args)
        .status()
        .context("spawn xcrun stapler staple")?;
    if !status.success() {
        bail!("stapler staple failed (exit={status})");
    }
    info!("phase 10 (staple) ok");
    Ok(())
}

pub(super) fn phase_11_assess(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
    if cfg!(not(target_os = "macos")) || opts.skip_notarize {
        return Ok(());
    }
    // Skip when codesign was skipped — there's nothing for spctl
    // to assess.
    if std::env::var("MUR_APPLE_DEVELOPER_ID").is_err() {
        return Ok(());
    }
    let bundle = locate_bundle()?;
    let args = assess_args(&bundle);
    let status = Command::new("spctl")
        .args(&args)
        .status()
        .context("spawn spctl --assess")?;
    if !status.success() {
        bail!(
            "spctl --assess rejected the bundle (exit={status}).\n\
             Run manually for details: spctl --assess --type execute \
             --verbose=4 {}",
            bundle.display()
        );
    }
    info!("phase 11 (assess) ok");
    Ok(())
}

// ─── phase 12 — package ───────────────────────────────────────────

pub(super) fn phase_12_package(_opts: &ExportGuiOptions, _staging: &Path) -> Result<PathBuf> {
    locate_bundle()
}

/// Locate the artifact `cargo tauri build` produced under
/// `mur-agent-gui/src-tauri/target/<triple>/release/bundle/<subdir>/`.
/// Used by both phase_8_codesign (which needs the bundle path before
/// signing) and phase_12_package. Pure read; safe to call multiple
/// times.
fn locate_bundle() -> Result<PathBuf> {
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
    // is patched per-agent in phase 4; the artifact is whatever Tauri
    // produced.
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path();
        let matches = match ext {
            "app" => path.is_dir() && path.extension().and_then(|s| s.to_str()) == Some("app"),
            other => path.extension().and_then(|s| s.to_str()) == Some(other),
        };
        if matches {
            return Ok(path);
        }
    }
    Err(anyhow!("no .{ext} artifact found in {}", dir.display()))
}

// ─── phase 13 — move to out ───────────────────────────────────────

pub(super) fn phase_13_move_to_out(opts: &ExportGuiOptions, bundle: &Path) -> Result<()> {
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
