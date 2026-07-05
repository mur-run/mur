//! Hub-side consent inbox for relay `install_request` events (Plan
//! `2026-07-04-relay-one-click-install.md` Task 3).
//!
//! Reads the append-only jsonl mur-core's `install_request` module
//! writes to `<mur_home>/hub/install-requests.jsonl`, filters out
//! anything already marked done in `install-requests.done`, and on
//! consent fetches the item from mur-server and routes it to the
//! existing type-specific installer. Fail-closed: deny (or an
//! unrecognized type) never installs anything, only marks the
//! request consumed so the Hub doesn't re-prompt after restart.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use mur_core::install_request::InstallRequestRecord;

/// Watches `<mur_home>/hub/` for changes to `install-requests.jsonl`
/// (and its `.done` sibling) and invokes `on_change` whenever either
/// file is written — same live-tail shape as `mur_channel::watch::watch_channels`,
/// used by the Hub UI to refresh the consent-modal inbox without polling.
pub fn watch_install_requests(
    mur_home: &Path,
    on_change: impl Fn() + Send + 'static,
) -> Result<notify::RecommendedWatcher> {
    let root = mur_home.join("hub");
    std::fs::create_dir_all(&root).with_context(|| format!("create {}", root.display()))?;
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if res.is_ok() {
            on_change();
        }
    })
    .context("create install-requests watcher")?;
    notify::Watcher::watch(&mut watcher, &root, notify::RecursiveMode::NonRecursive)
        .context("start install-requests watch")?;
    Ok(watcher)
}

/// One pending install request as shown in the Hub consent modal.
#[derive(Debug, Clone, Serialize)]
pub struct InstallRequestView {
    pub install_type: String,
    pub id: String,
    pub publisher: String,
    pub request_id: String,
    pub requested_at: u64,
    /// True when `publisher == "mur-official"` — shown as a badge.
    pub is_official: bool,
}

const OFFICIAL_PUBLISHER: &str = "mur-official";

fn done_path(mur_home: &Path) -> PathBuf {
    mur_home.join("hub").join("install-requests.done")
}

fn requests_path(mur_home: &Path) -> PathBuf {
    mur_home.join("hub").join("install-requests.jsonl")
}

/// True if `request_id` already appears in the done-file.
pub fn is_install_request_done(mur_home: &Path, request_id: &str) -> Result<bool> {
    let path = done_path(mur_home);
    if !path.exists() {
        return Ok(false);
    }
    let file = std::fs::File::open(&path)?;
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim() == request_id {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Appends `request_id` to `<mur_home>/hub/install-requests.done`
/// (idempotent — a repeat call is a harmless duplicate line, checked
/// via [`is_install_request_done`] before re-prompting).
pub fn mark_install_request_done(mur_home: &Path, request_id: &str) -> Result<()> {
    let path = done_path(mur_home);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if is_install_request_done(mur_home, request_id)? {
        return Ok(());
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{request_id}")?;
    Ok(())
}

fn parse_publisher(id: &str) -> String {
    id.split('/').next().unwrap_or(id).to_string()
}

/// List pending install requests (jsonl minus done-file), for the
/// Hub consent modal.
#[tauri::command]
pub fn install_inbox_list() -> Result<Vec<InstallRequestView>, String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| format!("{e:#}"))?;
    list_pending(&home).map_err(|e| format!("{e:#}"))
}

fn list_pending(mur_home: &Path) -> Result<Vec<InstallRequestView>> {
    let path = requests_path(mur_home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let file = std::fs::File::open(&path)?;
    let mut out = Vec::new();
    for line in std::io::BufReader::new(file).lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(rec) = serde_json::from_str::<InstallRequestRecord>(&line) else {
            continue;
        };
        if is_install_request_done(mur_home, &rec.request_id)? {
            continue;
        }
        let publisher = parse_publisher(&rec.id);
        out.push(InstallRequestView {
            is_official: publisher == OFFICIAL_PUBLISHER,
            install_type: rec.install_type,
            id: rec.id,
            publisher,
            request_id: rec.request_id,
            requested_at: rec.requested_at,
        });
    }
    Ok(out)
}

/// Writes a new workflow file at `<mur_home>/workflows/<name>.yaml`.
///
/// Refuses (returns `Err`) if `yaml` doesn't parse as a valid
/// [`mur_common::workflow::Workflow`], or if the target file already
/// exists — no overwrite, no flag to force one.
pub fn install_workflow(mur_home: &Path, yaml: &str) -> Result<PathBuf> {
    let workflow: mur_common::workflow::Workflow =
        serde_yaml_ng::from_str(yaml).context("invalid workflow yaml")?;
    let dir = mur_home.join("workflows");
    std::fs::create_dir_all(&dir)?;
    let path = dir.join(format!("{}.yaml", workflow.base.name));
    if path.exists() {
        bail!(
            "workflow {:?} already exists at {}; refusing to overwrite",
            workflow.base.name,
            path.display()
        );
    }
    std::fs::write(&path, yaml)?;
    Ok(path)
}

/// Fetch item content from mur-server's registry download endpoint
/// using the configured `mobile_relay.api_key`.
async fn fetch_item(mur_home: &Path, install_type: &str, id: &str) -> Result<String> {
    let cfg = mur_common::config::Config::load_or_default(&mur_home.join("config.yaml"));
    let api_key = cfg
        .mobile_relay
        .api_key
        .filter(|k| !k.trim().is_empty())
        .context("mobile_relay.api_key not configured; run `mur agent mcp login` or set it in config.yaml")?;
    let base =
        std::env::var("MUR_REGISTRY_URL").unwrap_or_else(|_| "https://app.mur.run".to_string());
    let url = format!("{base}/api/v1/registry/{install_type}/{id}/download");
    let resp = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&api_key)
        .send()
        .await
        .context("fetching install-request item from mur-server")?;
    if !resp.status().is_success() {
        bail!("mur-server download failed: HTTP {}", resp.status());
    }
    resp.text().await.context("reading download response body")
}

/// Approve or deny a pending install request.
///
/// On approve: fetches the item, routes to the type-specific
/// installer (skill/mcp/plugin/workflow), then marks the request
/// done. On deny: marks it done without fetching or installing
/// anything. Unknown `install_type` is always an error, on either
/// path — dispatch is a straightforward match, not a plugin
/// interface.
#[tauri::command]
pub async fn install_inbox_consent(request_id: String, approve: bool) -> Result<(), String> {
    let home = mur_core::cmd::agent::resolve_mur_home().map_err(|e| format!("{e:#}"))?;
    let pending = list_pending(&home).map_err(|e| format!("{e:#}"))?;
    let Some(req) = pending.into_iter().find(|r| r.request_id == request_id) else {
        return Err(format!("no pending install request with id {request_id}"));
    };

    if approve {
        route_install(&home, &req)
            .await
            .map_err(|e| format!("{e:#}"))?;
    }

    mark_install_request_done(&home, &request_id).map_err(|e| format!("{e:#}"))
}

/// Route an approved request to its type-specific installer. Direct
/// match on the type string — deliberately not a trait/interface,
/// per repo convention of preferring simple dispatch.
async fn route_install(mur_home: &Path, req: &InstallRequestView) -> Result<()> {
    match req.install_type.as_str() {
        "skill" => {
            // Reuse the registry-install engine used by `cmd/agent/skill.rs`'s
            // CLI path (stamps origin). The Hub has no single "current agent"
            // concept for inbox installs, so we install onto the concierge
            // ("mur") agent — matching the Hub's existing default elsewhere.
            mur_core::cmd::agent::skill_registry_add::cmd_skill_registry_add(
                "mur", &req.id, None, true,
            )
            .await
            .map(|_| ())
            .context("skill registry install failed")
        }
        "mcp" => {
            let yaml = fetch_item(mur_home, "mcp", &req.id).await?;
            let entry: McpRemoteEntry =
                serde_yaml_ng::from_str(&yaml).context("invalid mcp registry entry")?;
            mur_core::cmd::agent::mcp::cmd_mcp_add_remote(
                "mur",
                &entry.name,
                &entry.url,
                None,
                None,
                entry.host.as_deref(),
            )
            .context("mcp add-remote failed")
        }
        "plugin" => {
            // Hub plugin import expects a local directory path; the fetched
            // item is that path (mur-server stages it locally before
            // returning). See `agent_addon_import` in `mcp_skills.rs`.
            let plugin_dir = fetch_item(mur_home, "plugin", &req.id).await?;
            mur_core::cmd::agent::addon::cmd_addon_import("mur", plugin_dir.trim(), None, true)
                .context("plugin import failed")
        }
        "workflow" => {
            let yaml = fetch_item(mur_home, "workflow", &req.id).await?;
            install_workflow(mur_home, &yaml).map(|_| ())
        }
        other => bail!("install_inbox: unsupported install type {other:?}"),
    }
}

/// Minimal shape of a registry MCP entry download — just enough to
/// route into `cmd_mcp_add_remote`.
#[derive(Debug, Deserialize)]
struct McpRemoteEntry {
    name: String,
    url: String,
    #[serde(default)]
    host: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_workflow_yaml(name: &str) -> String {
        format!(
            r#"
name: {name}
description: a test workflow
content:
  technical: "do the thing"
  principle: "why it matters"
steps: []
variables: []
"#
        )
    }

    #[test]
    fn install_workflow_writes_valid_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let yaml = sample_workflow_yaml("my-workflow");
        let path = install_workflow(home, &yaml).expect("valid workflow should install");
        assert!(path.exists());
        assert_eq!(path, home.join("workflows").join("my-workflow.yaml"));
    }

    #[test]
    fn install_workflow_rejects_invalid_yaml() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let err = install_workflow(home, "not: a valid workflow\n- broken").unwrap_err();
        assert!(format!("{err:#}").contains("invalid workflow yaml"));
    }

    #[test]
    fn install_workflow_refuses_overwrite() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let yaml = sample_workflow_yaml("dup-workflow");
        install_workflow(home, &yaml).expect("first install should succeed");
        let err = install_workflow(home, &yaml).unwrap_err();
        assert!(format!("{err:#}").contains("already exists"));
    }

    #[tokio::test]
    async fn route_install_rejects_unknown_type() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let req = InstallRequestView {
            install_type: "exe".to_string(),
            id: "pub/name".to_string(),
            publisher: "pub".to_string(),
            request_id: "req-1".to_string(),
            requested_at: 0,
            is_official: false,
        };
        let err = route_install(home, &req).await.unwrap_err();
        assert!(format!("{err:#}").contains("unsupported install type"));
    }

    #[test]
    fn mark_and_check_done_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        assert!(!is_install_request_done(home, "req-1").unwrap());
        mark_install_request_done(home, "req-1").unwrap();
        assert!(is_install_request_done(home, "req-1").unwrap());
        // idempotent
        mark_install_request_done(home, "req-1").unwrap();
        let contents = std::fs::read_to_string(done_path(home)).unwrap();
        assert_eq!(contents.lines().count(), 1);
    }
}
