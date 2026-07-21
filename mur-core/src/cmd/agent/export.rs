//! `mur agent export` — package an agent as a portable bundle.
//!
//! Default format is `.muragent` (signed v2, see spec §6). Legacy `.murpkg`
//! continues to be available via `--format=pkg`, and the self-contained
//! binary build via `--format=bin`. The GUI `.app` export is dispatched
//! separately in `dispatch.rs` (the `gui` format never reaches this fn).

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};
use mur_common::AgentProfile;
use mur_common::agent::NotificationTarget;
use mur_common::identity::AgentIdentity;
use mur_common::muragent::writer::{MuragentWriter, build_manifest_from_profile};

use super::resolve_mur_home;

pub fn cmd_export(name: &str, out: &str, format: &str) -> Result<()> {
    if matches!(format, "bin" | "gui") {
        bail!(
            "--format={format} is no longer supported.\n\
             Export a portable .muragent (the default) and share that one file:\n  \
             mur agent export {name} -o {name}.muragent\n\
             Recipients open it with the MuR Hub app, or run it headless with:\n  \
             mur-agent-runtime --load {name}.muragent"
        );
    }
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
        other => bail!("unsupported export format '{other}' (use muragent or pkg)"),
    }
    Ok(())
}

/// Resolve an installed agent's home and export it to `out` as a `.muragent`.
/// Public so the Hub "Share" command (mur-hub-gui) can reuse the exact CLI path.
#[allow(dead_code)]
pub fn export_agent_to_muragent(name: &str, out: &Path) -> Result<()> {
    let mur_home = resolve_mur_home()?;
    let agent_home = mur_home.join("agents").join(name);
    if !agent_home.exists() {
        bail!("agent '{name}' not found");
    }
    export_muragent(name, &agent_home, out)
}

fn export_muragent(name: &str, agent_home: &Path, out: &Path) -> Result<()> {
    let profile_path = agent_home.join("profile.yaml");
    let profile_yaml = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let profile: AgentProfile = serde_yaml_ng::from_str(&profile_yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;

    // Agents created outside `mur agent create` (e.g. the Hub's seeded
    // concierge) may not have a keypair yet — mint one so export works.
    let identity = match AgentIdentity::load(agent_home) {
        Ok(id) => id,
        Err(mur_common::identity::IdentityError::NotFound) => {
            let id = AgentIdentity::generate();
            id.save(agent_home)
                .with_context(|| format!("mint agent identity in {}", agent_home.display()))?;
            id
        }
        Err(e) => {
            return Err(e)
                .with_context(|| format!("load agent identity from {}", agent_home.display()));
        }
    };

    let mur_version = env!("CARGO_PKG_VERSION");
    let mut manifest = build_manifest_from_profile(&profile, mur_version);

    // If the agent binds via model_ref, the inline-derived hint may be a
    // stale default — override it from the registry entry (§7.1).
    if let Some(model_ref) = profile.model_ref.as_deref() {
        let reg_path = mur_common::model::ModelRegistry::default_path()?;
        let registry = mur_common::model::ModelRegistry::load_from(&reg_path)?;
        if let Some(hint) = model_hint_from_ref(model_ref, &registry) {
            manifest.model_hint = Some(hint);
        }
    }

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

    // System prompt (persona / core instructions) — without this the loaded
    // agent runs with no prompt.
    let sys_prompt_path = agent_home.join("sys_prompt.md");
    if sys_prompt_path.exists() {
        let md = fs::read_to_string(&sys_prompt_path)
            .with_context(|| format!("read {}", sys_prompt_path.display()))?;
        writer.set_sys_prompt(md);
    }

    // Skill backing files — the profile keeps the skill registrations; bundle
    // the files so they aren't dangling references after load.
    let skills_dir = agent_home.join("skills");
    if skills_dir.exists() {
        for entry in fs::read_dir(&skills_dir)
            .with_context(|| format!("read {}", skills_dir.display()))?
            .flatten()
        {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false)
                && let Some(name) = entry.file_name().to_str()
            {
                let data = fs::read(entry.path())
                    .with_context(|| format!("read skill {}", entry.path().display()))?;
                writer.add_skill(name, data);
            }
        }
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

    // Machine-specific / privacy-sensitive paths. A shared or published profile
    // must not leak the exporter's home dir, project layout, or local install
    // paths (e.g. filesystem grants list personal project directories). The
    // recipient re-grants filesystem/process access at install time.
    if !profile.entitlements.filesystem.read.is_empty()
        || !profile.entitlements.filesystem.write.is_empty()
    {
        profile.entitlements.filesystem.read.clear();
        profile.entitlements.filesystem.write.clear();
        removed.push("entitlements.filesystem.{read,write}".to_string());
    }
    // Keep bare binary names (yt-dlp, deno, ffmpeg); drop absolute/home paths.
    let spawn_before = profile.entitlements.processes.spawn.allowed.len();
    profile
        .entitlements
        .processes
        .spawn
        .allowed
        .retain(|p| !p.starts_with('/') && !p.starts_with('~'));
    if profile.entitlements.processes.spawn.allowed.len() != spawn_before {
        removed.push("entitlements.processes.spawn.allowed[path]".to_string());
    }
    // MCP command → basename (the manifest already carries the basename); drop
    // the machine-local binary hash.
    for m in &mut profile.mcp_servers {
        if let Some(base) = std::path::Path::new(&m.command).file_name() {
            let base = base.to_string_lossy().into_owned();
            if base != m.command {
                m.command = base;
                removed.push("mcp_servers[].command[path]".to_string());
            }
        }
        if m.binary_sha256.take().is_some() {
            removed.push("mcp_servers[].binary_sha256".to_string());
        }
    }
    // Appearance: local render directory + machine-specific render timestamp.
    if profile.appearance.expressions_dir.as_path() != std::path::Path::new("expressions") {
        profile.appearance.expressions_dir = std::path::PathBuf::from("expressions");
        removed.push("appearance.expressions_dir".to_string());
    }
    if profile.appearance.last_rendered_at.take().is_some() {
        removed.push("appearance.last_rendered_at".to_string());
    }

    removed
}

/// Resolve a `model_ref` against a registry into a `ModelHint`. Returns
/// `None` when the registry has no such entry (the inline-derived hint
/// then stands).
fn model_hint_from_ref(
    model_ref: &str,
    registry: &mur_common::model::ModelRegistry,
) -> Option<mur_common::muragent::manifest::ModelHint> {
    registry
        .models
        .get(model_ref)
        .map(|e| mur_common::muragent::model_class::classify(&e.provider, &e.model))
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

    #[test]
    fn sanitize_strips_machine_specific_paths() {
        let mut p = AgentProfile::default_for_tests();
        p.entitlements.filesystem.read = vec!["/Users/alice/Projects/secret".into()];
        p.entitlements.filesystem.write = vec!["/Users/alice/Projects/secret".into()];
        p.entitlements.processes.spawn.allowed =
            vec!["yt-dlp".into(), "/Users/alice/.mur/mcp-servers/x".into()];
        p.mcp_servers = vec![mur_common::agent::McpServerEntry {
            name: "media".into(),
            command: "/Users/alice/.mur/mcp-servers/mur-mcp-server".into(),
            binary_sha256: Some("deadbeef".into()),
            ..Default::default()
        }];
        p.appearance.expressions_dir = "/Users/alice/.mur/agents/x/expressions".into();
        p.appearance.last_rendered_at = Some(chrono::Utc::now());

        let _ = sanitize_profile_for_export(&mut p);

        assert!(p.entitlements.filesystem.read.is_empty());
        assert!(p.entitlements.filesystem.write.is_empty());
        assert_eq!(
            p.entitlements.processes.spawn.allowed,
            vec!["yt-dlp".to_string()]
        );
        assert_eq!(p.mcp_servers[0].command, "mur-mcp-server");
        assert!(p.mcp_servers[0].binary_sha256.is_none());
        assert_eq!(
            p.appearance.expressions_dir,
            std::path::PathBuf::from("expressions")
        );
        assert!(p.appearance.last_rendered_at.is_none());
        // No exporter home path may survive anywhere in the serialized profile.
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        assert!(!yaml.contains("/Users/alice"), "leaked path: {yaml}");
    }

    #[test]
    fn model_hint_override_uses_registry_entry() {
        use mur_common::model::{ModelEntry, ModelRegistry};
        let reg = ModelRegistry {
            schema_version: 1,
            models: std::collections::BTreeMap::from([(
                "anthropic_opus_4_7".to_string(),
                ModelEntry {
                    provider: "anthropic".into(),
                    model: "claude-opus-4-7".into(),
                    base_url: None,
                    secret: None,
                    capabilities: vec![],
                    params: serde_json::Value::Null,
                    tier: None,
                    cost_per_1k_tokens: None,
                    ..Default::default()
                },
            )]),
            roles: Default::default(),
        };
        let hint = model_hint_from_ref("anthropic_opus_4_7", &reg).expect("resolved");
        assert_eq!(
            hint.tier,
            mur_common::muragent::manifest::ModelTier::Frontier
        );
        assert!(!hint.local_capable);
    }

    #[test]
    fn removed_formats_redirect() {
        for fmt in ["bin", "gui"] {
            let err = cmd_export("coach", "/tmp/out", fmt)
                .unwrap_err()
                .to_string();
            assert!(err.contains(".muragent"), "fmt {fmt}: {err}");
            assert!(err.contains("--load"), "fmt {fmt}: {err}");
        }
    }

    #[test]
    fn export_entry_point_writes_muragent() {
        use mur_common::identity::AgentIdentity;
        let tmp = tempfile::TempDir::new().unwrap();
        let home = tmp.path().join("agents").join("coach");
        std::fs::create_dir_all(&home).unwrap();
        let mut p = mur_common::AgentProfile::default_for_tests();
        p.name = "coach".into();
        std::fs::write(
            home.join("profile.yaml"),
            serde_yaml_ng::to_string(&p).unwrap(),
        )
        .unwrap();
        AgentIdentity::generate().save(&home).unwrap();

        let out = tmp.path().join("coach.muragent");
        export_muragent("coach", &home, &out).unwrap();
        assert!(out.exists(), ".muragent written");
    }
}
