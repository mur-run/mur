use super::*;

/// Detect the default branch name (main or master).
pub(super) fn detect_git_branch(dir: &std::path::Path) -> String {
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

/// Mark the append-only open-items log as union-merged, idempotently.
///
/// Two machines both append at end-of-file, which is the one shape an ordinary
/// three-way merge cannot resolve — every sync would stop on a conflict in a
/// file no human ever edits. Union merge keeps both sides' lines; `open()`
/// folds them by id in timestamp order, so the duplicates and the interleaving
/// come out the same on both machines.
///
/// Written before the pull, not just the push: git reads the merge driver from
/// the working tree as it merges, so a rule that only arrives *inside* the
/// commit being pulled is too late for that very pull.
///
/// ponytail: if a machine already tracks a `.gitattributes` without this rule,
/// the write dirties the tree and that one pull warns instead of rebasing. The
/// following push commits the rule and every sync after it is clean, so this
/// costs one warning once rather than a lock file and a state machine.
pub(super) fn ensure_union_merge(mur_dir: &std::path::Path) -> std::io::Result<()> {
    let path = mur_dir.join(".gitattributes");
    let rule = format!("{} merge=union", mur_open_items::LOG_FILE);
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing.lines().any(|l| l.trim() == rule) {
        return Ok(());
    }
    let mut body = existing;
    if !body.is_empty() && !body.ends_with('\n') {
        body.push('\n');
    }
    body.push_str(&rule);
    body.push('\n');
    std::fs::write(&path, body)
}

pub(super) fn run_git_in(dir: &std::path::Path, args: &[&str]) -> Result<String> {
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

pub(super) fn apply_cloud_pull_v2(
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
pub(super) fn build_sync_changes(
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
pub(super) fn update_manifest_after_push(
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
pub(super) async fn sync_pull_once(
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
pub(super) fn md5_simple(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

/// Push unsynced workflows to the cloud server.
/// Uses `.synced` marker files in `~/.mur/workflows/` to track which have been pushed.
pub(super) async fn push_unsynced_workflows(
    server_url: &str,
    token: &str,
    quiet: bool,
) -> Result<()> {
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
