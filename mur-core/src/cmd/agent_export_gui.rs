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
use mur_common::bundle::{BundleMode, EmbeddedMetadata};
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

fn phase_2_prepare_payload(opts: &ExportGuiOptions, staging: &Path) -> Result<BundleMode> {
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

fn wcag_contrast_failures(theme: &serde_json::Value) -> Option<Vec<String>> {
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
fn phase_4_rewrite_tauri_conf(
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
    let bundle = conf
        .get_mut("bundle")
        .and_then(|b| b.as_object_mut());
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
struct ConfRestoreGuard;

impl Drop for ConfRestoreGuard {
    fn drop(&mut self) {
        if let Err(e) = restore_tauri_conf() {
            warn!("ConfRestoreGuard drop: {e:#}");
        }
    }
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

fn phase_8_codesign(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
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

fn phase_9_notarize(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
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

fn phase_10_staple(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
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

fn phase_11_assess(opts: &ExportGuiOptions, _staging: &Path) -> Result<()> {
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

fn phase_12_package(_opts: &ExportGuiOptions, _staging: &Path) -> Result<PathBuf> {
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

    // ─── WCAG AA validator (build-time gate inside phase_3) ──────

    #[test]
    fn wcag_passes_for_high_contrast_palette() {
        let theme = serde_json::json!({
            "name": "test-pass",
            "colors": {
                "bg": "#000000",
                "fg": "#ffffff",
                "fg_secondary": "#cccccc",
                "accent": "#ffff00",
                "accent_fg": "#000000",
                "border": "#888888"
            }
        });
        let failures = wcag_contrast_failures(&theme).expect("colors object present");
        assert!(failures.is_empty(), "unexpected failures: {failures:?}");
    }

    #[test]
    fn wcag_flags_low_contrast_body_text() {
        // fg #444 vs bg #555 → 1.04:1 ratio (way below 4.5:1).
        let theme = serde_json::json!({
            "name": "test-fail",
            "colors": {
                "bg": "#555555",
                "fg": "#444444",
                "accent": "#000000",
                "accent_fg": "#ffffff",
                "border": "#222222"
            }
        });
        let failures = wcag_contrast_failures(&theme).expect("colors object present");
        assert!(
            failures.iter().any(|f| f.contains("body text")),
            "expected a body-text failure, got: {failures:?}"
        );
    }

    #[test]
    fn wcag_returns_none_when_colors_block_missing() {
        let theme = serde_json::json!({"name": "no-colors"});
        assert!(wcag_contrast_failures(&theme).is_none());
    }

    #[test]
    fn wcag_skips_pairs_where_one_color_is_absent() {
        // accent_fg is missing — that pair should be skipped, not error.
        let theme = serde_json::json!({
            "name": "partial",
            "colors": {
                "bg": "#000000",
                "fg": "#ffffff",
                "accent": "#ffff00",
                "border": "#888888"
            }
        });
        let failures = wcag_contrast_failures(&theme).expect("colors present");
        // No failures expected — fg/bg, border/bg are both fine; accent_fg
        // pair is skipped.
        assert!(failures.is_empty(), "expected pass, got: {failures:?}");
    }

    #[test]
    fn strip_identity_removes_sensitive_files_and_keeps_others() {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let tmp = std::env::temp_dir().join(format!("mur-strip-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();

        // Build a fake source tarball with all the sensitive
        // names + one innocent file.
        let src = tmp.join("source.tar.gz");
        {
            let f = std::fs::File::create(&src).unwrap();
            let enc = GzEncoder::new(f, Compression::default());
            let mut t = tar::Builder::new(enc);
            for (path, body) in [
                ("identity.key", b"SECRET" as &[u8]),
                ("identity.pub", b"z123pub"),
                ("rotations.jsonl", b"{\"rotation\":1}"),
                ("profile.yaml", b"name: demo"),
                ("sys_prompt.md", b"You are a demo agent."),
                ("skills/web.md", b"# Web skill"),
            ] {
                let mut header = tar::Header::new_gnu();
                header.set_size(body.len() as u64);
                header.set_mode(0o644);
                header.set_cksum();
                t.append_data(&mut header, path, body).unwrap();
            }
            let enc = t.into_inner().unwrap();
            enc.finish().unwrap();
        }

        let dst = tmp.join("stripped.tar.gz");
        super::strip_identity_from_tarball(&src, &dst).unwrap();

        // Re-read the stripped tarball.
        let f = std::fs::File::open(&dst).unwrap();
        let mut a = tar::Archive::new(flate2::read::GzDecoder::new(f));
        let mut paths = Vec::new();
        for entry in a.entries().unwrap() {
            let entry = entry.unwrap();
            paths.push(entry.path().unwrap().to_string_lossy().to_string());
        }
        paths.sort();

        assert!(
            !paths.iter().any(|p| p.contains("identity.key")),
            "identity.key should be stripped: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("identity.pub")),
            "identity.pub should be stripped: {paths:?}"
        );
        assert!(
            !paths.iter().any(|p| p.contains("rotations.jsonl")),
            "rotations.jsonl should be stripped: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "profile.yaml"),
            "profile.yaml must survive: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "sys_prompt.md"),
            "sys_prompt.md must survive: {paths:?}"
        );
        assert!(
            paths.iter().any(|p| p == "skills/web.md"),
            "skills/ tree must survive: {paths:?}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn clone_mode_refuses_without_unsafe_env() {
        // Build options with clone_identity=true. We can't easily
        // exercise the full pipeline (needs a real agent home + tauri
        // toolchain); but the gate runs as the very first thing in
        // run() before any I/O, so we test it by calling the run()
        // entry directly with a path that doesn't exist — the gate
        // should fire before reaching the prereq_check or the agent
        // home read.
        // SAFETY: tests run serially in a single thread per default.
        unsafe {
            std::env::remove_var("MUR_ALLOW_UNSAFE_CLONE");
        }
        let opts = ExportGuiOptions {
            agent_name: "demo".into(),
            agent_home: PathBuf::from("/nonexistent"),
            out: PathBuf::from("/nonexistent/out"),
            theme: "light".into(),
            icon: None,
            clone_identity: true,
            skip_notarize: true,
        };
        let err = run(opts).unwrap_err().to_string();
        assert!(
            err.contains("clone-mode bundle would ship") || err.contains("MUR_ALLOW_UNSAFE_CLONE"),
            "expected clone-mode safety gate, got: {err}"
        );
    }

    #[test]
    fn wcag_treats_invalid_hex_as_skip() {
        let theme = serde_json::json!({
            "name": "bad-hex",
            "colors": {
                "bg": "#000000",
                "fg": "not-a-color",
                "accent": "#ffff00",
                "accent_fg": "#000000",
                "border": "#888888"
            }
        });
        // fg is unparseable → fg/bg pair is skipped silently. Should
        // not panic, should not produce a failure for that pair.
        let failures = wcag_contrast_failures(&theme).expect("colors present");
        assert!(
            !failures.iter().any(|f| f.contains("body text")),
            "expected fg/bg pair to be skipped on bad hex, got: {failures:?}"
        );
    }
}
