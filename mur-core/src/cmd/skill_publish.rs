//! Publish a skill to the default registry via fork + PR.
//!
//! Requires the GitHub CLI (`gh`) to be installed and authenticated.

use anyhow::{Context, Result, anyhow, bail};
use mur_common::identity::AgentIdentity;
use mur_common::skill::{parse_canonical, serialize_canonical, sign_manifest, validate};
use std::process::Command;

const REGISTRY_REPO: &str = "mur-run/skill-registry";

pub fn cmd_publish(path: &str) -> Result<()> {
    // 1. Read and validate the skill
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path))?;
    let m = parse_canonical(&text)?;
    validate(&m)?;

    // 2. Sign with publisher identity
    let identity = resolve_publisher_identity()?;
    let envelope = sign_manifest(&m, &identity)?;
    println!("✓ Skill signed");

    // 3. Check for gh CLI
    if Command::new("gh").arg("--version").output().is_err() {
        bail!("GitHub CLI (`gh`) not found. Install from https://cli.github.com/");
    }
    let auth_out = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .context("check gh auth")?;
    if !auth_out.status.success() {
        bail!("`gh auth status` failed — please run `gh auth login` first");
    }

    // 4. Determine fork
    let gh_user = current_gh_user()?;
    let fork_repo = format!("{gh_user}/skill-registry");
    let fork_exists = Command::new("gh")
        .args(["repo", "view", &fork_repo, "--json", "nameWithOwner"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if !fork_exists {
        println!("→ Forking {REGISTRY_REPO}...");
        let s = Command::new("gh")
            .args(["repo", "fork", REGISTRY_REPO, "--clone=false"])
            .status()
            .context("fork registry")?;
        if !s.success() {
            bail!("failed to fork {REGISTRY_REPO}");
        }
    }

    // 5. Clone fork, write skill, commit, push
    let tmpdir = tempfile::tempdir().context("create temp dir")?;
    let repo_dir = tmpdir.path().join("skill-registry");

    println!("→ Cloning fork...");
    let s = Command::new("git")
        .args([
            "clone",
            &format!("https://github.com/{fork_repo}.git"),
            &*repo_dir.to_string_lossy(),
        ])
        .status()
        .context("clone fork")?;
    if !s.success() {
        bail!("failed to clone fork");
    }

    Command::new("git")
        .args([
            "-C",
            &*repo_dir.to_string_lossy(),
            "remote",
            "add",
            "upstream",
            &format!("https://github.com/{REGISTRY_REPO}.git"),
        ])
        .status()
        .ok();

    let branch = format!("skill-{}", m.name);
    Command::new("git")
        .args([
            "-C",
            &*repo_dir.to_string_lossy(),
            "checkout",
            "-b",
            &branch,
        ])
        .status()
        .context("create branch")?;

    let skill_dir = repo_dir.join("skills").join(&m.name).join("versions");
    std::fs::create_dir_all(&skill_dir)?;
    let skill_path = skill_dir.join(format!("{}.yaml", m.version));
    std::fs::write(&skill_path, serialize_canonical(&m)?)?;

    let sig_path = skill_dir.join(format!("{}.sig.json", m.version));
    std::fs::write(&sig_path, &envelope)?;

    Command::new("git")
        .args(["-C", &*repo_dir.to_string_lossy(), "add", "."])
        .status()
        .context("git add")?;
    Command::new("git")
        .args([
            "-C",
            &*repo_dir.to_string_lossy(),
            "commit",
            "-m",
            &format!("feat: add {name} v{ver}", name = m.name, ver = m.version),
            "-m",
            &format!("Publisher: {}\nSigned: true", m.publisher),
        ])
        .status()
        .context("git commit")?;

    println!("→ Pushing branch...");
    let s = Command::new("git")
        .args([
            "-C",
            &*repo_dir.to_string_lossy(),
            "push",
            "origin",
            &branch,
        ])
        .status()
        .context("git push")?;
    if !s.success() {
        bail!("git push failed");
    }

    // 6. Create PR
    println!("→ Creating PR...");
    let pr_out = Command::new("gh")
        .args([
            "pr", "create",
            "--repo", REGISTRY_REPO,
            "--head", &format!("{}:{}", gh_user, branch),
            "--title", &format!("feat: add {name} v{ver}", name=m.name, ver=m.version),
            "--body", &format!(
                "## Summary\n\nAdd `{name}` skill version `{ver}` by {pub_}.\n\n## Verification\n\n- Content hash: {hash}\n- Signed: yes\n\n---\n*Created via `mur skill publish`*",
                name=m.name, ver=m.version, pub_=m.publisher,
                hash=mur_common::skill::content_sha256(&m).unwrap_or_default()
            ),
        ])
        .output().context("create PR")?;
    let pr_url = String::from_utf8_lossy(&pr_out.stdout).trim().to_string();
    if !pr_out.status.success() {
        let err = String::from_utf8_lossy(&pr_out.stderr);
        bail!("PR creation failed: {err}");
    }

    println!("✓ Published! PR: {pr_url}");
    Ok(())
}

fn resolve_publisher_identity() -> Result<AgentIdentity> {
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let key_path = home.join(".mur").join("publisher-identity.key");
    if key_path.exists() {
        AgentIdentity::load(&home.join(".mur")).map_err(|e| anyhow!("load publisher identity: {e}"))
    } else {
        let identity = AgentIdentity::generate();
        std::fs::create_dir_all(key_path.parent().unwrap())?;
        identity
            .save(&home.join(".mur"))
            .map_err(|e| anyhow!("save publisher identity: {e}"))?;
        eprintln!("ℹ Generated new publisher identity at ~/.mur/");
        Ok(identity)
    }
}

fn current_gh_user() -> Result<String> {
    let out = Command::new("gh")
        .args(["api", "user", "--jq", ".login"])
        .output()
        .context("get gh user")?;
    if !out.status.success() {
        bail!("failed to get GitHub username. Run `gh auth login` first");
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
