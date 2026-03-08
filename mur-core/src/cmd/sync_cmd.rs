use anyhow::Result;
use mur_common::pattern::*;

use crate::capture;
use crate::inject;
use crate::store::yaml::YamlStore;

/// Run device sync (cloud API or git pull/commit/push) based on config.
/// Returns Ok(()) on success, warns on failure but doesn't block.
pub(crate) fn device_sync(quiet: bool, direction: DeviceSyncDirection) -> Result<()> {
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
                        eprintln!("  ⚠ Not authenticated. Run `mur login` for cloud sync.");
                    }
                    return Ok(());
                }
            };

            match direction {
                DeviceSyncDirection::Pull => {
                    let url = format!("{}/api/sync/pull", server_url);
                    let device_id = crate::auth::get_device_id();
                    let device_name = crate::auth::get_device_name();
                    let device_os = crate::auth::get_device_os();
                    let output = std::process::Command::new("curl")
                        .args([
                            "-sf",
                            "--max-time",
                            "10",
                            "-H",
                            &format!("Authorization: Bearer {}", token),
                            "-H",
                            &format!("X-Device-ID: {}", device_id),
                            "-H",
                            &format!("X-Device-Name: {}", device_name),
                            "-H",
                            &format!("X-Device-OS: {}", device_os),
                            &url,
                        ])
                        .output();
                    match output {
                        Ok(o) if o.status.success() => {
                            let body = String::from_utf8_lossy(&o.stdout);
                            if body.trim() != "{}" && !body.trim().is_empty() {
                                apply_cloud_pull(&body, &mur_dir)?;
                                if !quiet {
                                    eprintln!("  ✓ Cloud pull complete.");
                                }
                            } else if !quiet {
                                eprintln!("  ✓ Already up to date.");
                            }
                        }
                        Ok(o) => {
                            if !quiet {
                                let stderr = String::from_utf8_lossy(&o.stderr);
                                eprintln!("  ⚠ Cloud pull failed: {}", stderr.trim());
                            }
                        }
                        Err(e) => {
                            if !quiet {
                                eprintln!("  ⚠ Cloud pull failed: {}", e);
                            }
                        }
                    }
                }
                DeviceSyncDirection::Push => {
                    let url = format!("{}/api/sync/push", server_url);
                    let patterns_dir = mur_dir.join("patterns");
                    let payload = build_cloud_push_payload(&patterns_dir)?;
                    let device_id = crate::auth::get_device_id();
                    let device_name = crate::auth::get_device_name();
                    let device_os = crate::auth::get_device_os();
                    let output = std::process::Command::new("curl")
                        .args([
                            "-sf",
                            "--max-time",
                            "15",
                            "-X",
                            "POST",
                            "-H",
                            &format!("Authorization: Bearer {}", token),
                            "-H",
                            &format!("X-Device-ID: {}", device_id),
                            "-H",
                            &format!("X-Device-Name: {}", device_name),
                            "-H",
                            &format!("X-Device-OS: {}", device_os),
                            "-H",
                            "Content-Type: application/json",
                            "-d",
                            &payload,
                            &url,
                        ])
                        .output();
                    match output {
                        Ok(o) if o.status.success() => {
                            if !quiet {
                                eprintln!("  ✓ Cloud push complete.");
                            }
                        }
                        Ok(o) => {
                            if !quiet {
                                let stderr = String::from_utf8_lossy(&o.stderr);
                                eprintln!("  ⚠ Cloud push failed: {}", stderr.trim());
                            }
                        }
                        Err(e) => {
                            if !quiet {
                                eprintln!("  ⚠ Cloud push failed: {}", e);
                            }
                        }
                    }
                }
                DeviceSyncDirection::Both => {
                    device_sync(quiet, DeviceSyncDirection::Pull)?;
                    device_sync(quiet, DeviceSyncDirection::Push)?;
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
                    let _ =
                        run_git_in(&mur_dir, &["add", "patterns/", "workflows/", "config.yaml"]);
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
                    device_sync(quiet, DeviceSyncDirection::Pull)?;
                    device_sync(quiet, DeviceSyncDirection::Push)?;
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
pub(crate) enum DeviceSyncDirection {
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

fn apply_cloud_pull(body: &str, mur_dir: &std::path::Path) -> Result<()> {
    // Parse JSON response containing pattern YAML content keyed by name
    let patterns: std::collections::HashMap<String, String> = serde_json::from_str(body)?;
    let patterns_dir = mur_dir.join("patterns");
    std::fs::create_dir_all(&patterns_dir)?;
    for (name, yaml_content) in &patterns {
        let safe_name = name.replace(['/', '\\', '.'], "_");
        let path = patterns_dir.join(format!("{}.yaml", safe_name));
        std::fs::write(&path, yaml_content)?;
    }
    Ok(())
}

fn build_cloud_push_payload(patterns_dir: &std::path::Path) -> Result<String> {
    use std::collections::HashMap;

    let mut map = HashMap::new();
    let sync_state_path = patterns_dir
        .parent()
        .unwrap_or(patterns_dir)
        .join(".sync_hashes.json");

    // Load previous sync hashes
    let prev_hashes: HashMap<String, String> = if sync_state_path.exists() {
        serde_json::from_str(&std::fs::read_to_string(&sync_state_path)?).unwrap_or_default()
    } else {
        HashMap::new()
    };

    let mut new_hashes = HashMap::new();

    if patterns_dir.exists() {
        for entry in std::fs::read_dir(patterns_dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                let name = path
                    .file_stem()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .to_string();
                let content = std::fs::read_to_string(&path)?;
                // Simple hash for change detection
                let hash = format!("{:x}", md5_simple(&content));
                new_hashes.insert(name.clone(), hash.clone());

                // Only include if changed since last sync
                if prev_hashes.get(&name).map(|h| h.as_str()) != Some(&hash) {
                    map.insert(name, content);
                }
            }
        }
    }

    // Save new hashes for next sync
    if let Ok(json) = serde_json::to_string(&new_hashes) {
        let _ = std::fs::write(&sync_state_path, json);
    }

    Ok(serde_json::to_string(&map)?)
}

/// Simple hash for change detection (not cryptographic).
fn md5_simple(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

pub(crate) async fn cmd_sync(quiet: bool, project_aware: bool) -> Result<()> {
    use crate::evolve::decay::apply_decay_all;
    use crate::evolve::maturity::apply_maturity_all;
    use crate::retrieve::scoring::score_and_rank;
    use inject::sync::{default_targets, generate_sync_content, write_sync_file};

    // ─── Heartbeat: register device activity ──────────────────
    crate::auth::heartbeat();

    // ─── Device sync first (cloud or git) ─────────────────────
    // Failures warn but don't block tool sync
    if let Err(e) = device_sync(quiet, DeviceSyncDirection::Both)
        && !quiet
    {
        eprintln!("  ⚠ Device sync error: {}", e);
    }

    let store = YamlStore::default_store()?;
    let now = chrono::Utc::now();

    // Run decay + maturity before syncing
    let _ = apply_decay_all(&store, now)?;
    let _ = apply_maturity_all(&store, now)?;

    let patterns = store.list_all()?;

    if patterns.is_empty() {
        if !quiet {
            println!("No patterns to sync.");
        }
        return Ok(());
    }

    // Get current working directory for project-scoped sync
    let cwd = std::env::current_dir()?;
    let targets = default_targets();

    // Build project-aware query when --project is set
    let project_name = cwd
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

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

        let mut scored = score_and_rank(&sync_query, patterns.clone());

        // When project-aware, additionally boost patterns whose applies.projects
        // or tags.languages match the current project
        if project_aware {
            let detected_lang = capture::starter::detect_language_name(&cwd);
            for sp in &mut scored {
                let p = &sp.pattern;
                // Boost patterns that explicitly list this project
                if p.applies
                    .projects
                    .iter()
                    .any(|proj| proj == &project_name || proj == "*")
                {
                    sp.score *= 1.3;
                }
                // Boost patterns matching detected language
                if let Some(ref lang) = detected_lang {
                    let lang_lower = lang.to_lowercase();
                    if p.tags
                        .languages
                        .iter()
                        .any(|l| l.to_lowercase() == lang_lower)
                        || p.applies
                            .languages
                            .iter()
                            .any(|l| l.to_lowercase() == lang_lower)
                    {
                        sp.score *= 1.2;
                    }
                }
            }
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
        }

        let top: Vec<Pattern> = scored
            .into_iter()
            .take(target.max_patterns)
            .map(|sp| sp.pattern)
            .collect();

        if top.is_empty() {
            continue;
        }

        let content = generate_sync_content(&top, &target.format);
        write_sync_file(&target_path, &content, &target.format)?;
        if !quiet {
            println!(
                "  {} — wrote {} patterns to {}",
                target.name,
                top.len(),
                target_path.display()
            );
        }
    }

    // ─── Ensure skills are installed ───────────────────────────
    let home = dirs::home_dir().expect("no home dir");
    let skill_installed = ensure_mur_skill(&home)?;
    if !quiet && skill_installed {
        println!("  🎓 MUR skill installed/updated for AI tools");
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
                    eprintln!("  ⚠ Reindex skipped: {} (run `mur reindex` manually or start Ollama)", e);
                }
            }
        }
    } else if !quiet {
        println!("  ✅ Index up to date");
    }

    if !quiet {
        println!("Sync complete.");
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
    let dirs_to_check = [
        mur_dir.join("patterns"),
        mur_dir.join("workflows"),
    ];

    for dir in &dirs_to_check {
        if !dir.exists() {
            continue;
        }
        if let Ok(entries) = std::fs::read_dir(dir) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.extension().and_then(|e| e.to_str()) == Some("yaml") {
                    if let Ok(meta) = std::fs::metadata(&path) {
                        if let Ok(mtime) = meta.modified() {
                            if mtime > index_mtime {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    false
}

/// Install/update the MUR skill for AI tools that support skills.
/// Writes canonical copies to ~/.mur/skills/ and symlinks from tool dirs.
/// Returns true if any skill was written.
pub(crate) fn ensure_mur_skill(home: &std::path::Path) -> Result<bool> {
    let skills: &[(&str, &str)] = &[
        ("mur", include_str!("../mur_skill.md")),
        ("mur-in", include_str!("../mur_in_skill.md")),
        ("mur-out", include_str!("../mur_out_skill.md")),
        ("mur-run", include_str!("../mur_workflow_skill.md")),
    ];

    let mur_skills_dir = home.join(".mur").join("skills");

    // Clean up deprecated/renamed skills
    let deprecated_skills = ["mur-workflow"];
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

    // Write canonical copies to ~/.mur/skills/<name>/SKILL.md
    for (name, content) in skills {
        let dir = mur_skills_dir.join(name);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("SKILL.md"), content)?;
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
