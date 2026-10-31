//! Companion onboarding (D2) commands. Moved verbatim from `commands.rs`.

use super::err;

// ─── D2 onboarding (M2.4) ──────────────────────────────────────────
//
// Three commands back the GUI's onboarding wizard:
//   * `companion_onboarding_status` — reads the persisted state so the
//     wizard can route to "show wizard" vs "show summary" on launch
//   * `companion_onboarding_submit` — invokes the same `init::run` path
//     the CLI uses (`mur agent companion init --answers`)
//   * `companion_onboarding_skip`   — closes the door for users who
//     don't want a relationship-keyed agent: marks `completed_at = now`
//     and applies the WarmOnly tier (Spec §4.2 default)
//
// All three resolve `~/.mur` via `mur_core::paths::mur_root` so the
// `MUR_HOME` override (used in tests + first-launch bootstrap) works.
// Helpers split out of the `#[tauri::command]` wrappers so integration
// tests can call them without a webview.

pub(super) fn onboarding_agent_dir(agent: &str) -> anyhow::Result<std::path::PathBuf> {
    let dir = mur_core::paths::mur_root(None).join("agents").join(agent);
    if !dir.exists() {
        anyhow::bail!(
            "agent {agent} does not exist at {} — run `mur agent create {agent}` first",
            dir.display()
        );
    }
    Ok(dir)
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct OnboardingStatus {
    pub completed_at: Option<String>,
    pub agent_display_name: Option<String>,
    pub first_memory_text: Option<String>,
    /// One of `off` | `warm_only` | `warm_and_behavior` | `all`,
    /// derived from `companion.{enabled, rhythm.enabled, proactive.enabled}`
    /// via `ProactiveTier::from_config`. Lower-case so the React wizard
    /// can match on the same string the submit payload uses.
    pub proactive_tier: String,
}

pub async fn companion_onboarding_status_impl(agent: &str) -> anyhow::Result<OnboardingStatus> {
    let dir = onboarding_agent_dir(agent)?;
    let body = tokio::fs::read_to_string(dir.join("profile.yaml")).await?;
    let p: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&body)?;
    let tier = mur_common::agent::ProactiveTier::from_config(&p.companion);
    let tier_str = serde_json::to_value(tier)?
        .as_str()
        .map(|s| s.to_string())
        .unwrap_or_else(|| "off".to_string());
    Ok(OnboardingStatus {
        completed_at: p.companion.onboarding.completed_at.map(|t| t.to_rfc3339()),
        agent_display_name: p.companion.onboarding.agent_display_name.clone(),
        first_memory_text: p
            .companion
            .onboarding
            .first_memory
            .as_ref()
            .map(|f| f.text.clone()),
        proactive_tier: tier_str,
    })
}

#[tauri::command]
pub async fn companion_onboarding_status(agent: String) -> Result<OnboardingStatus, String> {
    companion_onboarding_status_impl(&agent).await.map_err(err)
}

/// Wizard payload — mirrors the CLI's `--answers` YAML shape one-for-one.
/// Fields are validated downstream when `init::run` deserialises the
/// generated YAML, so this struct stays a flat string-typed bag.
///
/// `re_init` defaults to `false` (matching the CLI safety default).
/// The frontend must explicitly set it to `true` when re-running the
/// wizard from the "Edit" entrypoint so a user's first_memory cannot be
/// silently overwritten by an accidental wizard re-entry.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct OnboardingSubmit {
    pub agent_display_name: String,
    pub locale: String,
    pub name_for_user: String,
    /// One of `friend` | `coach` | `accountability_buddy` | `mentor`.
    pub relationship: String,
    pub first_memory: Option<String>,
    /// One of `off` | `warm_only` | `warm_and_behavior` | `all`.
    pub proactive_tier: String,
    /// `true` only when the wizard is launched from the "Edit" button
    /// against an already-onboarded agent. Default `false`.
    #[serde(default)]
    pub re_init: bool,
}

pub async fn companion_onboarding_submit_impl(
    agent: &str,
    p: OnboardingSubmit,
) -> anyhow::Result<()> {
    use std::io::Write;
    // Build the `--answers` payload as JSON-via-serde_json::Value first so
    // we can omit `first_memory` cleanly when absent (a literal `null` in
    // the YAML deserialises as `Some("null"-ish)` — but `serde_yaml_ng`
    // round-trips `Value::Null` to a real null, which `init::run` then
    // sees as `None` per `#[serde(default)]`).
    let mut obj = serde_json::Map::new();
    obj.insert("locale".into(), serde_json::Value::String(p.locale));
    obj.insert(
        "name_for_user".into(),
        serde_json::Value::String(p.name_for_user),
    );
    obj.insert(
        "agent_display_name".into(),
        serde_json::Value::String(p.agent_display_name),
    );
    obj.insert(
        "relationship".into(),
        serde_json::Value::String(p.relationship),
    );
    obj.insert(
        "formality".into(),
        serde_json::Value::String("casual".into()),
    );
    obj.insert(
        "extra_instructions".into(),
        serde_json::Value::String(String::new()),
    );
    if let Some(text) = p.first_memory {
        obj.insert("first_memory".into(), serde_json::Value::String(text));
    }
    obj.insert(
        "proactive_tier".into(),
        serde_json::Value::String(p.proactive_tier),
    );
    let yaml = serde_yaml_ng::to_string(&serde_json::Value::Object(obj))?;

    // Use a NamedTempFile so the file is unlinked on drop even if
    // `init::run` panics. The CLI parses by path, so we hand it the
    // path (not a handle) and keep the temp file alive for the call.
    let tmp = tempfile::NamedTempFile::new()?;
    tmp.as_file().write_all(yaml.as_bytes())?;
    tmp.as_file().sync_all().ok();
    let answers_path = tmp.path().to_path_buf();
    // re-init defaults off — the frontend must set re_init=true via the
    // "Edit" entrypoint to overwrite an existing onboarding state, so
    // the wizard's normal first-launch path can never silently clobber
    // a user's first_memory.
    mur_core::cmd::agent_companion::init::run(agent, Some(answers_path), p.re_init).await?;
    drop(tmp);
    Ok(())
}

#[tauri::command]
pub async fn companion_onboarding_submit(
    agent: String,
    payload: OnboardingSubmit,
) -> Result<(), String> {
    companion_onboarding_submit_impl(&agent, payload)
        .await
        .map_err(err)
}

pub async fn companion_onboarding_skip_impl(agent: &str) -> anyhow::Result<()> {
    let dir = onboarding_agent_dir(agent)?;
    let pf_path = dir.join("profile.yaml");
    let body = tokio::fs::read_to_string(&pf_path).await?;
    let mut p: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&body)?;
    // WarmOnly tier — flips `companion.enabled` true so voice composition
    // works, leaves rhythm + proactive dormant. Same default the 5-step
    // wizard lands on for users who hit Enter through every step.
    mur_common::agent::ProactiveTier::WarmOnly.apply(&mut p.companion);
    p.companion.onboarding.completed_at = Some(chrono::Utc::now());
    p.companion.onboarding.version = 1;
    let yaml = serde_yaml_ng::to_string(&p)?;
    // Atomic write: temp file + rename, mirroring `cmd::agent_companion::util`.
    let tmp_path = pf_path.with_extension("yaml.tmp");
    tokio::fs::write(&tmp_path, yaml).await?;
    tokio::fs::rename(&tmp_path, &pf_path).await?;
    Ok(())
}

#[tauri::command]
pub async fn companion_onboarding_skip(agent: String) -> Result<(), String> {
    companion_onboarding_skip_impl(&agent).await.map_err(err)
}
