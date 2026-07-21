//! `mur agent install <path>` / `mur agent uninstall <name>` / `mur agent inspect <path>`
//!
//! Thin CLI wrappers around the `mur_common::muragent::installer` flow. The
//! actual install logic — validation, trust upsert, payload extraction — lives
//! in mur-common and is shared with Hub and (future) Commander.

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::agent::AgentProfile;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::installer::{self, InstallOutcome};
use mur_common::muragent::manifest::MuragentManifest;
use mur_common::muragent::reader::MuragentArchive;
use mur_common::muragent::validator;
use mur_common::trust;

use super::resolve_mur_home;

/// Installs a `.muragent` bundle. Returns `(installed_name, signer_fingerprint_hex)`
/// so the dispatch layer can fire the best-effort trusted-recipe install hook
/// (async; mirrors the fleet-import wiring) without making this function async
/// itself — it has synchronous test callers.
pub fn cmd_install(
    path: &Path,
    model_ref_override: Option<&str>,
    as_name: Option<&str>,
) -> Result<(String, String)> {
    let archive = MuragentArchive::read(path)
        .with_context(|| format!("read .muragent file at {}", path.display()))?;
    let mur_home = resolve_mur_home()?;

    // Official-distribution gate: must run BEFORE any install side effects.
    // `installer::install_with_name` extracts the payload to disk as part of
    // its own validate+install pipeline, so the gate needs a prior, separate
    // validation pass here. `validator::validate` is pure (archive-only, no
    // I/O beyond reading the in-memory archive), so validating twice has no
    // side effects — it just costs a little extra CPU.
    let validation = validator::validate(&archive).context("validate .muragent")?;
    if validation.manifest.distribution.as_deref()
        == Some(mur_common::official::DISTRIBUTION_OFFICIAL)
    {
        let logged_in_user = crate::auth::load_tokens().and_then(|t| t.user_id);
        official_gate_agent(
            &mur_home,
            &validation.manifest.agent.slug,
            &validation.author_pubkey,
            true,
            logged_in_user.as_deref(),
            mur_common::skill::publisher_trust::MUR_OFFICIAL_PUBLISHER_KEY_FP,
        )?;
    }

    let outcome: InstallOutcome = installer::install_with_name(&archive, &mur_home, "cli", as_name)
        .context("install .muragent")?;

    // The install/dispatch name: the clone's name when `--as` was given,
    // else the manifest's own slug (unchanged from prior behavior).
    let installed_name: &str = as_name.unwrap_or(&outcome.manifest.agent.slug);

    if let Some(clone_name) = as_name {
        clone_identity_and_profile(&mur_home, clone_name)?;
        println!("Cloned agent as '{clone_name}'");
    } else {
        let verb = if outcome.was_update {
            "Updated"
        } else {
            "Installed"
        };
        println!(
            "{verb} agent '{}' ({})",
            outcome.manifest.agent.display_name, outcome.manifest.agent.slug
        );
    }
    println!("  trust:       {:?}", outcome.trust_level);
    println!("  fingerprint: {}", outcome.fingerprint_hex);
    println!("  words:       {}", outcome.fingerprint_words);

    if !outcome.downgraded_broad_egress.is_empty() {
        let names = outcome.downgraded_broad_egress.join(", ");
        println!(
            "⚠ broad-audited egress was reset to inherit for: {names}. \
             Re-grant locally with: mur agent mcp set-network {installed_name} <server> --broad-audited"
        );
    }

    if let Some(model_ref) = model_ref_override {
        apply_model_ref_override(&mur_home, installed_name, model_ref)?;
    } else {
        maybe_resolve_model(&mur_home, installed_name, &archive)?;
    }
    Ok((installed_name.to_string(), outcome.fingerprint_hex.clone()))
}

/// Official-distribution gate for `.muragent` installs. Mirrors the fleet
/// import gate (`cmd/fleet/import.rs::official_gate`): marker present ⇒ (1)
/// the package must be signed by `official_fp` (a self-signed package
/// claiming `distribution: official` is a spoof), and (2) a matching local
/// license must exist for `agents/<agent_slug>` + the logged-in user.
/// `signer_pk` is the raw verified Ed25519 public key; `official_fp` is an
/// `ed25519-<8hex>` fingerprint. The caller is
/// responsible for only invoking this when the manifest carries the marker.
fn official_gate_agent(
    mur_home: &Path,
    agent_slug: &str,
    signer_pk: &[u8; 32],
    signature_verified: bool,
    logged_in_user: Option<&str>,
    official_fp: &str,
) -> Result<()> {
    use mur_common::muragent::dsse::keyid_from_pubkey;
    // Derive the fingerprint from the *verified* signing key, never from the
    // envelope's self-asserted `keyid` string (which is outside the signed
    // payload and thus attacker-settable — an attacker could stamp the
    // official fp onto a bundle signed with their own key).
    if !signature_verified || keyid_from_pubkey(signer_pk) != official_fp {
        bail!(
            "package claims official distribution but is not signed by the MUR official key — refusing install"
        );
    }
    let Some(user) = logged_in_user else {
        bail!(
            "this is official MUR content — log in (`mur login`) and get it from app.mur.run via `mur official install`"
        );
    };
    let item = format!("agents/{agent_slug}");
    crate::official::store::require_license_against(mur_home, &item, user, official_fp).map_err(
        |e| {
            anyhow::anyhow!(
                "{e} — official MUR content can't be shared between accounts; get it from app.mur.run via `mur official install`"
            )
        },
    )
}

/// After a `--as <name>` clone install, give the clone a new identity: a
/// fresh UUIDv7 `profile.id`, `profile.name` set to the clone's directory
/// name, and a freshly minted + persisted Ed25519 identity (never the
/// source agent's private key — the installer already strips
/// `identity.key` from any incoming payload).
fn clone_identity_and_profile(mur_home: &Path, clone_name: &str) -> Result<()> {
    let agent_dir = mur_home.join("agents").join(clone_name);
    let profile_path = agent_dir.join("profile.yaml");
    let mut profile: AgentProfile = serde_yaml_ng::from_str(
        &std::fs::read_to_string(&profile_path)
            .with_context(|| format!("read {}", profile_path.display()))?,
    )
    .with_context(|| format!("parse {}", profile_path.display()))?;

    profile.name = clone_name.to_string();
    profile.id = uuid::Uuid::now_v7().to_string();

    // Atomic write (temp + rename) so a crash mid-write never leaves a
    // half-written profile.yaml for the clone; matches the repo YAML convention.
    let tmp = profile_path.with_extension("yaml.tmp");
    std::fs::write(&tmp, serde_yaml_ng::to_string(&profile)?)
        .with_context(|| format!("write {}", tmp.display()))?;
    std::fs::rename(&tmp, &profile_path)
        .with_context(|| format!("rename {} -> {}", tmp.display(), profile_path.display()))?;

    AgentIdentity::generate()
        .save(&agent_dir)
        .map_err(|e| anyhow::anyhow!("save fresh identity for clone '{clone_name}': {e}"))?;

    Ok(())
}

pub fn cmd_uninstall(name: &str, delete_data: bool) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_dir = mur_home.join("agents").join(name);

    if !agent_dir.exists() {
        bail!("agent '{name}' is not installed");
    }

    if delete_data {
        fs::remove_dir_all(&agent_dir).context("remove agent directory")?;
        println!("Uninstalled '{name}' and deleted all data");
    } else {
        for entry in fs::read_dir(&agent_dir)? {
            let entry = entry?;
            if entry.file_name() == "data" {
                continue;
            }
            let p = entry.path();
            if p.is_dir() {
                fs::remove_dir_all(&p)?;
            } else {
                fs::remove_file(&p)?;
            }
        }
        println!(
            "Uninstalled '{name}' (data preserved at {}/data)",
            agent_dir.display()
        );
    }
    Ok(())
}

pub fn cmd_inspect(path: &Path) -> Result<()> {
    let archive = MuragentArchive::read(path)
        .with_context(|| format!("read .muragent file at {}", path.display()))?;

    let manifest_yaml = archive
        .get_str("manifest.yaml")
        .context("read manifest.yaml from archive")?;
    let manifest: MuragentManifest =
        serde_yaml_ng::from_str(manifest_yaml).context("parse manifest.yaml from archive")?;

    println!(
        "Agent:       {} ({})",
        manifest.agent.display_name, manifest.agent.slug
    );
    println!("Schema:      {}", manifest.schema);
    println!("Exported at: {}", manifest.exported_at);
    println!("Mur version: {}", manifest.exporter.mur_version);
    println!("Bundle ID:   {}", manifest.agent.bundle_id);
    println!("URL scheme:  {}", manifest.agent.url_scheme);
    println!("Agent UUID:  {}", manifest.agent.original_uuid);
    println!("Surfaces:    {:?}", manifest.required_surfaces);
    println!("Capabilities: {:?}", manifest.optional_capabilities);
    println!("MCP servers: {}", manifest.mcp_servers.len());
    for mcp in &manifest.mcp_servers {
        println!("  - {} ({})", mcp.name, mcp.command_basename);
    }

    match validator::validate(&archive) {
        Ok(result) => {
            println!();
            println!("Signature:    VALID");
            println!("Author keyid: {}", result.keyid);
            println!(
                "Fingerprint:  {}",
                trust::short_fingerprint(&result.author_pubkey)
            );
            println!(
                "Words:        {}",
                trust::word_list_fingerprint(&result.author_pubkey)
            );
        }
        Err(e) => {
            println!();
            println!("Signature:    INVALID — {e}");
        }
    }
    Ok(())
}

fn maybe_resolve_model(mur_home: &Path, slug: &str, archive: &MuragentArchive) -> Result<()> {
    use crate::cmd::agent::model_resolve::{apply_model_choice, detect_hardware};
    use mur_common::model_resolve::recommend;

    let manifest_yaml = archive
        .get_str("manifest.yaml")
        .context("read manifest.yaml")?;
    let manifest: MuragentManifest = serde_yaml_ng::from_str(manifest_yaml)?;
    let hint = manifest.model_hint.clone();
    let hw = detect_hardware();
    let rec = recommend(hint.as_ref(), &hw);

    if !std::io::IsTerminal::is_terminal(&std::io::stdin()) {
        println!(
            "  model: not configured — use `mur agent install --model <ref>` \
             or run `mur model add` then `mur agent start {slug}`"
        );
        return Ok(());
    }

    println!("\nThis agent needs a model backend (no weights are bundled).");
    if let Some(h) = &hint {
        println!(
            "  Authored for: {}/{} (tier {:?})",
            h.provider, h.name, h.tier
        );
    }
    let choice = prompt_model_choice(&rec, hint.as_ref())?;
    if let Some(choice) = choice {
        let key = apply_model_choice(mur_home, slug, &choice)?;
        println!("  bound model_ref = {key}");
    } else {
        println!("  skipped — set one later with `mur model add`");
    }
    Ok(())
}

/// Apply a `--model <ref>` override: verify the ref exists in the registry
/// and point the agent at it. Used by `--model` flag (non-interactive path).
fn apply_model_ref_override(mur_home: &Path, slug: &str, model_ref: &str) -> Result<()> {
    use crate::cmd::agent::model_resolve::ModelChoice;
    use mur_common::model::ModelRegistry;

    let reg_path = ModelRegistry::default_path()?;
    let reg = ModelRegistry::load_from(&reg_path)?;
    anyhow::ensure!(
        reg.models.contains_key(model_ref),
        "model ref '{model_ref}' not found in ~/.mur/models.yaml — \
         run `mur model add` to register it first"
    );
    let entry = &reg.models[model_ref];
    let choice = ModelChoice {
        provider: entry.provider.clone(),
        model: entry.model.clone(),
        base_url: entry.base_url.clone(),
        secret: entry.secret.as_ref().map(|s| s.to_string()),
    };
    let key = crate::cmd::agent::model_resolve::apply_model_choice(mur_home, slug, &choice)?;
    println!("  model_ref = {key}");
    Ok(())
}

fn prompt_model_choice(
    rec: &mur_common::model_resolve::Recommendation,
    hint: Option<&mur_common::muragent::manifest::ModelHint>,
) -> Result<Option<crate::cmd::agent::model_resolve::ModelChoice>> {
    use crate::cmd::agent::model_resolve::ModelChoice;
    use mur_common::model_resolve::Recommendation;
    use std::io::Write;

    let default_local = hint.map(|h| (h.provider.clone(), h.name.clone()));
    let prompt = match rec {
        Recommendation::Local => "Pull a local model now? [Y/n] ",
        Recommendation::Cloud | Recommendation::CloudOrSmallerLocal => {
            "Paste an API key for a cloud model? [y/N] "
        }
        Recommendation::NeutralMenu => "Configure a model now? [y/N] ",
    };
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let yes = matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes" | "");

    if matches!(rec, Recommendation::Local)
        && yes
        && let Some((provider, model)) = default_local
    {
        return Ok(Some(ModelChoice {
            provider,
            model,
            base_url: None,
            secret: None,
        }));
    }
    if !yes {
        return Ok(None);
    }
    let provider = read_field("  provider (e.g. anthropic): ")?;
    let model = read_field("  model (e.g. claude-opus-4-7): ")?;
    let secret = read_field("  secret ref (e.g. env:ANTHROPIC_API_KEY): ")?;
    Ok(Some(ModelChoice {
        provider,
        model,
        base_url: None,
        secret: if secret.is_empty() {
            None
        } else {
            Some(secret)
        },
    }))
}

fn read_field(prompt: &str) -> Result<String> {
    use std::io::Write;
    print!("{prompt}");
    std::io::stdout().flush().ok();
    let mut s = String::new();
    std::io::stdin().read_line(&mut s)?;
    Ok(s.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maybe_resolve_with_model_ref_applies_without_prompt() {
        use mur_common::model::{ModelEntry, ModelRegistry};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path();

        // Create agent dir with a default profile
        let agent_home = mur_home.join("agents").join("demo");
        std::fs::create_dir_all(&agent_home).unwrap();
        let p = mur_common::agent::AgentProfile::default_for_tests();
        std::fs::write(
            agent_home.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();

        // Pre-populate the registry so the ref resolves
        unsafe {
            std::env::set_var("MUR_HOME", mur_home);
        }
        let reg_path = ModelRegistry::default_path().unwrap();
        let mut reg = ModelRegistry::load_from(&reg_path).unwrap_or_default();
        reg.models.insert(
            "ollama_llama3_2_3b".to_string(),
            ModelEntry {
                provider: "ollama".into(),
                model: "llama3.2:3b".into(),
                base_url: None,
                secret: None,
                capabilities: vec![],
                params: serde_json::Value::Null,
                tier: None,
                cost_per_1k_tokens: None,
                ..Default::default()
            },
        );
        reg.save_to(&reg_path).unwrap();

        // Call the new apply_model_ref_override helper
        apply_model_ref_override(mur_home, "demo", "ollama_llama3_2_3b").unwrap();

        let profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(agent_home.join("profile.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(profile.model_ref.as_deref(), Some("ollama_llama3_2_3b"));
    }

    #[test]
    fn cmd_install_as_clones_with_new_id_and_fresh_identity() {
        use mur_common::agent::AgentProfile as Profile;
        use mur_common::identity::AgentIdentity as Identity;
        use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path().join("mur");
        unsafe {
            std::env::set_var("MUR_HOME", &mur_home);
        }

        // Build a source .muragent bundle with a known source profile id.
        let mut source_profile = Profile::default_for_tests();
        source_profile.name = "aura".to_string();
        let source_id = source_profile.id.clone();
        let manifest = build_manifest_from_profile(&source_profile, "2.13.0");
        let profile_yaml = serde_yaml_ng::to_string(&source_profile).unwrap();
        let writer = MuragentWriter::new(manifest, profile_yaml, Identity::generate());
        let bundle_path = tmp.path().join("aura.muragent");
        writer.write(&bundle_path).unwrap();

        cmd_install(&bundle_path, None, Some("clone-x")).unwrap();

        let clone_dir = mur_home.join("agents").join("clone-x");
        let profile: Profile = serde_yaml_ng::from_str(
            &std::fs::read_to_string(clone_dir.join("profile.yaml")).unwrap(),
        )
        .unwrap();
        assert_eq!(profile.name, "clone-x");
        assert_ne!(profile.id, source_id, "clone must get a new profile.id");
        assert!(
            clone_dir.join("identity.key").exists(),
            "clone must have a freshly minted identity.key"
        );
        assert!(clone_dir.join("identity.pub").exists());
    }

    #[test]
    fn official_agent_gate_refuses_without_license_and_passes_with() {
        let home = tempfile::tempdir().unwrap();
        // signer key used for both the "bundle" signature and the license
        let key = ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]);
        let pk = key.verifying_key().to_bytes();
        let fp = mur_common::muragent::dsse::keyid_from_pubkey(&pk);
        // no license → refused
        let err =
            official_gate_agent(home.path(), "researcher", &pk, true, Some("u1"), &fp).unwrap_err();
        assert!(err.to_string().contains("app.mur.run"), "{err}");
        // matching license → passes
        let mut l = mur_common::official::OfficialLicense {
            format_version: mur_common::official::OFFICIAL_LICENSE_FORMAT,
            user_id: "u1".into(),
            item: "agents/researcher".into(),
            version: "1.0.0".into(),
            expires_at: "2027-01-01T00:00:00Z".into(),
            signer_pubkey: String::new(),
            sig: None,
        };
        mur_common::official::sign_license(&mut l, &key);
        crate::official::store::save_license(home.path(), &l).unwrap();
        official_gate_agent(home.path(), "researcher", &pk, true, Some("u1"), &fp).unwrap();
        // wrong signer key on the package → refused even with license. Deriving
        // the fp from the verified pubkey (not a self-asserted string) is what
        // makes this un-spoofable.
        let wrong_pk = ed25519_dalek::SigningKey::from_bytes(&[8u8; 32])
            .verifying_key()
            .to_bytes();
        let err = official_gate_agent(home.path(), "researcher", &wrong_pk, true, Some("u1"), &fp)
            .unwrap_err();
        assert!(err.to_string().contains("official key"), "{err}");
    }

    #[test]
    fn official_gate_agent_unverified_signature_refused() {
        let home = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[1u8; 32]);
        let pk = key.verifying_key().to_bytes();
        let fp = mur_common::muragent::dsse::keyid_from_pubkey(&pk);
        let err = official_gate_agent(home.path(), "researcher", &pk, false, Some("u1"), &fp)
            .unwrap_err();
        assert!(err.to_string().contains("official key"), "{err}");
    }

    #[test]
    fn official_gate_agent_no_login_refused_with_app_mur_run() {
        let home = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[3u8; 32]);
        let pk = key.verifying_key().to_bytes();
        let fp = mur_common::muragent::dsse::keyid_from_pubkey(&pk);
        let err = official_gate_agent(home.path(), "researcher", &pk, true, None, &fp).unwrap_err();
        assert!(err.to_string().contains("app.mur.run"), "{err}");
    }

    #[test]
    fn official_gate_agent_wrong_user_license_refused() {
        let home = tempfile::tempdir().unwrap();
        let key = ed25519_dalek::SigningKey::from_bytes(&[5u8; 32]);
        let pk = key.verifying_key().to_bytes();
        let fp = mur_common::muragent::dsse::keyid_from_pubkey(&pk);
        let mut l = mur_common::official::OfficialLicense {
            format_version: mur_common::official::OFFICIAL_LICENSE_FORMAT,
            user_id: "u1".into(),
            item: "agents/researcher".into(),
            version: "1.0.0".into(),
            expires_at: "2027-01-01T00:00:00Z".into(),
            signer_pubkey: String::new(),
            sig: None,
        };
        mur_common::official::sign_license(&mut l, &key);
        crate::official::store::save_license(home.path(), &l).unwrap();

        let err =
            official_gate_agent(home.path(), "researcher", &pk, true, Some("u2"), &fp).unwrap_err();
        assert!(err.to_string().contains("different account"), "{err}");
    }

    /// No `distribution` marker on the manifest ⇒ the gate is never invoked
    /// and install proceeds exactly as before (mirrors the fleet-import
    /// `official_gate_no_marker_is_noop` coverage, at the `cmd_install`
    /// wiring level since `official_gate_agent` itself takes no manifest).
    #[test]
    fn cmd_install_no_marker_is_unaffected() {
        use mur_common::agent::AgentProfile as Profile;
        use mur_common::identity::AgentIdentity as Identity;
        use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path().join("mur");
        unsafe {
            std::env::set_var("MUR_HOME", &mur_home);
        }

        let mut source_profile = Profile::default_for_tests();
        source_profile.name = "plain".to_string();
        let manifest = build_manifest_from_profile(&source_profile, "2.13.0");
        assert!(manifest.distribution.is_none(), "sanity: unmarked manifest");
        let profile_yaml = serde_yaml_ng::to_string(&source_profile).unwrap();
        let writer = MuragentWriter::new(manifest, profile_yaml, Identity::generate());
        let bundle_path = tmp.path().join("plain.muragent");
        writer.write(&bundle_path).unwrap();

        cmd_install(&bundle_path, None, None).unwrap();
        assert!(
            mur_home
                .join("agents")
                .join("plain")
                .join("profile.yaml")
                .exists()
        );
    }

    /// End-to-end wiring proof via `cmd_install`. Production pins the REAL
    /// `MUR_OFFICIAL_PUBLISHER_KEY_FP`, which no test-generated signing key
    /// can ever match (the private key isn't available to the client) — so
    /// this test can only reach the gate's signer-mismatch branch, not the
    /// login/license branches (those are covered directly against
    /// `official_gate_agent` above). This still proves the gate is wired
    /// into `cmd_install` and fails closed before anything is written.
    #[test]
    fn cmd_install_official_marked_agent_is_refused() {
        use mur_common::agent::AgentProfile as Profile;
        use mur_common::identity::AgentIdentity as Identity;
        use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};
        use tempfile::TempDir;

        let tmp = TempDir::new().unwrap();
        let mur_home = tmp.path().join("mur");
        unsafe {
            std::env::set_var("MUR_HOME", &mur_home);
        }

        let mut source_profile = Profile::default_for_tests();
        source_profile.name = "official-agent".to_string();
        let mut manifest = build_manifest_from_profile(&source_profile, "2.13.0");
        manifest.distribution = Some(mur_common::official::DISTRIBUTION_OFFICIAL.to_string());
        let profile_yaml = serde_yaml_ng::to_string(&source_profile).unwrap();
        let writer = MuragentWriter::new(manifest, profile_yaml, Identity::generate());
        let bundle_path = tmp.path().join("official-agent.muragent");
        writer.write(&bundle_path).unwrap();

        let err = cmd_install(&bundle_path, None, None).unwrap_err();
        let msg = format!("{err:#}");
        assert!(
            msg.contains("official key"),
            "expected official-distribution gate refusal, got: {msg}"
        );
        assert!(
            !mur_home.join("agents").join("official-agent").exists(),
            "agent must not be written when the official gate refuses install"
        );
    }
}
