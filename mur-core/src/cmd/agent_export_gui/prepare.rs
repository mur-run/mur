use super::*;

// ─── phase 1 — prereq check ────────────────────────────────────────

pub(super) fn phase_1_prereq_check(_opts: &ExportGuiOptions) -> Result<()> {
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

pub(super) fn phase_2_prepare_payload(
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

    // Template mode strips identity + rotation history; clone mode
    // preserves everything. We use the existing pkg::export_to_pkg
    // to produce a base tarball, then post-process for template
    // mode by re-tarring without identity.{key,pub} and rotations.jsonl.
    //
    // SECURITY: Shipping a "template" bundle that contains the
    // source agent's private key would be a class-A foot-gun
    // (see Cisco SSH host-key incident). Bootstrap mints a fresh
    // keypair anyway, so stripping is pure defence-in-depth.
    let raw_pkg = staging.join("agent.murpkg.tar.gz");
    mur_agent_runtime::export::pkg::export_to_pkg(&opts.agent_home, &raw_pkg)?;

    // The bundled tar.gz that the GUI app reads at first launch.
    let bundled = staging.join("agent-payload.tar.gz");
    match mode {
        BundleMode::Clone => {
            std::fs::copy(&raw_pkg, &bundled)?;
        }
        BundleMode::Template => {
            strip_identity_from_tarball(&raw_pkg, &bundled).with_context(|| {
                format!(
                    "strip identity from template-mode payload ({})",
                    raw_pkg.display()
                )
            })?;
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

pub(super) fn phase_3_prepare_theme(opts: &ExportGuiOptions, staging: &Path) -> Result<()> {
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

    // WCAG AA contrast gate — refuse to ship a theme that fails the
    // text + UI contrast thresholds documented in spec § 7.7. Same
    // logic exposed as `cargo test --lib theme::tests` for CI.
    let theme_json = std::fs::read_to_string(theme_src.join("theme.json"))?;
    let theme: serde_json::Value = serde_json::from_str(&theme_json)?;
    if let Some(failures) = wcag_contrast_failures(&theme)
        && !failures.is_empty()
    {
        bail!(
            "theme '{}' fails WCAG AA contrast:\n  {}",
            opts.theme,
            failures.join("\n  ")
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

    info!(
        "phase 3 (prepare_theme={}) ok in {:?}",
        opts.theme,
        span.elapsed()
    );
    Ok(())
}

// ─── WCAG AA helpers (mirror of mur-agent-gui::theme) ─────────────

pub(super) fn wcag_contrast_failures(theme: &serde_json::Value) -> Option<Vec<String>> {
    let colors = theme.get("colors")?.as_object()?;
    let name = theme.get("name").and_then(|v| v.as_str()).unwrap_or("?");
    let mut failures = Vec::new();
    let pairs: &[(&str, &str, f32, &str)] = &[
        ("fg", "bg", 4.5, "body text"),
        ("fg_secondary", "bg", 4.5, "secondary text"),
        ("accent_fg", "accent", 4.5, "accent button text"),
        ("border", "bg", 3.0, "UI border"),
    ];
    for (fg_k, bg_k, threshold, label) in pairs {
        let (Some(fg), Some(bg)) = (
            colors.get(*fg_k).and_then(|v| v.as_str()),
            colors.get(*bg_k).and_then(|v| v.as_str()),
        ) else {
            continue;
        };
        let (Some(fg_lum), Some(bg_lum)) = (relative_luminance(fg), relative_luminance(bg)) else {
            continue;
        };
        let r = contrast_ratio(fg_lum, bg_lum);
        if r < *threshold {
            failures.push(format!(
                "{name} '{label}': {fg_k} ({fg}) vs {bg_k} ({bg}) = {r:.2}:1, want ≥ {threshold}:1"
            ));
        }
    }
    Some(failures)
}

fn relative_luminance(hex: &str) -> Option<f32> {
    let s = hex.trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let r = u8::from_str_radix(&s[0..2], 16).ok()?;
    let g = u8::from_str_radix(&s[2..4], 16).ok()?;
    let b = u8::from_str_radix(&s[4..6], 16).ok()?;
    let f = |c: u8| {
        let cs = c as f32 / 255.0;
        if cs <= 0.03928 {
            cs / 12.92
        } else {
            ((cs + 0.055) / 1.055).powf(2.4)
        }
    };
    Some(0.2126 * f(r) + 0.7152 * f(g) + 0.0722 * f(b))
}

fn contrast_ratio(l1: f32, l2: f32) -> f32 {
    let (a, b) = if l1 > l2 { (l1, l2) } else { (l2, l1) };
    (a + 0.05) / (b + 0.05)
}

// ─── phase 4 — rewrite tauri.conf.json + stage payload ───────────

/// Patches tauri.conf.json IN PLACE under src-tauri/ so `cargo tauri
/// build` picks up the per-agent identifiers + payload resources.
/// The original is backed up to `<src-tauri>/tauri.conf.json.bak` and
/// restored by phase 12 (or the next export's prepare_payload, on
/// crash). Also drops `agent-payload.tar.gz` + `metadata.json` into
/// `src-tauri/` so bundle.resources can include them.
pub(super) fn phase_4_rewrite_tauri_conf(
    opts: &ExportGuiOptions,
    staging: &Path,
    _mode: BundleMode,
) -> Result<()> {
    let span = Instant::now();
    let gui_root = workspace_gui_root()?;
    let src_tauri = gui_root.join("src-tauri");
    let conf_path = src_tauri.join("tauri.conf.json");
    let backup_path = src_tauri.join("tauri.conf.json.bak");

    // Restore any leftover backup from a prior crashed run before
    // overwriting (so we don't lose the pristine template).
    if backup_path.exists() && !conf_path.exists() {
        std::fs::rename(&backup_path, &conf_path).ok();
    }
    std::fs::copy(&conf_path, &backup_path)
        .with_context(|| format!("backup {}", conf_path.display()))?;

    let body = std::fs::read_to_string(&conf_path)
        .with_context(|| format!("read {}", conf_path.display()))?;
    let mut conf: serde_json::Value =
        serde_json::from_str(&body).with_context(|| format!("parse {}", conf_path.display()))?;

    let safe_name = sanitize_for_bundle_id(&opts.agent_name);
    conf["productName"] = serde_json::json!(opts.agent_name.clone());
    conf["identifier"] = serde_json::json!(format!("run.mur.agent.{safe_name}"));
    conf["version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));

    // Ensure the bundle includes our staged payload + metadata.
    // Tauri allows `bundle.resources` to be either an array of paths
    // OR a {src: dest} map. Our static template uses the array form;
    // bail loudly if a future template change breaks that assumption
    // (silently skipping would produce a bundle without the payload
    // and phase 7 would emit an artifact that won't bootstrap).
    let bundle = conf
        .get_mut("bundle")
        .ok_or_else(|| anyhow!("tauri.conf.json: missing `bundle` block"))?;
    let resources = bundle.get_mut("resources").ok_or_else(|| {
        anyhow!("tauri.conf.json: `bundle.resources` block missing — template drift?")
    })?;
    let shape_label = match resources {
        serde_json::Value::Object(_) => "object",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        _ => "other",
    };
    let arr = resources.as_array_mut().ok_or_else(|| {
        anyhow!("tauri.conf.json: `bundle.resources` must be an array (got {shape_label})")
    })?;
    let want = ["agent-payload.tar.gz", "metadata.json"];
    for w in want {
        if !arr.iter().any(|v| v.as_str() == Some(w)) {
            arr.push(serde_json::json!(w));
        }
    }

    // Track C3 / M-c3.1.4 — substitute the per-agent slug into the
    // `muragent-{{AGENT_SLUG}}` placeholder under
    // `plugins.deep-link.desktop.schemes`. The shared helper is
    // re-used by `rewrite_url_scheme` for hermetic unit testing.
    apply_url_scheme_substitution(&mut conf, &safe_name);

    // Track C3 / M-w3 — render Info.plist from Info.plist.template
    // with the per-agent display name and point
    // `bundle.macOS.infoPlist` at the rendered file so Tauri merges
    // the `NSServices` entries into the final
    // `MyAgent.app/Contents/Info.plist`. macOS-only at runtime, but
    // the rewrite is harmless on Linux/Windows export targets — the
    // bundler simply ignores `bundle.macOS.*` there.
    if src_tauri.join("Info.plist.template").exists() {
        apply_nsservices_substitution(&mut conf);
        rewrite_nsservices(&src_tauri, &safe_name, &opts.agent_name)?;
    }

    std::fs::write(&conf_path, serde_json::to_string_pretty(&conf)?)?;

    // Copy staged payload + metadata next to tauri.conf.json so the
    // bundle resources spec finds them.
    std::fs::copy(
        staging.join("agent-payload.tar.gz"),
        src_tauri.join("agent-payload.tar.gz"),
    )?;
    std::fs::copy(
        staging.join("metadata.json"),
        src_tauri.join("metadata.json"),
    )?;

    info!(
        "phase 4 (rewrite_tauri_conf → {}, productName={}, identifier=run.mur.agent.{safe_name}) ok in {:?}",
        conf_path.display(),
        opts.agent_name,
        span.elapsed()
    );
    Ok(())
}

/// Walk a parsed `tauri.conf.json` value and replace every
/// `{{AGENT_SLUG}}` token under `plugins.deep-link.desktop.schemes`
/// with `slug`. Idempotent — running it twice is a no-op once the
/// substitution has happened. Tolerates a missing `plugins` block so
/// older configs (or future ones that drop deep-link) don't error.
fn apply_url_scheme_substitution(conf: &mut serde_json::Value, slug: &str) {
    let Some(schemes) = conf
        .get_mut("plugins")
        .and_then(|p| p.get_mut("deep-link"))
        .and_then(|d| d.get_mut("desktop"))
        .and_then(|d| d.get_mut("schemes"))
        .and_then(|s| s.as_array_mut())
    else {
        return;
    };
    for entry in schemes.iter_mut() {
        if let Some(s) = entry.as_str() {
            let replaced = s.replace("{{AGENT_SLUG}}", slug);
            *entry = serde_json::Value::String(replaced);
        }
    }
}

/// Stand-alone hermetic entry point that the integration test calls
/// without spinning up the full export pipeline. Reads
/// `tauri_conf_dir/tauri.conf.json`, applies the slug substitution,
/// writes back. Production callers route through
/// `phase_4_rewrite_tauri_conf`, which calls
/// `apply_url_scheme_substitution` directly to avoid the disk
/// round-trip; this wrapper exists so the unit test can verify the
/// disk-side semantics in isolation.
/// Track C3 / M-w3 — point `bundle.macOS.infoPlist` at the rendered
/// `Info.plist` so the Tauri bundler merges its `NSServices` array
/// into the final `MyAgent.app/Contents/Info.plist`.
///
/// Idempotent — running it twice yields the same conf. Tolerates a
/// missing `bundle.macOS` block (creates it) so future template
/// refactors that drop the macOS section don't break export.
fn apply_nsservices_substitution(conf: &mut serde_json::Value) {
    let bundle = conf.get_mut("bundle").and_then(|b| b.as_object_mut());
    let Some(bundle) = bundle else {
        return;
    };
    let macos = bundle
        .entry("macOS".to_string())
        .or_insert_with(|| serde_json::json!({}));
    if let Some(obj) = macos.as_object_mut() {
        obj.insert(
            "infoPlist".to_string(),
            serde_json::Value::String("Info.plist".to_string()),
        );
    }
}

#[allow(dead_code)] // Reached only by `tests/agent_export_gui_url_scheme.rs`.
pub fn rewrite_url_scheme(tauri_conf_dir: &Path, slug: &str) -> Result<()> {
    let path = tauri_conf_dir.join("tauri.conf.json");
    let raw = std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut conf: serde_json::Value =
        serde_json::from_str(&raw).with_context(|| format!("parse {}", path.display()))?;
    apply_url_scheme_substitution(&mut conf, slug);
    std::fs::write(&path, serde_json::to_string_pretty(&conf)?)
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Track C3 / M-c3.3.3 — substitute `{{AGENT_DISPLAY}}` in
/// `Info.plist.template` and write the result back to `Info.plist`
/// in the same directory.
///
/// `slug` is currently unused (Apple's `NSServices` array doesn't carry
/// the kebab-case slug; menu titles + `NSPortName` use the human display
/// name). It's wired through anyway so callers can match the
/// `rewrite_url_scheme(slug)` signature and so future template fields
/// (e.g. dock-drop UTI suffixes) can opt in without a signature break.
///
/// The output filename intentionally drops the `.template` suffix —
/// `bundle.macOS.infoPlist` in `tauri.conf.json` is expected to point
/// at `Info.plist` (the rendered file), not `Info.plist.template`.
#[allow(dead_code)] // Reached only by `tests/agent_export_gui_nsservices.rs`.
pub fn rewrite_nsservices(tauri_conf_dir: &Path, _slug: &str, display: &str) -> Result<()> {
    let template = tauri_conf_dir.join("Info.plist.template");
    let raw = std::fs::read_to_string(&template)
        .with_context(|| format!("read {}", template.display()))?;
    let rendered = raw.replace("{{AGENT_DISPLAY}}", display);
    let out = tauri_conf_dir.join("Info.plist");
    std::fs::write(&out, rendered).with_context(|| format!("write {}", out.display()))?;
    Ok(())
}

/// Restore the pristine tauri.conf.json after the build (called from
/// phase_13 success path AND from any failure; idempotent).
fn restore_tauri_conf() -> Result<()> {
    let gui_root = workspace_gui_root()?;
    let conf_path = gui_root.join("src-tauri/tauri.conf.json");
    let backup_path = gui_root.join("src-tauri/tauri.conf.json.bak");
    if backup_path.exists() {
        std::fs::rename(&backup_path, &conf_path)?;
    }
    // Clean up the per-export drop-ins.
    let _ = std::fs::remove_file(gui_root.join("src-tauri/agent-payload.tar.gz"));
    let _ = std::fs::remove_file(gui_root.join("src-tauri/metadata.json"));
    // Track C3 / M-w3 — the rendered Info.plist is per-agent build
    // output (the .template stays); drop it so a follow-up export
    // for a different agent doesn't accidentally bundle the prior
    // agent's NSServices entries.
    let _ = std::fs::remove_file(gui_root.join("src-tauri/Info.plist"));
    Ok(())
}

/// RAII guard that restores `tauri.conf.json` on drop, so a panic /
/// SIGINT mid-build doesn't leave the source tree dirty. Belt and
/// braces with the explicit finally-style restore in `run()`.
pub(super) struct ConfRestoreGuard;

impl Drop for ConfRestoreGuard {
    fn drop(&mut self) {
        if let Err(e) = restore_tauri_conf() {
            warn!("ConfRestoreGuard drop: {e:#}");
        }
    }
}
