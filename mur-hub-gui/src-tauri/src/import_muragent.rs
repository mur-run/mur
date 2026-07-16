//! `mur agent install` — Hub side. Tauri commands for the §7.2 import dialog.
//!
//! Two commands:
//! - `inspect_muragent_file(path)` → read-only inspection of the package.
//!   Runs the full validator (to surface signature/integrity errors as a
//!   refusal in the UI), looks up the author in the local trust store, and
//!   returns the metadata the §7.2 dialog needs to render. Does NOT modify
//!   any filesystem state.
//! - `install_muragent_file(path)` → invokes `mur_common::muragent::installer`
//!   with surface=`"hub"`. The UI is responsible for enforcing the 5-second
//!   first-time-author delay before this call is allowed.
//!
//! The two-step flow matches the spec: signature/integrity failures are
//! fatal (refusal in `inspect`), and `install` is only reached for valid
//! packages — there is no "continue anyway" path.

use std::path::Path;

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use mur_common::muragent::installer;
use mur_common::muragent::manifest::{MuragentManifest, Surface};
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::validator;
use mur_common::trust::{self, TrustLevel, TrustStore};
use mur_core::cmd::agent::model_resolve::ModelChoice;
use mur_gui_core::autostart;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct DeclaredPermissions {
    /// MCP server names + basenames the agent will attempt to spawn.
    pub mcp_servers: Vec<McpServerView>,
    /// Optional capabilities the agent declares (e.g. `a2a.message.send`).
    pub capabilities: Vec<String>,
    /// Hub voice enabled (TTS + STT).
    pub voice_enabled: bool,
    /// Hub pet enabled.
    pub pet_enabled: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct McpServerView {
    pub name: String,
    pub command_basename: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ModelHintView {
    pub provider: String,
    pub name: String,
    pub tier: String, // "small" | "mid" | "frontier"
    pub min_ram_gb: u32,
    pub local_capable: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TrustStatus {
    /// First time this pubkey has been seen on this host.
    FirstTimeAuthor,
    /// Pubkey is known and not refused.
    KnownAuthor { level: String },
    /// Display name is known but pubkey is different, and no rotation manifest
    /// is on file. The UI must NOT offer an install button in this state.
    KeyChangeRefused { previous_fingerprint: String },
}

#[derive(Debug, Clone, Serialize)]
pub struct MuragentInspection {
    // Identity
    pub display_name: String,
    pub slug: String,
    pub bundle_id: String,
    pub url_scheme: String,
    pub original_uuid: String,

    // Origin
    pub schema: String,
    pub mur_version: String,
    pub exported_at: String,
    pub required_surfaces: Vec<String>,

    // Signature outcome
    pub signature_valid: bool,
    pub signature_error: Option<String>,

    // Trust
    pub trust_status: TrustStatus,
    pub fingerprint_hex: String,
    pub fingerprint_words: String,
    pub author_keyid: String,

    // What this agent will do (per §7.2 rule 5)
    pub permissions: DeclaredPermissions,
    /// Model the agent was authored against, for first-run resolution (§7.1).
    pub model_hint: Option<ModelHintView>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstallReceipt {
    pub display_name: String,
    pub slug: String,
    pub was_update: bool,
    pub trust_level: String,
    pub fingerprint_hex: String,
    /// Platform stub path (`.app`, `.lnk`, or `.desktop`), empty if generation failed.
    pub stub_path: String,
}

/// Read-only inspection of a `.muragent` for the §7.2 dialog. Errors here are
/// surfaced to the UI as a refusal — there is no fall-through to install.
#[tauri::command]
pub fn inspect_muragent_file(path: String) -> Result<MuragentInspection, String> {
    inspect_inner(Path::new(&path)).map_err(|e| e.to_string())
}

/// Perform the actual install. UI must only call this after the user has
/// consented to the dialog (and, on first-time-author, after the 5s delay).
#[tauri::command]
pub fn install_muragent_file(
    app: tauri::AppHandle,
    path: String,
) -> Result<InstallReceipt, String> {
    // Hub version from the Tauri context (tauri.conf.json, CI-stamped) — not
    // env!("CARGO_PKG_VERSION"), which is stuck at 0.1.0 (workspace-excluded).
    let hub_version = app.package_info().version.to_string();
    install_inner(Path::new(&path), &hub_version).map_err(|e| e.to_string())
}

fn inspect_inner(path: &Path) -> anyhow::Result<MuragentInspection> {
    let archive =
        MuragentArchive::read(path).map_err(|e| anyhow::anyhow!("read .muragent: {e}"))?;

    // Validate the archive fully. Signature/integrity failures land in the
    // returned inspection so the UI renders a clear refusal screen rather
    // than throwing an unmapped error.
    match validator::validate(&archive) {
        Ok(result) => build_inspection(&archive, &result.manifest, Some(&result), None),
        Err(e) => {
            // We still need the unsigned manifest to populate the display
            // name / fingerprint columns of the refusal screen. Read it
            // direct from manifest.yaml — already integrity-bound to the
            // signature *if* the signature were valid; here we explicitly
            // mark signature_valid=false so the UI doesn't show an Import
            // button.
            let manifest_yaml = archive
                .get_str("manifest.yaml")
                .map_err(|e| anyhow::anyhow!("read manifest.yaml: {e}"))?;
            let manifest: MuragentManifest = serde_yaml_ng::from_str(manifest_yaml)
                .map_err(|err| anyhow::anyhow!("parse manifest.yaml: {err}"))?;
            build_inspection(&archive, &manifest, None, Some(e.to_string()))
        }
    }
}

fn install_inner(path: &Path, hub_version: &str) -> anyhow::Result<InstallReceipt> {
    let archive =
        MuragentArchive::read(path).map_err(|e| anyhow::anyhow!("read .muragent: {e}"))?;
    let mur_home = trust::mur_home();
    let outcome =
        installer::install(&archive, &mur_home, "hub").map_err(|e| anyhow::anyhow!("{e}"))?;

    // Generate the per-agent OS stub (Dock icon / URL scheme / Start Menu entry).
    // Read the installed icon so the stub can show a proper icon.
    let icon_path = mur_home
        .join("agents")
        .join(&outcome.manifest.agent.slug)
        .join("icon")
        .join("icon.icns");
    let icon_icns = std::fs::read(&icon_path).ok();

    let stub_path = match mur_gui_core::stub::generate(
        &outcome.manifest.agent.slug,
        &outcome.manifest.agent.display_name,
        &outcome.manifest.agent.bundle_id,
        &outcome.manifest.agent.url_scheme,
        icon_icns.as_deref(),
        hub_version,
        &mur_home,
    ) {
        Ok(info) => info.path.to_string_lossy().into_owned(),
        Err(e) => {
            // Stub generation is best-effort: log the error but don't fail
            // the install. The agent is installed and usable; the user just
            // won't have an OS stub until they reinstall or Hub retries.
            tracing::warn!(
                "stub generation failed for {}: {e}",
                outcome.manifest.agent.slug
            );
            String::new()
        }
    };

    // Register and start the agent via the OS init system (spec §3.1).
    // Both calls are best-effort — the agent is already installed on disk.
    let slug = &outcome.manifest.agent.slug;
    let bundle_id = &outcome.manifest.agent.bundle_id;
    if let Ok(runtime_bin) = find_runtime_binary_for_autostart() {
        if let Err(e) = autostart::register(
            slug,
            &outcome.manifest.agent.display_name,
            &runtime_bin,
            &mur_home,
        ) {
            tracing::warn!("autostart register failed for {slug}: {e}");
        }
        if let Err(e) = autostart::start_service(slug) {
            tracing::warn!("autostart start_service failed for {slug}: {e}");
        }
    } else {
        tracing::warn!("mur-agent-runtime binary not found; skipping autostart for {slug}");
    }
    let _ = bundle_id;

    Ok(InstallReceipt {
        display_name: outcome.manifest.agent.display_name.clone(),
        slug: outcome.manifest.agent.slug.clone(),
        was_update: outcome.was_update,
        trust_level: format!("{:?}", outcome.trust_level).to_lowercase(),
        fingerprint_hex: outcome.fingerprint_hex,
        stub_path,
    })
}

fn find_runtime_binary_for_autostart() -> anyhow::Result<std::path::PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe.parent().ok_or_else(|| anyhow::anyhow!("no parent"))?;
    #[cfg(target_os = "windows")]
    let name = "mur-agent-runtime.exe";
    #[cfg(not(target_os = "windows"))]
    let name = "mur-agent-runtime";
    let candidate = dir.join(name);
    if candidate.exists() {
        return Ok(candidate);
    }
    anyhow::bail!("mur-agent-runtime not found at {}", candidate.display())
}

fn build_inspection(
    _archive: &MuragentArchive,
    manifest: &MuragentManifest,
    valid: Option<&validator::ValidationResult>,
    signature_error: Option<String>,
) -> anyhow::Result<MuragentInspection> {
    let trust_store = TrustStore::load().map_err(|e| anyhow::anyhow!("load trust store: {e}"))?;

    let (fingerprint_hex, fingerprint_words, author_keyid, pubkey_b64) = match valid {
        Some(r) => (
            trust::short_fingerprint(&r.author_pubkey),
            trust::word_list_fingerprint(&r.author_pubkey),
            r.keyid.clone(),
            B64.encode(r.author_pubkey),
        ),
        None => ("—".into(), "—".into(), "—".into(), String::new()),
    };

    let trust_status = if valid.is_none() {
        // Signature invalid; trust lookup is meaningless. The UI uses
        // signature_valid=false to short-circuit, so this value is just a
        // placeholder.
        TrustStatus::FirstTimeAuthor
    } else if let Some(entry) = trust_store.find_by_pubkey(&pubkey_b64) {
        let level = match entry.trust_level {
            TrustLevel::Known => "known",
            TrustLevel::Pending => "pending",
            TrustLevel::Rejected => "rejected",
            TrustLevel::Superseded => "superseded",
        }
        .to_string();
        TrustStatus::KnownAuthor { level }
    } else {
        let by_name = trust_store.find_by_display_name(&manifest.agent.display_name);
        if let Some(prev) = by_name.first() {
            TrustStatus::KeyChangeRefused {
                previous_fingerprint: prev.fingerprint.clone(),
            }
        } else {
            TrustStatus::FirstTimeAuthor
        }
    };

    let permissions = DeclaredPermissions {
        mcp_servers: manifest
            .mcp_servers
            .iter()
            .map(|m| McpServerView {
                name: m.name.clone(),
                command_basename: m.command_basename.clone(),
            })
            .collect(),
        capabilities: manifest.optional_capabilities.clone(),
        voice_enabled: manifest
            .hub
            .as_ref()
            .and_then(|h| h.voice.as_ref())
            .map(|v| v.enabled)
            .unwrap_or(false),
        pet_enabled: manifest
            .hub
            .as_ref()
            .and_then(|h| h.pet.as_ref())
            .map(|p| p.enabled)
            .unwrap_or(false),
    };

    Ok(MuragentInspection {
        display_name: manifest.agent.display_name.clone(),
        slug: manifest.agent.slug.clone(),
        bundle_id: manifest.agent.bundle_id.clone(),
        url_scheme: manifest.agent.url_scheme.clone(),
        original_uuid: manifest.agent.original_uuid.clone(),
        schema: manifest.schema.clone(),
        mur_version: manifest.exporter.mur_version.clone(),
        exported_at: manifest.exported_at.clone(),
        required_surfaces: manifest
            .required_surfaces
            .iter()
            .map(|s| match s {
                Surface::Hub => "hub".into(),
                Surface::Commander => "commander".into(),
            })
            .collect(),
        signature_valid: valid.is_some(),
        signature_error,
        trust_status,
        fingerprint_hex,
        fingerprint_words,
        author_keyid,
        permissions,
        model_hint: manifest.model_hint.as_ref().map(|h| ModelHintView {
            provider: h.provider.clone(),
            name: h.name.clone(),
            tier: format!("{:?}", h.tier).to_lowercase(),
            min_ram_gb: h.min_ram_gb,
            local_capable: h.local_capable,
        }),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct ResolutionView {
    pub hint: Option<ModelHintView>,
    pub total_ram_gb: u32,
    pub apple_silicon: bool,
    pub ollama_present: bool,
    pub recommendation: String,
}

#[tauri::command]
pub fn model_resolution_view(path: String) -> Result<ResolutionView, String> {
    use mur_common::model_resolve::{Recommendation, recommend};
    use mur_common::muragent::manifest::ModelHint;
    use mur_core::cmd::agent::model_resolve::detect_hardware;

    let archive = MuragentArchive::read(Path::new(&path)).map_err(|e| e.to_string())?;
    let manifest_yaml = archive
        .get_str("manifest.yaml")
        .map_err(|e| e.to_string())?;
    let manifest: MuragentManifest =
        serde_yaml_ng::from_str(manifest_yaml).map_err(|e| e.to_string())?;
    let hw = detect_hardware();
    let hint: Option<ModelHint> = manifest.model_hint.clone();
    let rec = match recommend(hint.as_ref(), &hw) {
        Recommendation::Local => "local",
        Recommendation::Cloud => "cloud",
        Recommendation::CloudOrSmallerLocal => "cloud_or_smaller_local",
        Recommendation::NeutralMenu => "neutral_menu",
    };
    Ok(ResolutionView {
        hint: hint.map(|h| ModelHintView {
            provider: h.provider,
            name: h.name,
            tier: format!("{:?}", h.tier).to_lowercase(),
            min_ram_gb: h.min_ram_gb,
            local_capable: h.local_capable,
        }),
        total_ram_gb: hw.total_ram_gb,
        apple_silicon: hw.apple_silicon,
        ollama_present: hw.ollama_present,
        recommendation: rec.to_string(),
    })
}

#[tauri::command]
pub fn apply_agent_model(slug: String, choice: ModelChoice) -> Result<String, String> {
    use mur_common::trust;
    use mur_core::cmd::agent::model_resolve::apply_model_choice;

    let mur_home = trust::mur_home();
    apply_model_choice(&mur_home, &slug, &choice).map_err(|e| e.to_string())
}
