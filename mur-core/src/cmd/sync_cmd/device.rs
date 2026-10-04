use super::*;

/// Run device sync (cloud API or git pull/commit/push) based on config.
/// Returns Ok(()) on success, warns on failure but doesn't block.
pub(super) fn resolve_team_id(
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
            let mur_dir = mur_common::home::mur_home_or_err()?;
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
                                let mut rejected: Vec<String> = Vec::new();
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
                                    // Parse before writing. The name and path
                                    // checks above stop a malicious filename;
                                    // nothing stopped content that cannot
                                    // load. An unparseable workflow written
                                    // here is rewritten on every sync and
                                    // warns forever from the store, naming a
                                    // file the user cannot fix and did not
                                    // create (#803).
                                    if let Err(e) =
                                        serde_yaml_ng::from_str::<mur_common::workflow::Workflow>(
                                            yaml_content,
                                        )
                                    {
                                        rejected.push(format!("{safe_name} ({e})"));
                                        continue;
                                    }
                                    std::fs::write(&path, yaml_content)?;
                                    pulled += 1;
                                }
                                if !quiet && pulled > 0 {
                                    eprintln!("  ✓ Pulled {} workflow(s) from server.", pulled);
                                }
                                // Report rather than write. Silence here would
                                // reproduce the old failure in a new place: the
                                // workflow would simply be missing, with no
                                // more explanation than before.
                                if !rejected.is_empty() {
                                    eprintln!(
                                        "  ⚠ Skipped {} workflow(s) from the server that do not parse:",
                                        rejected.len(),
                                    );
                                    for r in &rejected {
                                        eprintln!("      {r}");
                                    }
                                    eprintln!(
                                        "    They were not written locally. Fix or remove them server-side \
                                         (`mur workflow delete <name>`)."
                                    );
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
            let mur_dir = mur_common::home::mur_home_or_err()?;

            // Initialize git repo in ~/.mur if needed
            if !mur_dir.join(".git").exists() {
                run_git_in(&mur_dir, &["init"])?;
                run_git_in(&mur_dir, &["remote", "add", "origin", remote])?;
            }

            // Both directions need this in place, and pull needs it most: the
            // open-items log is append-only and both machines append at EOF,
            // so an ordinary three-way merge conflicts on every single sync.
            // `merge=union` keeps both sides' lines instead, which is only safe
            // because the fold is upsert-by-id in timestamp order — duplicated
            // and interleaved lines reach the same answer either way.
            if let Err(e) = ensure_union_merge(&mur_dir)
                && !quiet
            {
                eprintln!("  ⚠ could not write .gitattributes: {e}");
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
                    let _ = run_git_in(
                        &mur_dir,
                        &[
                            "add",
                            "skills/",
                            "workflows/",
                            "config.yaml",
                            ".gitattributes",
                            mur_open_items::LOG_FILE,
                        ],
                    );
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
