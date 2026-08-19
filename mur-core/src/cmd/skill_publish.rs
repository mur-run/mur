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
    // Embed the DSSE envelope as `publisher_signature` in the yaml (what registry-index expects).
    let yaml = serialize_canonical(&m)?;
    let signed = format!(
        "{}publisher_signature: '{}'\n",
        yaml,
        envelope.replace('\'', "''")
    );
    let skill_path = skill_dir.join(format!("{}.yaml", m.version));
    std::fs::write(&skill_path, signed)?;

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
    resolve_publisher_identity_in(&home.join(".mur"))
}

/// Split out from `resolve_publisher_identity` so the thing that matters can be
/// asserted: that this never writes `<mur_home>/identity.key`. The version that
/// read `dirs::home_dir()` directly could not be tested at all, which is part of
/// why #1011 sat there.
fn resolve_publisher_identity_in(mur_home: &std::path::Path) -> Result<AgentIdentity> {
    // Its OWN directory (#1011). This used to guard on
    // `~/.mur/publisher-identity.key` while `AgentIdentity::save`/`load` join
    // `identity.key` — so the guard tested a file nothing ever created, always
    // took the else branch, and overwrote `~/.mur/identity.key`: the HOST key,
    // with no `.prev` and no rotation attestation to recover from.
    //
    // A separate directory keeps `save`/`load` usable as-is and makes a
    // collision structurally impossible rather than merely unlikely.
    let dir = mur_home.join("publisher");
    if dir.join("identity.key").exists() {
        AgentIdentity::load(&dir).map_err(|e| anyhow!("load publisher identity: {e}"))
    } else {
        let identity = AgentIdentity::generate();
        std::fs::create_dir_all(&dir)?;
        identity
            .save(&dir)
            .map_err(|e| anyhow!("save publisher identity: {e}"))?;
        eprintln!("ℹ Generated new publisher identity at ~/.mur/publisher/");
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

#[cfg(test)]
mod tests {
    use super::*;

    /// #1011: resolving a publisher identity must never touch the HOST key.
    ///
    /// The old code guarded on `publisher-identity.key` — a file nothing ever
    /// created — while `save`/`load` join `identity.key`. So the guard was
    /// always false, the else branch always ran, and `fs::write` truncated
    /// `~/.mur/identity.key`. Unrecoverable: no `.prev`, and no rotation
    /// attestation to bridge the swap.
    #[test]
    fn resolving_a_publisher_identity_never_touches_the_host_key() {
        let tmp = tempfile::TempDir::new().unwrap();
        let mur_home = tmp.path();
        std::fs::create_dir_all(mur_home).unwrap();

        // A host key, exactly as a real ~/.mur has.
        let host = AgentIdentity::generate();
        host.save(mur_home).unwrap();
        let host_bytes = std::fs::read(mur_home.join("identity.key")).unwrap();

        let publisher = resolve_publisher_identity_in(mur_home).unwrap();

        assert_eq!(
            std::fs::read(mur_home.join("identity.key")).unwrap(),
            host_bytes,
            "the host key was overwritten"
        );
        assert_ne!(
            publisher.pubkey_text(),
            host.pubkey_text(),
            "the publisher identity must not BE the host key"
        );
        assert!(mur_home.join("publisher").join("identity.key").exists());
    }

    /// ...and it is stable: a second call returns the same identity rather than
    /// minting a new one, which is what makes published signatures verifiable
    /// across releases.
    #[test]
    fn resolving_twice_returns_the_same_publisher_identity() {
        let tmp = tempfile::TempDir::new().unwrap();
        let first = resolve_publisher_identity_in(tmp.path()).unwrap();
        let second = resolve_publisher_identity_in(tmp.path()).unwrap();
        assert_eq!(first.pubkey_text(), second.pubkey_text());
    }
}
