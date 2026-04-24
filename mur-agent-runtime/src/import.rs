//! Import — installs a .murpkg archive into `<mur_home>/agents/<name>/`.

use anyhow::{Context, Result, anyhow, bail};
use flate2::read::GzDecoder;
use mur_common::AgentProfile;
use std::fs::{self, File};
use std::path::{Path, PathBuf};
use tar::Archive;
use tempfile::TempDir;

#[derive(Debug, Clone, Default)]
pub struct ImportOptions {
    /// Override the agent name baked into the package (useful for avoiding
    /// collisions with an existing agent of the same name).
    pub rename: Option<String>,
}

#[derive(Debug)]
pub struct ImportReport {
    pub installed_name: String,
    pub missing_prerequisites: Vec<String>,
}

/// Unpack a .murpkg into `<mur_home>/agents/<name>/`, minting a fresh UUIDv7
/// for the new agent. Returns a report naming the installed agent and any
/// MCP command basenames that look unavailable on $PATH.
pub fn import_pkg(pkg_path: &Path, mur_home: &Path, opts: ImportOptions) -> Result<ImportReport> {
    // 1. Unpack into a scratch directory we control so a malformed archive
    //    can't clobber `<mur_home>/agents/*`.
    let scratch = TempDir::new().context("create scratch dir")?;
    {
        let file = File::open(pkg_path).with_context(|| format!("open {}", pkg_path.display()))?;
        let gz = GzDecoder::new(file);
        let mut archive = Archive::new(gz);
        archive
            .unpack(scratch.path())
            .with_context(|| format!("unpack {}", pkg_path.display()))?;
    }

    // 2. Validate the manifest and pull the packaged name.
    let manifest_path = scratch.path().join("manifest.yaml");
    if !manifest_path.exists() {
        bail!("archive is missing manifest.yaml");
    }
    let manifest_yaml = fs::read_to_string(&manifest_path).context("read manifest.yaml")?;
    let manifest: serde_yaml_ng::Value =
        serde_yaml_ng::from_str(&manifest_yaml).context("parse manifest.yaml")?;
    let schema = manifest["schema"]
        .as_str()
        .ok_or_else(|| anyhow!("manifest.schema is missing"))?;
    if schema != "mur-agent-package/1" {
        bail!("unsupported manifest schema '{schema}'");
    }

    // 3. Load the profile from the unpacked copy, rewrite its identity.
    let profile_path = scratch.path().join("profile.yaml");
    if !profile_path.exists() {
        bail!("archive is missing profile.yaml");
    }
    let mut profile: AgentProfile =
        serde_yaml_ng::from_str(&fs::read_to_string(&profile_path).context("read profile.yaml")?)
            .context("parse profile.yaml")?;
    let installed_name = opts.rename.unwrap_or_else(|| profile.name.clone());
    profile.name = installed_name.clone();
    profile.id = uuid::Uuid::now_v7().to_string();
    let now = chrono::Utc::now().to_rfc3339();
    profile.created_at = now.clone();
    profile.updated_at = now;

    // 4. Refuse clobbering an existing agent.
    let dest = mur_home.join("agents").join(&installed_name);
    if dest.exists() {
        bail!(
            "agent '{installed_name}' already exists at {}",
            dest.display()
        );
    }
    fs::create_dir_all(&dest).with_context(|| format!("create {}", dest.display()))?;

    // 5. Re-point the unix-socket bind path at the new home so a renamed or
    //    relocated agent doesn't accidentally stomp the exporter's socket.
    if profile.transport.socket.enabled && profile.transport.socket.bind.starts_with("unix://") {
        profile.transport.socket.bind = format!("unix://{}/agent.sock", dest.display());
    }

    // 6. Write the rewritten profile to the final location first, then move
    //    the other artifacts into place.
    let rewritten = serde_yaml_ng::to_string(&profile).context("serialize rewritten profile")?;
    fs::write(dest.join("profile.yaml"), rewritten).context("write profile.yaml")?;

    let prompt_src = scratch.path().join("sys_prompt.md");
    if prompt_src.exists() {
        fs::rename(&prompt_src, dest.join("sys_prompt.md"))
            .or_else(|_| fs::copy(&prompt_src, dest.join("sys_prompt.md")).map(|_| ()))
            .context("install sys_prompt.md")?;
    }

    let skills_src = scratch.path().join("skills");
    if skills_src.exists() {
        fs::create_dir_all(dest.join("skills")).ok();
        for entry in fs::read_dir(&skills_src)?.flatten() {
            if entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                let name = entry.file_name();
                let to = dest.join("skills").join(&name);
                fs::copy(entry.path(), to).context("install skill")?;
            }
        }
    }

    // 7. Check prerequisites (best-effort PATH lookup on command_basename).
    let missing = check_missing_prereqs(&manifest);

    Ok(ImportReport {
        installed_name,
        missing_prerequisites: missing,
    })
}

fn check_missing_prereqs(manifest: &serde_yaml_ng::Value) -> Vec<String> {
    let mut missing = Vec::new();
    if let Some(servers) = manifest["prerequisites"]["mcp_servers"].as_sequence() {
        for s in servers {
            let basename = s["command_basename"].as_str().unwrap_or("");
            if basename.is_empty() {
                continue;
            }
            if !which_exists(basename) {
                missing.push(basename.to_string());
            }
        }
    }
    missing
}

fn which_exists(bin: &str) -> bool {
    // Absolute paths → direct existence check.
    let p = PathBuf::from(bin);
    if p.is_absolute() {
        return p.exists();
    }
    // PATH walk.
    if let Ok(path_env) = std::env::var("PATH") {
        for dir in path_env.split(':') {
            if Path::new(dir).join(bin).exists() {
                return true;
            }
        }
    }
    false
}
