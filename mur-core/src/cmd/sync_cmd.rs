use anyhow::{Context, Result};

use crate::capture;
use crate::inject;
use crate::store::yaml::YamlStore;

/// Run device sync (cloud API or git pull/commit/push) based on config.
/// Returns Ok(()) on success, warns on failure but doesn't block.
fn resolve_team_id(
    cli_team: Option<&str>,
    config: &mur_common::config::SyncConfig,
) -> Option<String> {
    cli_team
        .map(|s| s.to_string())
        .or_else(|| std::env::var("MUR_TEAM_ID").ok())
        .or_else(|| config.team_id.clone())
}

pub(crate) async fn device_sync(
    quiet: bool,
    direction: DeviceSyncDirection,
    team: Option<&str>,
) -> Result<()> {
    let config = crate::store::config::load_config()?;

    match config.sync.method.as_str() {
        "cloud" => {
            if !quiet {
                eprintln!("  ☁ Cloud sync ({})...", direction.label());
            }
            // Cloud sync via server API — requires authentication
            let server_url = &config.server.url;
            let mur_dir = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("no home dir"))?
                .join(".mur");
            let token = match crate::auth::load_tokens() {
                Some(t) => t.access_token,
                None => {
                    if !quiet {
                        eprintln!("  ⚠ Not authenticated. Run `mur auth login` for cloud sync.");
                    }
                    return Ok(());
                }
            };

            // Resolve team ID for pattern sync (CLI > env > config)
            let team_id = resolve_team_id(team, &config.sync);
            if team_id.is_none()
                && matches!(
                    direction,
                    DeviceSyncDirection::Pull | DeviceSyncDirection::Both
                )
                && !quiet
            {
                eprintln!(
                    "  ⚠ Cloud pattern sync skipped: no team ID. Pass --team <id> or set MUR_TEAM_ID."
                );
            }

            match direction {
                DeviceSyncDirection::Pull => {
                    let device_id = crate::auth::get_device_id();
                    let device_name = crate::auth::get_device_name();
                    let device_os = crate::auth::get_device_os();
                    let client = reqwest::Client::new();

                    // Pattern sync — team-scoped, versioned
                    if let Some(ref tid) = team_id {
                        let version_path = mur_dir.join(".sync_version");
                        let local_version: i64 = std::fs::read_to_string(&version_path)
                            .ok()
                            .and_then(|s| s.trim().parse().ok())
                            .unwrap_or(0);
                        let pull_url = format!(
                            "{}/api/v1/core/teams/{}/sync/pull?since={}",
                            server_url, tid, local_version
                        );
                        let resp = client
                            .get(&pull_url)
                            .timeout(std::time::Duration::from_secs(10))
                            .header("Authorization", format!("Bearer {}", token))
                            .header("X-Device-ID", &device_id)
                            .header("X-Device-Name", &device_name)
                            .header("X-Device-OS", &device_os)
                            .send()
                            .await;
                        match resp {
                            Ok(r) if r.status().is_success() => {
                                let body = r.text().await.unwrap_or_default();
                                match serde_json::from_str::<mur_common::sync_types::SyncPullResponse>(
                                    &body,
                                ) {
                                    Ok(pull) => {
                                        apply_cloud_pull_v2(&pull, &mur_dir)?;
                                        if let Err(e) =
                                            std::fs::write(&version_path, pull.version.to_string())
                                        {
                                            tracing::warn!("Failed to write sync version: {e}");
                                        }
                                        if !quiet {
                                            eprintln!(
                                                "  ✓ Cloud pull complete (version {}).",
                                                pull.version
                                            );
                                        }
                                    }
                                    Err(e) => {
                                        if !quiet {
                                            eprintln!("  ⚠ Cloud pull parse error: {e}");
                                        }
                                    }
                                }
                            }
                            Ok(r) => {
                                if !quiet {
                                    eprintln!("  ⚠ Cloud pull failed: HTTP {}", r.status());
                                }
                            }
                            Err(e) => {
                                if !quiet {
                                    eprintln!("  ⚠ Cloud pull failed: {}", e);
                                }
                            }
                        }
                    }

                    // ── Schedule sync (pull) ──────────────────────────────
                    if !quiet {
                        eprintln!("  ☁ Pulling schedules...");
                    }
                    let sched_url = format!("{}/api/v1/schedules", server_url);
                    let sched_resp = client
                        .get(&sched_url)
                        .timeout(std::time::Duration::from_secs(10))
                        .header("Authorization", format!("Bearer {}", token))
                        .header("X-Device-ID", &device_id)
                        .header("X-Device-Name", &device_name)
                        .header("X-Device-OS", &device_os)
                        .send()
                        .await;

                    match sched_resp {
                        Ok(r) if r.status().is_success() => {
                            let body = r.text().await.unwrap_or_default();
                            if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&body)
                                && let Some(data) = resp.get("data").and_then(|d| d.as_array())
                            {
                                let mut schedules: Vec<mur_common::schedule::Schedule> = Vec::new();
                                for item in data {
                                    let sched = mur_common::schedule::Schedule {
                                        id: item
                                            .get("id")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or_default()
                                            .to_string(),
                                        workflow: item
                                            .get("workflow_name")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or_default()
                                            .to_string(),
                                        cron: item
                                            .get("cron_expr")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or_default()
                                            .to_string(),
                                        timezone: item
                                            .get("timezone")
                                            .and_then(|v| v.as_str())
                                            .unwrap_or("UTC")
                                            .to_string(),
                                        enabled: item
                                            .get("enabled")
                                            .and_then(|v| v.as_bool())
                                            .unwrap_or(true),
                                        user_id: String::new(),
                                        variables: Default::default(),
                                        notify: mur_common::schedule::ScheduleNotify {
                                            notify_type: item
                                                .get("notify_type")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or_default()
                                                .to_string(),
                                            target: item
                                                .get("notify_target")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or_default()
                                                .to_string(),
                                        },
                                        on_missed: Default::default(),
                                        executor: mur_common::schedule::ScheduleExecutor::Server,
                                    };
                                    schedules.push(sched);
                                }

                                if !schedules.is_empty() {
                                    // Merge with existing local schedules instead of overwriting
                                    let existing_schedules =
                                        mur_common::schedule_claim::load_schedules()
                                            .unwrap_or_default();
                                    let server_workflow_names: std::collections::HashSet<String> =
                                        schedules.iter().map(|s| s.workflow.clone()).collect();

                                    // Keep local-only schedules (not on server)
                                    for local in existing_schedules {
                                        if !server_workflow_names.contains(&local.workflow) {
                                            schedules.push(local);
                                        }
                                    }

                                    let file = mur_common::schedule::SchedulesFile { schedules };
                                    let yaml = serde_yaml::to_string(&file)?;
                                    let path = mur_dir.join("schedules.yaml");
                                    std::fs::write(&path, yaml)?;
                                    if !quiet {
                                        eprintln!(
                                            "  ✓ Pulled {} schedule(s) from server.",
                                            data.len()
                                        );
                                    }
                                }
                            }
                        }
                        Ok(r) => {
                            if !quiet {
                                eprintln!("  ⚠ Schedule pull failed: HTTP {}", r.status());
                            }
                        }
                        Err(e) => {
                            if !quiet {
                                eprintln!("  ⚠ Schedule pull failed: {}", e);
                            }
                        }
                    }

                    // ── Workflow sync (pull) ──────────────────────────────
                    if !quiet {
                        eprintln!("  ☁ Pulling workflows...");
                    }
                    let wf_url = format!("{}/api/v1/workflows", server_url);
                    let wf_resp = client
                        .get(&wf_url)
                        .timeout(std::time::Duration::from_secs(10))
                        .header("Authorization", format!("Bearer {}", token))
                        .header("X-Device-ID", &device_id)
                        .header("X-Device-Name", &device_name)
                        .header("X-Device-OS", &device_os)
                        .send()
                        .await;

                    match wf_resp {
                        Ok(r) if r.status().is_success() => {
                            let body = r.text().await.unwrap_or_default();
                            if let Ok(resp) = serde_json::from_str::<serde_json::Value>(&body)
                                && let Some(data) = resp.get("data").and_then(|d| d.as_array())
                            {
                                let workflows_dir = mur_dir.join("workflows");
                                std::fs::create_dir_all(&workflows_dir)?;
                                let mut pulled = 0u32;
                                for item in data {
                                    let name = item
                                        .get("name")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_default();
                                    let yaml_content = item
                                        .get("yaml_content")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or_default();
                                    if name.is_empty() || yaml_content.is_empty() {
                                        continue;
                                    }
                                    // Sanitize name to prevent path traversal
                                    let safe_name = name.replace(['/', '\\', '~'], "_");
                                    if safe_name.is_empty()
                                        || safe_name.contains("..")
                                        || safe_name.starts_with('-')
                                    {
                                        continue;
                                    }
                                    let path = workflows_dir.join(format!("{}.yaml", safe_name));
                                    if !path.starts_with(&workflows_dir) {
                                        continue;
                                    }
                                    std::fs::write(&path, yaml_content)?;
                                    pulled += 1;
                                }
                                if !quiet && pulled > 0 {
                                    eprintln!("  ✓ Pulled {} workflow(s) from server.", pulled);
                                }
                            }
                        }
                        Ok(r) => {
                            if !quiet {
                                eprintln!("  ⚠ Workflow pull failed: HTTP {}", r.status());
                            }
                        }
                        Err(e) => {
                            if !quiet {
                                eprintln!("  ⚠ Workflow pull failed: {}", e);
                            }
                        }
                    }
                }
                DeviceSyncDirection::Push => {
                    let device_id = crate::auth::get_device_id();
                    let device_name = crate::auth::get_device_name();
                    let device_os = crate::auth::get_device_os();
                    let client = reqwest::Client::new();

                    // Pattern push — team-scoped, optimistic concurrency
                    if let Some(ref tid) = team_id {
                        let patterns_dir = mur_dir.join("patterns");
                        let manifest_path = mur_dir.join(".sync_manifest.json");
                        let version_path = mur_dir.join(".sync_version");
                        let local_version: i64 = std::fs::read_to_string(&version_path)
                            .ok()
                            .and_then(|s| s.trim().parse().ok())
                            .unwrap_or(0);

                        let changes = build_sync_changes(&patterns_dir, &manifest_path)?;
                        if changes.is_empty() {
                            if !quiet {
                                eprintln!("  ✓ Nothing to push (no changes).");
                            }
                        } else {
                            let push_url =
                                format!("{}/api/v1/core/teams/{}/sync/push", server_url, tid);
                            let req = mur_common::sync_types::SyncPushRequest {
                                base_version: local_version,
                                changes,
                                force_local: false,
                            };
                            let body = serde_json::to_string(&req)?;
                            let resp = client
                                .post(&push_url)
                                .timeout(std::time::Duration::from_secs(15))
                                .header("Authorization", format!("Bearer {}", token))
                                .header("X-Device-ID", &device_id)
                                .header("X-Device-Name", &device_name)
                                .header("X-Device-OS", &device_os)
                                .header("Content-Type", "application/json")
                                .body(body)
                                .send()
                                .await;
                            match resp {
                                Ok(r) if r.status().is_success() => {
                                    let resp_body = r.text().await.unwrap_or_default();
                                    match serde_json::from_str::<
                                        mur_common::sync_types::SyncPushResponse,
                                    >(&resp_body)
                                    {
                                        Ok(pr) if pr.ok => {
                                            // Update manifest after successful push
                                            update_manifest_after_push(
                                                &patterns_dir,
                                                &manifest_path,
                                            )?;
                                            if let Some(v) = pr.version {
                                                let _ =
                                                    std::fs::write(&version_path, v.to_string());
                                            }
                                            if !quiet {
                                                eprintln!("  ✓ Cloud push complete.");
                                            }
                                        }
                                        Ok(pr) if pr.conflict.unwrap_or(false) => {
                                            // Pull latest, re-diff, retry once
                                            if !quiet {
                                                eprintln!(
                                                    "  ↺ Conflict detected, pulling latest..."
                                                );
                                            }
                                            if let Err(e) = sync_pull_once(
                                                server_url,
                                                tid,
                                                &token,
                                                &mur_dir,
                                                &client,
                                                &device_id,
                                                &device_name,
                                                &device_os,
                                            )
                                            .await
                                                && !quiet
                                            {
                                                eprintln!(
                                                    "  ⚠ Pull during conflict resolution failed: {e}"
                                                );
                                            }
                                            let changes2 =
                                                build_sync_changes(&patterns_dir, &manifest_path)?;
                                            if changes2.is_empty() {
                                                if !quiet {
                                                    eprintln!(
                                                        "  ✓ Resolved after pull (no remaining changes)."
                                                    );
                                                }
                                            } else {
                                                let sv = std::fs::read_to_string(&version_path)
                                                    .ok()
                                                    .and_then(|s| s.trim().parse().ok())
                                                    .unwrap_or(0);
                                                let req2 =
                                                    mur_common::sync_types::SyncPushRequest {
                                                        base_version: sv,
                                                        changes: changes2,
                                                        force_local: false,
                                                    };
                                                let body2 = serde_json::to_string(&req2)?;
                                                let resp2 = client
                                                    .post(&push_url)
                                                    .timeout(std::time::Duration::from_secs(15))
                                                    .header(
                                                        "Authorization",
                                                        format!("Bearer {}", token),
                                                    )
                                                    .header("X-Device-ID", &device_id)
                                                    .header("X-Device-Name", &device_name)
                                                    .header("X-Device-OS", &device_os)
                                                    .header("Content-Type", "application/json")
                                                    .body(body2)
                                                    .send()
                                                    .await;
                                                match resp2 {
                                                    Ok(r2) if r2.status().is_success() => {
                                                        let _ = serde_json::from_str::<mur_common::sync_types::SyncPushResponse>(&r2.text().await.unwrap_or_default()).ok().and_then(|pr2| pr2.version).map(|v| std::fs::write(&version_path, v.to_string()));
                                                        update_manifest_after_push(
                                                            &patterns_dir,
                                                            &manifest_path,
                                                        )?;
                                                        if !quiet {
                                                            eprintln!(
                                                                "  ✓ Push resolved after retry."
                                                            );
                                                        }
                                                    }
                                                    _ => {
                                                        if !quiet {
                                                            eprintln!(
                                                                "  ⚠ Push retry failed; run `mur sync` to retry."
                                                            );
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                        _ => {
                                            if !quiet {
                                                eprintln!(
                                                    "  ⚠ Cloud push failed: unexpected response"
                                                );
                                            }
                                        }
                                    }
                                }
                                Ok(r) => {
                                    if !quiet {
                                        eprintln!("  ⚠ Cloud push failed: HTTP {}", r.status());
                                    }
                                }
                                Err(e) => {
                                    if !quiet {
                                        eprintln!("  ⚠ Cloud push failed: {}", e);
                                    }
                                }
                            }
                        }
                    }

                    // Also push unsynced session recordings
                    if let Err(e) =
                        crate::session::cloud::push_unsynced(server_url, &token, quiet).await
                        && !quiet
                    {
                        eprintln!("  ⚠ Session push failed: {}", e);
                    }

                    // Also push unsynced workflows
                    if let Err(e) = push_unsynced_workflows(server_url, &token, quiet).await
                        && !quiet
                    {
                        eprintln!("  ⚠ Workflow push failed: {}", e);
                    }

                    // ── Schedule sync (push) ──────────────────────────────
                    if !quiet {
                        eprintln!("  ☁ Syncing schedules...");
                    }
                    let schedules_path = mur_dir.join("schedules.yaml");
                    if schedules_path.exists() {
                        let content = std::fs::read_to_string(&schedules_path)?;
                        let file: mur_common::schedule::SchedulesFile = serde_yaml::from_str(
                            &content,
                        )
                        .unwrap_or(mur_common::schedule::SchedulesFile { schedules: vec![] });

                        if !file.schedules.is_empty() {
                            let payload = serde_json::json!({
                                "schedules": file.schedules.iter().map(|s| serde_json::json!({
                                    "workflow_name": s.workflow,
                                    "cron_expr": s.cron,
                                    "timezone": s.timezone,
                                    "enabled": s.enabled,
                                    "notify_type": s.notify.notify_type,
                                    "notify_target": s.notify.target,
                                })).collect::<Vec<_>>(),
                            });

                            let sched_url = format!("{}/api/v1/schedules/sync", server_url);
                            let resp = client
                                .post(&sched_url)
                                .timeout(std::time::Duration::from_secs(10))
                                .header("Authorization", format!("Bearer {}", token))
                                .header("X-Device-ID", &device_id)
                                .header("X-Device-Name", &device_name)
                                .header("X-Device-OS", &device_os)
                                .json(&payload)
                                .send()
                                .await;

                            match resp {
                                Ok(r) if r.status().is_success() => {
                                    if !quiet {
                                        eprintln!(
                                            "  ✓ Synced {} schedule(s).",
                                            file.schedules.len()
                                        );
                                    }
                                }
                                Ok(r) => {
                                    if !quiet {
                                        eprintln!("  ⚠ Schedule sync failed: HTTP {}", r.status());
                                    }
                                }
                                Err(e) => {
                                    if !quiet {
                                        eprintln!("  ⚠ Schedule sync failed: {}", e);
                                    }
                                }
                            }
                        }
                    }
                }
                DeviceSyncDirection::Both => {
                    Box::pin(device_sync(quiet, DeviceSyncDirection::Pull, team)).await?;
                    Box::pin(device_sync(quiet, DeviceSyncDirection::Push, team)).await?;
                }
            }
        }
        "git" => {
            let remote = config.sync.git_remote.as_deref().unwrap_or("");
            if remote.is_empty() {
                if !quiet {
                    eprintln!(
                        "  ⚠ Git sync configured but no remote URL set. Update sync.git_remote in config."
                    );
                }
                return Ok(());
            }
            let mur_dir = dirs::home_dir()
                .ok_or_else(|| anyhow::anyhow!("no home dir"))?
                .join(".mur");

            // Initialize git repo in ~/.mur if needed
            if !mur_dir.join(".git").exists() {
                run_git_in(&mur_dir, &["init"])?;
                run_git_in(&mur_dir, &["remote", "add", "origin", remote])?;
            }

            match direction {
                DeviceSyncDirection::Pull => {
                    let branch = detect_git_branch(&mur_dir);
                    if !quiet {
                        eprintln!("  📥 Git pull...");
                    }
                    match run_git_in(&mur_dir, &["pull", "--rebase", "origin", &branch]) {
                        Ok(_) => {
                            if !quiet {
                                eprintln!("  ✓ Git pull complete.");
                            }
                        }
                        Err(e) => {
                            if !quiet {
                                eprintln!("  ⚠ Git pull failed: {}", e);
                            }
                        }
                    }
                }
                DeviceSyncDirection::Push => {
                    let branch = detect_git_branch(&mur_dir);
                    if !quiet {
                        eprintln!("  📤 Git push...");
                    }
                    let _ = run_git_in(&mur_dir, &["add", "skills/", "workflows/", "config.yaml"]);
                    let commit_result =
                        run_git_in(&mur_dir, &["commit", "-m", "mur: auto-sync patterns"]);
                    // Commit may fail if nothing changed — that's fine
                    if commit_result.is_ok() {
                        match run_git_in(&mur_dir, &["push", "origin", &branch]) {
                            Ok(_) => {
                                if !quiet {
                                    eprintln!("  ✓ Git push complete.");
                                }
                            }
                            Err(e) => {
                                if !quiet {
                                    eprintln!("  ⚠ Git push failed: {}", e);
                                }
                            }
                        }
                    } else if !quiet {
                        eprintln!("  ✓ Nothing to push (no changes).");
                    }
                }
                DeviceSyncDirection::Both => {
                    Box::pin(device_sync(quiet, DeviceSyncDirection::Pull, team)).await?;
                    Box::pin(device_sync(quiet, DeviceSyncDirection::Push, team)).await?;
                }
            }
        }
        _ => {
            // "local" or unknown — no device sync
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum DeviceSyncDirection {
    Pull,
    Push,
    Both,
}

impl DeviceSyncDirection {
    fn label(self) -> &'static str {
        match self {
            Self::Pull => "pull",
            Self::Push => "push",
            Self::Both => "pull+push",
        }
    }
}

/// Detect the default branch name (main or master).
fn detect_git_branch(dir: &std::path::Path) -> String {
    // Try to get current branch
    if let Ok(output) = std::process::Command::new("git")
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .current_dir(dir)
        .output()
    {
        let branch = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if !branch.is_empty() && branch != "HEAD" {
            return branch;
        }
    }
    // Fallback: check if main or master exists
    if std::process::Command::new("git")
        .args(["show-ref", "--verify", "--quiet", "refs/heads/main"])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
    {
        return "main".to_string();
    }
    "main".to_string()
}

fn run_git_in(dir: &std::path::Path, args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr)
    }
}

fn apply_cloud_pull_v2(
    response: &mur_common::sync_types::SyncPullResponse,
    mur_dir: &std::path::Path,
) -> Result<()> {
    // Legacy cloud pattern payloads are ignored (workflow-engine v2 P1a removed
    // the pattern pipeline); skills/workflows sync via their own channels.
    if !response.patterns.is_empty() {
        tracing::debug!(
            "ignoring {} legacy cloud pattern payload(s)",
            response.patterns.len()
        );
    }
    let _ = mur_dir;
    Ok(())
}

/// Build change list for cloud push by comparing local patterns dir with
/// the sync manifest (`~/.mur/.sync_manifest.json`).
///
/// Manifest format: `{ "name": { "server_id": "...", "version": 0, "content_hash": "..." } }`
fn build_sync_changes(
    patterns_dir: &std::path::Path,
    manifest_path: &std::path::Path,
) -> Result<Vec<mur_common::sync_types::PatternChange>> {
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};

    let mut changes = Vec::new();

    // Load manifest
    let manifest: HashMap<String, serde_json::Value> = if manifest_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(manifest_path)?).unwrap_or_default()
    } else {
        HashMap::new()
    };

    let mut seen = std::collections::HashSet::new();

    // Scan local patterns
    if patterns_dir.exists() {
        for entry in std::fs::read_dir(patterns_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let content = std::fs::read_to_string(&path)?;
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            content.hash(&mut hasher);
            let hash = format!("{:x}", hasher.finish());

            seen.insert(name.clone());

            match manifest.get(&name) {
                Some(entry) => {
                    let prev_hash = entry
                        .get("content_hash")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");
                    if prev_hash != hash {
                        changes.push(mur_common::sync_types::PatternChange {
                            action: "update".into(),
                            id: entry
                                .get("server_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            pattern: Some(mur_common::sync_types::PatternPayload {
                                name: name.clone(),
                                content,
                            }),
                        });
                    }
                }
                None => {
                    changes.push(mur_common::sync_types::PatternChange {
                        action: "create".into(),
                        id: None,
                        pattern: Some(mur_common::sync_types::PatternPayload {
                            name: name.clone(),
                            content,
                        }),
                    });
                }
            }
        }
    }

    // Deleted patterns (in manifest but not on disk)
    for (name, entry) in &manifest {
        if !seen.contains(name) {
            changes.push(mur_common::sync_types::PatternChange {
                action: "delete".into(),
                id: entry
                    .get("server_id")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                pattern: None,
            });
        }
    }

    Ok(changes)
}

/// After a successful push, rebuild the sync manifest from local state.
fn update_manifest_after_push(
    patterns_dir: &std::path::Path,
    manifest_path: &std::path::Path,
) -> Result<()> {
    use std::collections::HashMap;
    use std::hash::{Hash, Hasher};

    let mut manifest: HashMap<String, serde_json::Value> = HashMap::new();

    if patterns_dir.exists() {
        for entry in std::fs::read_dir(patterns_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
                continue;
            }
            let name = path
                .file_stem()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            content.hash(&mut hasher);
            let hash = format!("{:x}", hasher.finish());
            let mut entry = serde_json::Map::new();
            entry.insert("content_hash".into(), serde_json::Value::String(hash));
            // Preserve server_id if known
            manifest.insert(name, serde_json::Value::Object(entry));
        }
    }

    // Merge with existing manifest to preserve server_ids
    if manifest_path.exists()
        && let Ok(old) = serde_json::from_str::<HashMap<String, serde_json::Value>>(
            &std::fs::read_to_string(manifest_path)?,
        )
    {
        for (name, entry) in manifest.iter_mut() {
            if let Some(old_entry) = old.get(name)
                && let Some(sid) = old_entry.get("server_id")
                && let Some(obj) = entry.as_object_mut()
            {
                obj.insert("server_id".into(), sid.clone());
            }
        }
    }

    // Write merged manifest
    std::fs::write(manifest_path, serde_json::to_string(&manifest)?)?;

    Ok(())
}

/// One-shot pull for conflict resolution during push retry.
#[allow(clippy::too_many_arguments)]
async fn sync_pull_once(
    server_url: &str,
    team_id: &str,
    token: &str,
    mur_dir: &std::path::Path,
    client: &reqwest::Client,
    device_id: &str,
    device_name: &str,
    device_os: &str,
) -> Result<()> {
    let version_path = mur_dir.join(".sync_version");
    let local_version: i64 = std::fs::read_to_string(&version_path)
        .ok()
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or(0);
    let url = format!(
        "{}/api/v1/core/teams/{}/sync/pull?since={}",
        server_url, team_id, local_version
    );
    let resp = client
        .get(&url)
        .timeout(std::time::Duration::from_secs(10))
        .header("Authorization", format!("Bearer {}", token))
        .header("X-Device-ID", device_id)
        .header("X-Device-Name", device_name)
        .header("X-Device-OS", device_os)
        .send()
        .await?;
    let body = resp.text().await?;
    let pull: mur_common::sync_types::SyncPullResponse = serde_json::from_str(&body)?;
    apply_cloud_pull_v2(&pull, mur_dir)?;
    std::fs::write(&version_path, pull.version.to_string())?;
    Ok(())
}

/// Simple hash for change detection (not cryptographic).
fn md5_simple(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

pub(crate) async fn cmd_sync(quiet: bool, project_aware: bool, team: Option<&str>) -> Result<()> {
    use inject::sync::{default_targets, generate_sync_content_from_items, write_sync_file};

    // ─── Heartbeat: register device activity ──────────────────
    crate::auth::heartbeat();

    // ─── Device sync first (cloud or git) ─────────────────────
    // Failures warn but don't block tool sync
    if let Err(e) = device_sync(quiet, DeviceSyncDirection::Both, team).await
        && !quiet
    {
        eprintln!("  ⚠ Device sync error: {}", e);
    }

    // Ensure built-in skills are installed BEFORE loading the corpus — on a
    // pristine home the corpus is empty and the early return below would
    // otherwise skip installation forever (issue #593).
    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("HOME directory not found"))?;
    let mur_dir = mur_common::trust::mur_home();
    let skill_installed = ensure_mur_skill(&home, &mur_dir)?;
    if !quiet && skill_installed {
        println!("  🎓 MUR skill installed/updated for AI tools");
    }

    // Skills are the sync content source (workflow-engine v2 P1b).
    let candidates =
        crate::retrieve::skill_candidates::load_skill_candidates(&mur_dir.join("skills"), &mur_dir)
            .unwrap_or_default();

    if candidates.is_empty() {
        if !quiet {
            println!("No skills to sync.");
        }
        return Ok(());
    }

    // Get current working directory for project-scoped sync
    let cwd = std::env::current_dir()?;
    let targets = default_targets();

    // Build project-aware query when --project is set. Resolve git worktrees to
    // the main repo name so sync scopes per-repo, consistent with the index.
    let project_name = crate::codebase::scanner::project_name_from_path(&cwd);

    let sync_query = if project_aware {
        build_project_sync_query(&cwd, &project_name)
    } else {
        project_name.clone()
    };

    for target in &targets {
        let target_path = cwd.join(&target.file);

        // Only write to files that already exist on disk
        if !target_path.exists() {
            continue;
        }

        let scored =
            crate::retrieve::scoring::score_and_rank_generic(&sync_query, candidates.clone());

        let top: Vec<crate::inject::hook::InjectedItem> = scored
            .into_iter()
            .filter(|s| {
                s.item.stats.lifecycle_state != mur_common::skill::stats::LifecycleState::Archived
            })
            .take(target.max_patterns)
            .map(|s| s.item.to_injected_item())
            .collect();

        if top.is_empty() {
            continue;
        }

        let content = generate_sync_content_from_items(&top, &target.format);
        write_sync_file(&target_path, &content, &target.format)?;
        if !quiet {
            println!(
                "  {} — wrote {} skills to {}",
                target.name,
                top.len(),
                target_path.display()
            );
        }
    }

    // ─── Auto-reindex if dirty ───────────────────────────────
    let index_dirty = is_index_dirty(&home);
    if index_dirty {
        if !quiet {
            println!("  🔄 Index outdated — reindexing...");
        }
        match crate::cmd::reindex::cmd_reindex().await {
            Ok(()) => {}
            Err(e) => {
                if !quiet {
                    eprintln!(
                        "  ⚠ Reindex skipped: {} (run `mur reindex` manually or start Ollama)",
                        e
                    );
                }
            }
        }
    } else if !quiet {
        println!("  ✅ Index up to date");
    }

    // ─── Ensure default templates exist ──────────────────────
    ensure_default_templates(&home, quiet)?;

    if !quiet {
        println!("Sync complete.");
    }
    Ok(())
}

/// Bootstrap default template files if they don't exist.
fn ensure_default_templates(home: &std::path::Path, quiet: bool) -> Result<()> {
    let templates_dir = home.join(".mur").join("templates");
    let extract_prompt = templates_dir.join("extract-prompt.md");

    if !extract_prompt.exists() {
        std::fs::create_dir_all(&templates_dir)?;
        std::fs::write(&extract_prompt, crate::cmd::learn::DEFAULT_EXTRACT_PROMPT)?;
        if !quiet {
            println!(
                "  📝 Created default extraction template: {}",
                extract_prompt.display()
            );
        }
    }

    Ok(())
}

/// Check if the LanceDB index is stale compared to pattern/workflow YAML files.
fn is_index_dirty(home: &std::path::Path) -> bool {
    let mur_dir = home.join(".mur");
    let index_dir = mur_dir.join("index");

    // No index → dirty
    if !index_dir.exists() {
        return true;
    }

    // Get index mtime (use the directory mtime as proxy)
    let index_mtime = match std::fs::metadata(&index_dir).and_then(|m| m.modified()) {
        Ok(t) => t,
        Err(_) => return true,
    };

    // Check all YAML files in patterns/ and workflows/
    let dirs_to_check = [mur_dir.join("patterns"), mur_dir.join("workflows")];

    for dir in &dirs_to_check {
        if !dir.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml")
                    && let Ok(meta) = std::fs::metadata(&path)
                    && let Ok(mtime) = meta.modified()
                    && mtime > index_mtime
                {
                    return true;
                }
            }
        }
    }

    false
}

/// Dev-discipline builtin names (spec 2026-07-23). Installation never
/// overwrites a same-named skill the user authored themselves.
const NEW_DEV_SKILL_NAMES: &[&str] = &[
    "mur-dev",
    "mur-grilling",
    "mur-brainstorm",
    "mur-domain-modeling",
    "mur-writing-plans",
    "mur-tickets",
    "mur-executing-plans",
    "mur-delegate-dev",
    "mur-worktree",
    "mur-tdd",
    "mur-debugging",
    "mur-code-review",
    "mur-receiving-review",
    "mur-verification",
    "mur-finishing-branch",
    "mur-merge-conflicts",
    "mur-skill-authoring",
];

/// Publishers whose on-disk copies we own and may update in place.
const MUR_OFFICIAL_PUBLISHERS: &[&str] = &["human:mur-official", "human:mur"];

/// Never-shadow (spec 2026-07-23 §6): true when `name` is a dev-discipline
/// builtin AND `dir/skill.yaml` exists but was not published by MUR.
/// ponytail: publisher-based check; origin_hash edit detection if users
/// report clobbered local edits.
fn dev_skill_shadowed_by_user(dir: &std::path::Path, name: &str) -> bool {
    if !NEW_DEV_SKILL_NAMES.contains(&name) {
        return false;
    }
    let Ok(existing) = std::fs::read_to_string(dir.join("skill.yaml")) else {
        return false;
    };
    match mur_common::skill::parse_canonical(&existing) {
        Ok(m) => !MUR_OFFICIAL_PUBLISHERS.contains(&m.publisher.as_str()),
        Err(_) => true,
    }
}

/// Install/update the MUR skill for AI tools that support skills.
/// Writes canonical copies to ~/.mur/skills/ and symlinks from tool dirs.
/// Returns true if any skill was written.
pub(crate) fn ensure_mur_skill(home: &std::path::Path, mur_root: &std::path::Path) -> Result<bool> {
    let skills: &[(&str, &str)] = &[
        ("mur-context", include_str!("../skills/mur_context.yaml")),
        ("mur-in", include_str!("../skills/mur_in.yaml")),
        ("mur-out", include_str!("../skills/mur_out.yaml")),
        ("mur-run", include_str!("../skills/mur_run.yaml")),
        (
            "mur-native-tools",
            include_str!("../skills/mur_native_tools.yaml"),
        ),
        (
            "mur-agent-manage",
            include_str!("../skills/mur_agent_manage.yaml"),
        ),
        (
            "mur-project-index",
            include_str!("../skills/mur_project_index.yaml"),
        ),
        (
            "mur-project-remove",
            include_str!("../skills/mur_project_remove.yaml"),
        ),
        (
            "mur-project-search",
            include_str!("../skills/mur_project_search.yaml"),
        ),
        ("mur-compress", include_str!("../skills/mur_compress.yaml")),
        (
            "mur-session-remove",
            include_str!("../skills/mur_session_remove.yaml"),
        ),
        ("vlc-control", include_str!("../skills/vlc_control.yaml")),
        (
            "scene-explain",
            include_str!("../skills/scene_explain.yaml"),
        ),
        (
            "video-analyze",
            include_str!("../skills/video_analyze.yaml"),
        ),
        (
            "watch-together",
            include_str!("../skills/watch_together.yaml"),
        ),
        (
            "parallel-code",
            include_str!("../skills/parallel_code.yaml"),
        ),
        (
            "parallel-decompose",
            include_str!("../skills/parallel_decompose.yaml"),
        ),
        (
            "mur-fleet-manage",
            include_str!("../skills/mur_fleet_manage.yaml"),
        ),
        (
            "mur-fleet-loop",
            include_str!("../skills/mur_fleet_loop.yaml"),
        ),
        (
            "mur-fleet-share",
            include_str!("../skills/mur_fleet_share.yaml"),
        ),
        (
            "mur-workflow-author",
            include_str!("../skills/mur_workflow_author.yaml"),
        ),
        (
            "mur-workflow-hitl",
            include_str!("../skills/mur_workflow_hitl.yaml"),
        ),
        (
            "mur-workflow-delegate",
            include_str!("../skills/mur_workflow_delegate.yaml"),
        ),
        (
            "mur-agent-setup",
            include_str!("../skills/mur_agent_setup.yaml"),
        ),
        (
            "mur-agent-mcp-wire",
            include_str!("../skills/mur_agent_mcp_wire.yaml"),
        ),
        (
            "mur-agent-schedule",
            include_str!("../skills/mur_agent_schedule.yaml"),
        ),
        (
            "mur-parallel-exec",
            include_str!("../skills/mur_parallel_exec.yaml"),
        ),
        (
            "mur-parallel-tracks",
            include_str!("../skills/mur_parallel_tracks.yaml"),
        ),
        (
            "mur-parallel-merge",
            include_str!("../skills/mur_parallel_merge.yaml"),
        ),
        (
            "parallel-topology-guide",
            include_str!("../skills/parallel_topology_guide.yaml"),
        ),
        (
            "deep-research-router",
            include_str!("../skills/deep_research_router.yaml"),
        ),
        (
            "deep-research-worker",
            include_str!("../skills/deep_research_worker.yaml"),
        ),
        (
            "deep-research-verify",
            include_str!("../skills/deep_research_verify.yaml"),
        ),
        ("mur-dev", include_str!("../skills/mur_dev.yaml")),
    ];

    let mur_skills_dir = mur_root.join("skills");

    // Clean up deprecated/renamed skills
    let deprecated_skills = ["mur-workflow", "mur"];
    let tool_dirs: &[&str] = &[".claude", ".augment", ".agents"];
    for old_name in &deprecated_skills {
        let old_canonical = mur_skills_dir.join(old_name);
        if old_canonical.exists() {
            let _ = std::fs::remove_dir_all(&old_canonical);
        }
        for tool_dir_name in tool_dirs {
            let old_link = home.join(tool_dir_name).join("skills").join(old_name);
            if old_link.exists() || old_link.symlink_metadata().is_ok() {
                let _ = std::fs::remove_file(&old_link);
                let _ = std::fs::remove_dir_all(&old_link);
            }
        }
    }

    // Write canonical YAML to ~/.mur/skills/<name>/skill.yaml
    // and render markdown to ~/.mur/skills/<name>/SKILL.md for AI tool compat.
    let mut shadowed: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for (name, content) in skills {
        let dir = mur_skills_dir.join(name);
        if dev_skill_shadowed_by_user(&dir, name) {
            tracing::info!(
                skill = name,
                "skipping builtin install: user-authored skill of the same name exists (never-shadow)"
            );
            shadowed.insert(name);
            continue;
        }
        std::fs::create_dir_all(&dir)?;
        // Canonical YAML — consumed by M2 SkillLoader
        std::fs::write(dir.join("skill.yaml"), content)?;
        // Markdown rendering — consumed by existing AI tool hooks
        let md =
            mur_common::skill::yaml_to_markdown(content).unwrap_or_else(|_| content.to_string());
        std::fs::write(dir.join("SKILL.md"), md)?;
    }

    // Tool dirs to symlink into
    let tool_dirs: &[&str] = &[".claude", ".augment", ".agents"];

    for tool_dir_name in tool_dirs {
        let tool_base = home.join(tool_dir_name);
        if !tool_base.exists() && *tool_dir_name != ".agents" {
            continue;
        }
        let tool_skills = tool_base.join("skills");
        std::fs::create_dir_all(&tool_skills)?;

        for (name, _) in skills {
            if shadowed.contains(name) {
                continue;
            }
            let canonical = mur_skills_dir.join(name);
            let link = tool_skills.join(name);
            symlink_skill_dir(&canonical, &link)?;
        }
    }

    Ok(true)
}

/// Create a symlink from `link` -> `target`. If `link` exists as a regular
/// directory, remove it first. If it's already a correct symlink, skip.
fn symlink_skill_dir(target: &std::path::Path, link: &std::path::Path) -> Result<()> {
    if link.exists() || link.symlink_metadata().is_ok() {
        // Check if it's already a correct symlink
        if let Ok(existing) = std::fs::read_link(link)
            && existing == target
        {
            return Ok(());
        }
        // Remove old dir or wrong symlink
        if link.is_dir()
            && !link
                .symlink_metadata()
                .map(|m| m.file_type().is_symlink())
                .unwrap_or(false)
        {
            std::fs::remove_dir_all(link)?;
        } else {
            std::fs::remove_file(link)?;
        }
    }

    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link)?;
    }

    #[cfg(not(unix))]
    {
        // Fallback: copy the directory contents
        std::fs::create_dir_all(link)?;
        for entry in std::fs::read_dir(target)? {
            let entry = entry?;
            let dest = link.join(entry.file_name());
            std::fs::copy(entry.path(), dest)?;
        }
    }

    Ok(())
}

/// Push unsynced workflows to the cloud server.
/// Uses `.synced` marker files in `~/.mur/workflows/` to track which have been pushed.
async fn push_unsynced_workflows(server_url: &str, token: &str, quiet: bool) -> Result<()> {
    let mur_dir = dirs::home_dir()
        .ok_or_else(|| anyhow::anyhow!("no home dir"))?
        .join(".mur");
    let workflows_dir = mur_dir.join("workflows");
    if !workflows_dir.exists() {
        return Ok(());
    }

    let device_id = crate::auth::get_device_id();
    let device_name = crate::auth::get_device_name();
    let device_os = crate::auth::get_device_os();
    let url = format!("{}/api/v1/workflows", server_url);

    let mut pushed = 0usize;
    for entry in std::fs::read_dir(&workflows_dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("yaml") {
            continue;
        }
        let name = path
            .file_stem()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();

        // Check .synced marker — skip if content hasn't changed
        let synced_path = workflows_dir.join(format!("{}.synced", name));
        let content = std::fs::read_to_string(&path)?;
        let content_hash = format!("{:x}", md5_simple(&content));
        if synced_path.exists()
            && let Ok(prev_hash) = std::fs::read_to_string(&synced_path)
            && prev_hash.trim() == content_hash
        {
            continue;
        }

        // POST workflow YAML to server
        let payload = serde_json::json!({
            "name": name,
            "yaml_content": content,
        });
        let body = serde_json::to_string(&payload)?;

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .timeout(std::time::Duration::from_secs(15))
            .header("Authorization", format!("Bearer {}", token))
            .header("X-Device-ID", &device_id)
            .header("X-Device-Name", &device_name)
            .header("X-Device-OS", &device_os)
            .header("Content-Type", "application/json")
            .body(body)
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                // Write content hash as synced marker
                if let Err(e) = std::fs::write(&synced_path, &content_hash) {
                    tracing::warn!("Failed to write synced marker: {e}");
                }
                pushed += 1;
            }
            Ok(r) => {
                if !quiet {
                    eprintln!("  ⚠ Workflow push failed for {}: HTTP {}", name, r.status());
                }
            }
            Err(e) => {
                if !quiet {
                    eprintln!("  ⚠ Workflow push failed for {}: {}", name, e);
                }
            }
        }
    }

    if !quiet && pushed > 0 {
        eprintln!("  ☁ Pushed {} workflow(s) to cloud.", pushed);
    }

    Ok(())
}

// ─── Phase-1 memory-sync CLI helpers ─────────────────────────────────────────

/// Execute `mur push [--dry-run]`.
pub(crate) async fn run_push(server_url: &str, dry_run: bool) -> anyhow::Result<()> {
    let outbox = crate::sync::Outbox::default_location()?;
    let pending_paths = outbox.list_pending()?;

    if pending_paths.is_empty() {
        println!("outbox empty, nothing to push");
        return Ok(());
    }

    // Parse all pending signals; collect (path, signal) pairs, drop bad YAML with warning.
    let mut to_send: Vec<(std::path::PathBuf, mur_common::Signal)> =
        Vec::with_capacity(pending_paths.len());
    for p in pending_paths {
        let yaml = match std::fs::read_to_string(&p) {
            Ok(s) => s,
            Err(e) => {
                eprintln!("  skip unreadable {}: {e}", p.display());
                continue;
            }
        };
        match serde_yaml::from_str::<mur_common::Signal>(&yaml) {
            Ok(sig) => to_send.push((p, sig)),
            Err(e) => eprintln!("  skip bad YAML {}: {e}", p.display()),
        }
    }

    if dry_run {
        println!("[dry-run] would push {} signal(s)", to_send.len());
        for (p, s) in &to_send {
            println!("  - {} ({})", s.id, p.display());
        }
        return Ok(());
    }

    let tokens = crate::auth::load_tokens()
        .ok_or_else(|| anyhow::anyhow!("not logged in (run `mur auth login`)"))?;
    let client = crate::sync::SyncClient::new(server_url, &tokens.access_token)?;

    let signals: Vec<mur_common::Signal> = to_send.iter().map(|(_, s)| s.clone()).collect();
    let resp = client.push_batch(&signals).await.context("push_batch")?;

    // Move accepted signal files to .flushed/.
    for (path, sig) in &to_send {
        if resp.accepted.iter().any(|a| *a == sig.id.to_string()) {
            outbox.mark_flushed(path)?;
        }
    }

    println!(
        "pushed: {} accepted, {} rejected",
        resp.accepted.len(),
        resp.rejected.len()
    );
    for r in &resp.rejected {
        println!("  rejected {}: {}", r.id, r.reason);
    }
    Ok(())
}

/// Execute `mur fetch [--dry-run]`.
pub(crate) async fn run_fetch(server_url: &str, dry_run: bool) -> anyhow::Result<()> {
    let cursor_store = crate::sync::CursorStore::default_location()?;
    let cursor = cursor_store.load()?;

    if dry_run {
        println!(
            "[dry-run] would fetch since={:?}",
            cursor.last_signal_id.as_deref()
        );
        return Ok(());
    }

    let tokens = crate::auth::load_tokens()
        .ok_or_else(|| anyhow::anyhow!("not logged in (run `mur auth login`)"))?;
    let client = crate::sync::SyncClient::new(server_url, &tokens.access_token)?;

    let resp = client
        .fetch_pending(cursor.last_signal_id.as_deref())
        .await
        .context("fetch_pending")?;

    let inbox = crate::sync::Inbox::default_location()?;
    let mut ids_to_ack: Vec<String> = Vec::with_capacity(resp.signals.len());
    for s in &resp.signals {
        inbox.receive(s)?;
        ids_to_ack.push(s.id.to_string());
    }

    let store = YamlStore::default_store()?;
    let report = inbox.apply_all(&store)?;

    println!(
        "fetched: {} signal(s) — applied {}, skipped {}, errors {}",
        resp.signals.len(),
        report.applied,
        report.skipped,
        report.errors.len()
    );
    for e in &report.errors {
        eprintln!("  error: {e}");
    }

    if !ids_to_ack.is_empty() {
        client.ack(&ids_to_ack).await.context("ack")?;
    }

    cursor_store.save(&crate::sync::FetchCursor {
        last_signal_id: resp.next_cursor,
        last_fetched_at: Some(chrono::Utc::now()),
    })?;
    Ok(())
}

/// Execute `mur sync status`.
pub(crate) fn run_status() -> anyhow::Result<()> {
    let outbox = crate::sync::Outbox::default_location()?;
    let cursor_store = crate::sync::CursorStore::default_location()?;

    let outbox_pending = outbox.list_pending()?.len();

    // Count inbox pending files directly — Inbox doesn't expose a list API.
    let inbox_dir = dirs::home_dir()
        .map(|h| h.join(".mur/inbox"))
        .unwrap_or_default();
    let inbox_pending = if inbox_dir.exists() {
        std::fs::read_dir(&inbox_dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .filter(|e| {
                        e.path().extension().and_then(|s| s.to_str()) == Some("yaml")
                            && !e
                                .path()
                                .file_name()
                                .and_then(|s| s.to_str())
                                .is_some_and(|n| n.starts_with('.'))
                    })
                    .count()
            })
            .unwrap_or(0)
    } else {
        0
    };

    let cursor = cursor_store.load()?;

    println!("sync status");
    println!("  outbox pending: {outbox_pending}");
    println!("  inbox pending:  {inbox_pending}");
    match cursor.last_fetched_at {
        Some(t) => println!("  last fetch:     {t}"),
        None => println!("  last fetch:     never"),
    }
    Ok(())
}

/// Build a richer query for project-aware sync by detecting language and git context.
pub(crate) fn build_project_sync_query(cwd: &std::path::Path, project_name: &str) -> String {
    let mut parts = vec![project_name.to_string()];

    // Detect language
    if let Some(lang) = capture::starter::detect_language_name(cwd) {
        parts.push(lang);
    }

    // Try git remote for extra context
    if let Ok(output) = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(cwd)
        .output()
        && output.status.success()
    {
        let remote = String::from_utf8_lossy(&output.stdout).trim().to_string();
        if let Some(name) = remote.rsplit('/').next() {
            let name = name.trim_end_matches(".git");
            if name != project_name {
                parts.push(name.to_string());
            }
        }
    }

    parts.join(" ")
}

#[cfg(test)]
mod sync_status_tests {
    use super::*;

    #[test]
    fn run_status_works_on_fresh_system() {
        // run_status uses default_location() which creates dirs under $HOME/.mur/.
        // We just verify it doesn't panic and returns Ok.
        run_status().unwrap();
    }
}

#[cfg(test)]
mod sync_skill_tests {

    #[test]
    fn installs_project_search_skill() {
        let home = std::env::temp_dir().join(format!(
            "mur-skilltest-{}-{}",
            std::process::id(),
            std::fs::read_dir(std::env::temp_dir())
                .map(|d| d.count())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&home).unwrap();

        super::ensure_mur_skill(&home, &home.join(".mur")).unwrap();

        let skill_yaml = home
            .join(".mur")
            .join("skills")
            .join("mur-project-search")
            .join("skill.yaml");
        assert!(
            skill_yaml.exists(),
            "mur-project-search skill.yaml must be written"
        );
        let body = std::fs::read_to_string(&skill_yaml).unwrap();
        assert!(body.contains("name: mur-project-search"));

        std::fs::remove_dir_all(&home).ok();
    }

    #[test]
    fn ensure_mur_skill_ships_mur_native_tools() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path().join("home");
        let root = tmp.path().join("root");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&root).unwrap();

        super::ensure_mur_skill(&home, &root).unwrap();

        let path = root.join("skills/mur-native-tools/skill.yaml");
        assert!(
            path.exists(),
            "mur-native-tools must be written to the global store by ensure_mur_skill"
        );
        let raw = std::fs::read_to_string(&path).unwrap();
        let m = mur_common::skill::parse_canonical(&raw).unwrap();
        assert_eq!(m.name, "mur-native-tools");
    }
}

#[cfg(test)]
mod builtin_skill_tests {
    #[test]
    fn new_builtin_skills_parse_and_respect_disclosure_budgets() {
        // (name, yaml, expect_on_demand)
        let cases: &[(&str, &str, bool)] = &[
            (
                "mur-fleet-manage",
                include_str!("../skills/mur_fleet_manage.yaml"),
                false,
            ),
            (
                "mur-fleet-loop",
                include_str!("../skills/mur_fleet_loop.yaml"),
                true,
            ),
            (
                "mur-fleet-share",
                include_str!("../skills/mur_fleet_share.yaml"),
                true,
            ),
            (
                "mur-workflow-author",
                include_str!("../skills/mur_workflow_author.yaml"),
                false,
            ),
            (
                "mur-workflow-hitl",
                include_str!("../skills/mur_workflow_hitl.yaml"),
                true,
            ),
            (
                "mur-workflow-delegate",
                include_str!("../skills/mur_workflow_delegate.yaml"),
                true,
            ),
            (
                "mur-agent-setup",
                include_str!("../skills/mur_agent_setup.yaml"),
                false,
            ),
            (
                "mur-agent-mcp-wire",
                include_str!("../skills/mur_agent_mcp_wire.yaml"),
                true,
            ),
            (
                "mur-agent-schedule",
                include_str!("../skills/mur_agent_schedule.yaml"),
                true,
            ),
            (
                "mur-parallel-exec",
                include_str!("../skills/mur_parallel_exec.yaml"),
                false,
            ),
            (
                "mur-parallel-tracks",
                include_str!("../skills/mur_parallel_tracks.yaml"),
                true,
            ),
            (
                "mur-parallel-merge",
                include_str!("../skills/mur_parallel_merge.yaml"),
                true,
            ),
            (
                "parallel-topology-guide",
                include_str!("../skills/parallel_topology_guide.yaml"),
                true,
            ),
            (
                "parallel-decompose",
                include_str!("../skills/parallel_decompose.yaml"),
                false,
            ),
            (
                "parallel-code",
                include_str!("../skills/parallel_code.yaml"),
                false,
            ),
            (
                "mur-native-tools",
                include_str!("../skills/mur_native_tools.yaml"),
                false,
            ),
            ("mur-dev", include_str!("../skills/mur_dev.yaml"), false),
        ];
        use mur_common::skill::manifest::Visibility;
        for (name, yaml, on_demand) in cases {
            let m = mur_common::skill::parse_canonical(yaml)
                .unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
            assert_eq!(&m.name, name);
            assert_eq!(
                m.visibility == Visibility::OnDemand,
                *on_demand,
                "{name}: wrong visibility"
            );
            assert!(
                m.description.chars().count() <= 120,
                "{name}: description over 120 chars"
            );
            assert!(
                m.content.r#abstract.split_whitespace().count() <= 50,
                "{name}: abstract over 50 words"
            );
            let body = m
                .content
                .context
                .clone()
                .or_else(|| m.content.note.clone())
                .unwrap_or_default();
            let body_lines = body.lines().count();
            assert!(
                body_lines <= 150,
                "{name}: body {body_lines} lines (budget 150)"
            );
        }
    }
}

#[cfg(test)]
mod dev_skill_trigger_tests {
    /// Every dev-discipline keyword trigger must be a valid regex — the
    /// runtime trigger matcher compiles them with `regex::Regex::new`.
    #[test]
    fn dev_skill_keyword_triggers_compile() {
        let yamls: &[&str] = &[include_str!("../skills/mur_dev.yaml")];
        for y in yamls {
            let m = mur_common::skill::parse_canonical(y).expect("parse");
            for t in &m.triggers {
                if let Some(p) = &t.pattern {
                    regex::Regex::new(p).unwrap_or_else(|e| {
                        panic!("{}: trigger regex fails to compile: {e}", m.name)
                    });
                }
            }
        }
    }
}

#[cfg(test)]
mod deep_research_skill_tests {
    /// Loader test (Task 9): the three deep-research fleet skills must parse
    /// into `SkillManifest`, be scoped to the `deep-research` fleet (not the
    /// default `User` scope), and carry a non-empty procedure — mirrors
    /// `builtin_skill_tests::new_builtin_skills_parse_and_respect_disclosure_budgets`
    /// above, adapted for `scope: fleet` + procedure-mode content instead of
    /// the CLI-tool-hint disclosure budgets those skills use.
    #[test]
    fn deep_research_skills_parse_scope_fleet_with_nonempty_procedure() {
        let cases: &[(&str, &str)] = &[
            (
                "deep-research-router",
                include_str!("../skills/deep_research_router.yaml"),
            ),
            (
                "deep-research-worker",
                include_str!("../skills/deep_research_worker.yaml"),
            ),
            (
                "deep-research-verify",
                include_str!("../skills/deep_research_verify.yaml"),
            ),
        ];
        use mur_common::skill::manifest::SkillScope;
        for (name, yaml) in cases {
            let m = mur_common::skill::parse_canonical(yaml)
                .unwrap_or_else(|e| panic!("{name}: parse failed: {e}"));
            assert_eq!(&m.name, name);
            assert_eq!(m.scope, SkillScope::Fleet, "{name}: scope must be Fleet");
            assert_eq!(
                m.fleet.as_deref(),
                Some("deep-research"),
                "{name}: fleet selector must be deep-research"
            );
            let steps = &m
                .content
                .procedure
                .as_ref()
                .unwrap_or_else(|| panic!("{name}: content.procedure must be Some"))
                .steps;
            assert!(
                !steps.is_empty(),
                "{name}: procedure steps must be non-empty"
            );
            for step in steps {
                assert!(
                    !step.description.trim().is_empty(),
                    "{name}: every step needs a non-empty description"
                );
            }
        }

        // The retired escalation-ladder skill must not be recreated under
        // either its old aura-* name or a deep-research-* rename.
        for (name, yaml) in cases {
            assert!(
                !yaml.to_lowercase().contains("aura-"),
                "{name}: must not reference the retired aura-* skills"
            );
        }
    }

    #[test]
    fn deep_research_router_emits_own_line_convergence_marker() {
        let yaml = include_str!("../skills/deep_research_router.yaml");
        let m = mur_common::skill::parse_canonical(yaml).unwrap();
        let steps = m.content.procedure.unwrap().steps;
        let body: String = steps
            .iter()
            .map(|s| s.description.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            body.lines()
                .any(|l| l.trim() == "RESEARCH_COMPLETE" || l.trim() == "`RESEARCH_COMPLETE`"),
            "router skill must instruct emitting RESEARCH_COMPLETE alone on its own line"
        );
    }
}

#[cfg(test)]
mod never_shadow_tests {
    /// A user-authored skill occupying a dev-discipline name must survive
    /// `ensure_mur_skill` untouched (spec 2026-07-23 §6 never-shadow).
    #[test]
    fn user_skill_with_dev_name_is_not_overwritten() {
        let home = tempfile::tempdir().unwrap();
        let mur_root = home.path().join(".mur");
        let dir = mur_root.join("skills").join("mur-tdd");
        std::fs::create_dir_all(&dir).unwrap();
        let user_yaml = "name: mur-tdd\nversion: 0.0.1\npublisher: human:alice\n\
                         description: my own tdd notes\ncategory: workflow\n\
                         content:\n  abstract: mine\n  context: keep me\n";
        std::fs::write(dir.join("skill.yaml"), user_yaml).unwrap();

        super::ensure_mur_skill(home.path(), &mur_root).unwrap();

        let after = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();
        assert_eq!(after, user_yaml, "user-authored skill must not be clobbered");
    }

    /// Unparseable existing YAML is treated as user-authored (fail-safe skip).
    #[test]
    fn unparseable_existing_dev_skill_is_skipped() {
        let home = tempfile::tempdir().unwrap();
        let mur_root = home.path().join(".mur");
        let dir = mur_root.join("skills").join("mur-tdd");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("skill.yaml"), ": not yaml {{{{").unwrap();

        super::ensure_mur_skill(home.path(), &mur_root).unwrap();

        let after = std::fs::read_to_string(dir.join("skill.yaml")).unwrap();
        assert_eq!(after, ": not yaml {{{{");
    }

    #[test]
    fn shadow_predicate_publisher_rules() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("skill.yaml");
        // Foreign publisher → shadowed (skip).
        std::fs::write(&f, "name: mur-tdd\nversion: 0.0.1\npublisher: human:alice\ndescription: d\ncategory: workflow\ncontent:\n  abstract: a\n  context: c\n").unwrap();
        assert!(super::dev_skill_shadowed_by_user(dir.path(), "mur-tdd"));
        // MUR publisher → not shadowed (update as usual).
        std::fs::write(&f, "name: mur-tdd\nversion: 0.0.1\npublisher: human:mur-official\ndescription: d\ncategory: workflow\ncontent:\n  abstract: a\n  context: c\n").unwrap();
        assert!(!super::dev_skill_shadowed_by_user(dir.path(), "mur-tdd"));
        // Non-dev names never shadow (existing builtin semantics unchanged).
        assert!(!super::dev_skill_shadowed_by_user(dir.path(), "mur-run"));
        // No file on disk → nothing to shadow.
        std::fs::remove_file(&f).unwrap();
        assert!(!super::dev_skill_shadowed_by_user(dir.path(), "mur-tdd"));
    }
}
