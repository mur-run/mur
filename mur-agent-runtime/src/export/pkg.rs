//! .murpkg archive format — create an agent package tarball.

use anyhow::{Context, Result, anyhow, bail};
use flate2::Compression;
use flate2::write::GzEncoder;
use mur_common::AgentProfile;
use mur_common::agent::NotificationTarget;
use serde::Serialize;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tar::Builder;

#[derive(Debug, Serialize)]
struct Manifest {
    schema: &'static str,
    exported_at: String,
    original_uuid: String,
    name: String,
    sanitized: SanitizedReport,
    prerequisites: Prerequisites,
}

#[derive(Debug, Default, Serialize)]
struct SanitizedReport {
    removed_fields: Vec<String>,
}

#[derive(Debug, Default, Serialize)]
struct Prerequisites {
    mcp_servers: Vec<McpPrereq>,
}

#[derive(Debug, Serialize)]
struct McpPrereq {
    name: String,
    command_basename: String,
}

/// Create a .murpkg tar.gz at `out_path` from an agent home directory.
pub fn export_to_pkg(agent_home: &Path, out_path: &Path) -> Result<()> {
    let profile_path = agent_home.join("profile.yaml");
    if !profile_path.exists() {
        bail!(
            "agent_home '{}' is missing profile.yaml",
            agent_home.display()
        );
    }
    let yaml = fs::read_to_string(&profile_path)
        .with_context(|| format!("read {}", profile_path.display()))?;
    let mut profile: AgentProfile = serde_yaml_ng::from_str(&yaml)
        .with_context(|| format!("parse {}", profile_path.display()))?;

    let removed = sanitize_profile(&mut profile);

    let manifest = Manifest {
        schema: "mur-agent-package/1",
        exported_at: chrono::Utc::now().to_rfc3339(),
        original_uuid: profile.id.clone(),
        name: profile.name.clone(),
        sanitized: SanitizedReport {
            removed_fields: removed,
        },
        prerequisites: Prerequisites {
            mcp_servers: profile
                .mcp_servers
                .iter()
                .map(|s| McpPrereq {
                    name: s.name.clone(),
                    command_basename: PathBuf::from(&s.command)
                        .file_name()
                        .and_then(|n| n.to_str())
                        .unwrap_or(&s.command)
                        .to_string(),
                })
                .collect(),
        },
    };
    let manifest_yaml = serde_yaml_ng::to_string(&manifest).context("serialize manifest")?;
    let sanitized_profile_yaml =
        serde_yaml_ng::to_string(&profile).context("serialize sanitized profile")?;
    let readme = render_readme(&profile, &manifest);

    let file = File::create(out_path).with_context(|| format!("create {}", out_path.display()))?;
    let gz = GzEncoder::new(file, Compression::default());
    let mut tar = Builder::new(gz);

    add_blob(&mut tar, "manifest.yaml", manifest_yaml.as_bytes())?;
    add_blob(&mut tar, "profile.yaml", sanitized_profile_yaml.as_bytes())?;
    add_blob(&mut tar, "README.md", readme.as_bytes())?;

    let prompt_path = agent_home.join("sys_prompt.md");
    if prompt_path.exists() {
        let bytes = fs::read(&prompt_path)?;
        add_blob(&mut tar, "sys_prompt.md", &bytes)?;
    }

    let skills_dir = agent_home.join("skills");
    if skills_dir.exists() {
        for entry in fs::read_dir(&skills_dir)?.flatten() {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                let bytes = fs::read(entry.path())?;
                let name = format!(
                    "skills/{}",
                    entry
                        .file_name()
                        .to_str()
                        .ok_or_else(|| anyhow!("skill basename not utf-8"))?
                );
                add_blob(&mut tar, &name, &bytes)?;
            }
        }
    }

    tar.into_inner()
        .context("close tar")?
        .finish()
        .context("flush gzip")?;
    Ok(())
}

fn add_blob<W: std::io::Write>(tar: &mut Builder<W>, name: &str, bytes: &[u8]) -> Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    tar.append_data(&mut header, name, bytes)
        .with_context(|| format!("tar append {name}"))
}

fn sanitize_profile(profile: &mut AgentProfile) -> Vec<String> {
    let mut removed = Vec::new();
    // Strip notification targets that may carry secrets (webhook URLs,
    // env-var indirections that name a secret) — record what was stripped.
    let mut new_targets = Vec::with_capacity(profile.notifications.on_task_complete.len());
    for t in profile.notifications.on_task_complete.drain(..) {
        if is_secretful(&t) {
            removed.push(notification_label(&t, "on_task_complete"));
        } else {
            new_targets.push(t);
        }
    }
    profile.notifications.on_task_complete = new_targets;

    let mut new_err = Vec::with_capacity(profile.notifications.on_error.len());
    for t in profile.notifications.on_error.drain(..) {
        if is_secretful(&t) {
            removed.push(notification_label(&t, "on_error"));
        } else {
            new_err.push(t);
        }
    }
    profile.notifications.on_error = new_err;

    let mut new_shut = Vec::with_capacity(profile.notifications.on_shutdown.len());
    for t in profile.notifications.on_shutdown.drain(..) {
        if is_secretful(&t) {
            removed.push(notification_label(&t, "on_shutdown"));
        } else {
            new_shut.push(t);
        }
    }
    profile.notifications.on_shutdown = new_shut;

    // Strip socket auth token_file references so packaged agents don't carry
    // a host-local secret path.
    if profile.transport.socket.auth.is_some() {
        removed.push("transport.socket.auth.token_file".to_string());
        profile.transport.socket.auth = None;
    }

    removed
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

fn notification_label(t: &NotificationTarget, lifecycle: &str) -> String {
    let kind = match t {
        NotificationTarget::Webhook { .. } => "webhook.url",
        NotificationTarget::Slack { .. } => "slack.webhook_url_env",
        NotificationTarget::Webpush { .. } => "webpush.url",
        NotificationTarget::Email { .. } => "email.smtp_config_file",
        NotificationTarget::Agent { .. } => "agent",
        NotificationTarget::Commander => "commander",
    };
    format!("notifications.{lifecycle}[].{kind}")
}

fn render_readme(profile: &AgentProfile, manifest: &Manifest) -> String {
    let mut buf = String::new();
    buf.push_str(&format!(
        "# {} ({})\n\n",
        profile.display_name, profile.name
    ));
    buf.push_str(&format!("Exported: {}\n\n", manifest.exported_at));
    buf.push_str(&format!("{}\n\n", profile.persona.description));
    if !manifest.prerequisites.mcp_servers.is_empty() {
        buf.push_str("## MCP prerequisites\n\n");
        for s in &manifest.prerequisites.mcp_servers {
            buf.push_str(&format!("- {} (`{}`)\n", s.name, s.command_basename));
        }
    }
    if !manifest.sanitized.removed_fields.is_empty() {
        buf.push_str(
            "\n## Sanitized fields\n\nThe following fields were stripped before packaging:\n",
        );
        for f in &manifest.sanitized.removed_fields {
            buf.push_str(&format!("- `{f}`\n"));
        }
    }
    buf
}
