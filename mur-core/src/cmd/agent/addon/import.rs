//! Claude plugin importer (Phase 2). Expands local plugin dir into
//! per-agent skills + command-skills + MCP entries, recorded under one
//! fail-closed (disabled) `AddonRef`.

use std::fs;
use std::path::PathBuf;

use anyhow::{Result, bail};

use mur_common::agent::{AddonRef, McpServerEntry};
use mur_common::skill::SkillManifest;
use mur_common::skill::scan::scan_skill;
use mur_common::skill::write_to_dir;

use super::parse::{
    PluginJson, command_to_manifest, manifest_version, parse_mcp_json, skill_md_to_manifest,
};
use crate::cmd::agent::{load_profile_for_edit, save_profile};
use crate::cmd::agent_mcp_pin::{build_pinned_entry, compute_binary_sha256, resolve_command};

/// Provenance tag plugin imported local directory (no marketplace).
const SOURCE_LOCAL: &str = "claude-local";
/// Shell-ish argv tokens warrant advisory (non-blocking) warning.
const SHELLISH_TOKENS: &[&str] = &["-c", "eval"];

/// Reject member names escape agent's skills dir.
pub fn safe_member_name(n: &str) -> Result<()> {
    if n.is_empty() || n.contains('/') || n.contains('\\') || n.contains("..") {
        bail!("unsafe add-on member name: {n:?}");
    }
    Ok(())
}

/// Collected MCP entry plus env-notice keys (notice printed only after all checks pass).
struct PendingMcp {
    server_name: String,
    entry: McpServerEntry,
    env_keys: Vec<String>,
    shellish_warn: bool,
}

pub fn cmd_addon_import(name: &str, plugin_dir: &str, force: bool) -> Result<()> {
    let (profile_path, mut profile) = load_profile_for_edit(name)?;
    let mur_home = crate::cmd::resolve_mur_home()?;
    let agent_skills_dir = mur_home.join("agents").join(name).join("skills");

    // Canonicalize the plugin root (rejects a non-existent dir).
    let root = fs::canonicalize(plugin_dir)
        .map_err(|e| anyhow::anyhow!("plugin dir {plugin_dir:?}: {e}"))?;
    let plugin: PluginJson = serde_json::from_str(
        &fs::read_to_string(root.join("plugin.json"))
            .map_err(|e| anyhow::anyhow!("read plugin.json: {e}"))?,
    )?;

    let addon_id = plugin.name.clone();
    if profile.addons.iter().any(|g| g.id == addon_id) {
        bail!("add-on '{addon_id}' already imported into '{name}'; remove it first");
    }

    // ── Phase 1: Collect + validate (NO disk writes) ──────────────────────────
    // All checks must pass before a single byte is written.

    // Pending skills/commands: (dest_path, manifest)
    let mut pending_skills: Vec<(PathBuf, SkillManifest)> = Vec::new();
    let mut pending_cmds: Vec<(PathBuf, SkillManifest)> = Vec::new();
    // Pending MCP entries.
    let mut pending_mcp: Vec<PendingMcp> = Vec::new();

    // 1a. skills/<dir>/SKILL.md
    let skills_dir = root.join("skills");
    if skills_dir.is_dir() {
        for entry in fs::read_dir(&skills_dir)? {
            let d = entry?.path();
            let md = d.join("SKILL.md");
            if !md.is_file() {
                continue;
            }
            let dir_name = d.file_name().and_then(|s| s.to_str()).unwrap_or_default();
            let manifest = skill_md_to_manifest(dir_name, &fs::read_to_string(&md)?, &plugin);
            safe_member_name(&manifest.name)?;
            scan_or_block(&manifest, force)?;
            let dest = agent_skills_dir.join(&manifest.name);
            if dest.exists() {
                bail!(
                    "skill '{}' already exists for agent '{name}'; remove it first",
                    manifest.name
                );
            }
            pending_skills.push((dest, manifest));
        }
    }

    // 1b. commands/<name>.toml
    let cmds_dir = root.join("commands");
    if cmds_dir.is_dir() {
        for entry in fs::read_dir(&cmds_dir)? {
            let p = entry?.path();
            if p.extension().and_then(|e| e.to_str()) != Some("toml") {
                continue;
            }
            let cmd_name = p.file_stem().and_then(|s| s.to_str()).unwrap_or_default();
            let manifest = command_to_manifest(cmd_name, &fs::read_to_string(&p)?, &plugin)?;
            safe_member_name(&manifest.name)?;
            scan_or_block(&manifest, force)?;
            let dest = agent_skills_dir.join(&manifest.name);
            if dest.exists() {
                bail!(
                    "skill '{}' already exists for agent '{name}'; remove it first",
                    manifest.name
                );
            }
            pending_cmds.push((dest, manifest));
        }
    }

    // 1c. .mcp.json — validate command, pin sha256, surface env notice only.
    let mcp_path = root.join(".mcp.json");
    if mcp_path.is_file() {
        let j = parse_mcp_json(&fs::read_to_string(&mcp_path)?)?;
        for (server, srv) in j.mcp_servers {
            if profile.mcp_servers.iter().any(|m| m.name == server) {
                bail!(
                    "MCP server '{server}' already exists on '{name}'; rename or remove it first"
                );
            }
            // Resolve + hash the binary (rejects path-escape / missing binary).
            let resolved = resolve_command(&srv.command)
                .map_err(|e| anyhow::anyhow!("MCP '{server}' command {:?}: {e}", srv.command))?;
            let sha = compute_binary_sha256(&resolved)?;
            let shellish_warn = srv
                .args
                .iter()
                .any(|a| SHELLISH_TOKENS.contains(&a.as_str()));
            let env_keys: Vec<String> = srv.env.keys().cloned().collect();
            let entry =
                build_pinned_entry(&server, &srv.command, &srv.args, sha, String::new(), None);
            pending_mcp.push(PendingMcp {
                server_name: server,
                entry,
                env_keys,
                shellish_warn,
            });
        }
    }

    // ── Phase 2: Commit (writes only after all checks passed) ─────────────────

    // 2a. Write skills.
    let mut skill_members: Vec<String> = Vec::new();
    for (dest, manifest) in pending_skills {
        let skill_name = manifest.name.clone();
        write_to_dir(&dest, &manifest)
            .map_err(|e| anyhow::anyhow!("write skill {}: {e}", skill_name))?;
        skill_members.push(skill_name);
    }

    // 2b. Write commands.
    let mut cmd_members: Vec<String> = Vec::new();
    for (dest, manifest) in pending_cmds {
        let cmd_name = manifest.name.clone();
        write_to_dir(&dest, &manifest)
            .map_err(|e| anyhow::anyhow!("write command {}: {e}", cmd_name))?;
        cmd_members.push(cmd_name);
    }

    // 2c. Push MCP entries + print notices.
    let mut mcp_members: Vec<String> = Vec::new();
    for pm in pending_mcp {
        if pm.shellish_warn {
            eprintln!(
                "warning: MCP '{}' args contain shell-ish tokens {SHELLISH_TOKENS:?}; review before enabling",
                pm.server_name
            );
        }
        if !pm.env_keys.is_empty() {
            println!(
                "note: MCP '{}' declares env vars (NOT imported). Wire them with:",
                pm.server_name
            );
            for k in &pm.env_keys {
                println!("  mur agent secret set {name} {k}");
            }
        }
        mcp_members.push(pm.server_name);
        profile.mcp_servers.push(pm.entry);
    }

    // 2d. Record the group — FAIL-CLOSED (disabled). The only choke point.
    profile.addons.push(AddonRef {
        id: addon_id.clone(),
        source: format!(
            "{SOURCE_LOCAL}:{}@{}",
            plugin.name,
            manifest_version(&plugin)
        ),
        enabled: false,
        skills: skill_members,
        mcp: mcp_members,
        commands: cmd_members,
    });
    save_profile(&profile_path, &mut profile)?;

    println!(
        "Imported add-on '{addon_id}' into '{name}' (disabled). Enable with:\n  mur agent addon enable {name} {addon_id}"
    );
    Ok(())
}

/// Run the security scan; block unless `--force` (spec §7).
fn scan_or_block(manifest: &mur_common::skill::SkillManifest, force: bool) -> Result<()> {
    let report = scan_skill(manifest)?;
    if report.has_blocking_findings() && !force {
        bail!(
            "security scan refused skill '{}'; re-run with --force to override",
            manifest.name
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Delegate to the shared lock defined at the addon module level so that
    // tests in import.rs and mod.rs are serialized against each other.
    use super::super::ADDON_TEST_LOCK as ENV_LOCK;

    // Minimal agent profile on disk so load_profile_for_edit works.
    fn write_agent(home: &std::path::Path, name: &str) {
        let dir = home.join("agents").join(name);
        fs::create_dir_all(&dir).unwrap();
        let p = mur_common::agent::AgentProfile::default_for_tests();
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        fs::write(dir.join("profile.yaml"), yaml).unwrap();
    }

    fn write_plugin(root: &std::path::Path) {
        fs::create_dir_all(root.join("skills/brainstorm")).unwrap();
        fs::create_dir_all(root.join("commands")).unwrap();
        fs::write(
            root.join("plugin.json"),
            r#"{"name":"sample","version":"1.2.3","description":"d","author":"Acme"}"#,
        )
        .unwrap();
        fs::write(
            root.join("skills/brainstorm/SKILL.md"),
            "---\nname: brainstorm\ndescription: think\n---\nbody\n",
        )
        .unwrap();
        fs::write(
            root.join("commands/review.toml"),
            "prompt = \"review {{args}}\"\n",
        )
        .unwrap();
        // MCP pointing at a real, always-present executable so sha256 pins.
        fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"echo":{"command":"/bin/echo","args":["hi"],"env":{"TOKEN":"x"}}}}"#,
        )
        .unwrap();
    }

    #[test]
    fn import_is_fail_closed_isolated_and_pins_mcp() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Point the importer at this home.
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "alice");
        write_agent(home, "bob");
        let plugin = home.join("sample-plugin");
        write_plugin(&plugin);

        cmd_addon_import("alice", plugin.to_str().unwrap(), false).unwrap();

        // Reload alice's profile.
        let (_p, alice) = crate::cmd::agent::load_profile_for_edit("alice").unwrap();
        let g = alice.addons.iter().find(|g| g.id == "sample").unwrap();
        // Fail-closed.
        assert!(!g.enabled);
        assert!(g.skills.contains(&"brainstorm".to_string()));
        assert!(g.commands.contains(&"review".to_string()));
        assert!(g.mcp.contains(&"echo".to_string()));
        // MCP pinned with a sha and env NOT written into the profile.
        let echo = alice.mcp_servers.iter().find(|m| m.name == "echo").unwrap();
        assert!(echo.binary_sha256.is_some());
        let yaml = serde_yaml_ng::to_string(&alice).unwrap();
        assert!(!yaml.contains("TOKEN")); // env surfaced as notice only

        // Per-agent isolation: skill written under alice, not bob.
        assert!(
            home.join("agents/alice/skills/brainstorm/skill.yaml")
                .exists()
        );
        assert!(
            !home
                .join("agents/bob/skills/brainstorm/skill.yaml")
                .exists()
        );
    }

    #[test]
    fn rejects_path_escaping_member_name() {
        // safe_member_name is the traversal guard.
        assert!(safe_member_name("ok").is_ok());
        assert!(safe_member_name("../evil").is_err());
        assert!(safe_member_name("a/b").is_err());
        assert!(safe_member_name("").is_err());
    }

    #[test]
    fn import_refuses_to_overwrite_existing_skill() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "charlie");
        let plugin = home.join("sample-plugin2");
        write_plugin(&plugin);

        // First import must succeed.
        cmd_addon_import("charlie", plugin.to_str().unwrap(), false).unwrap();

        // Record the original skill content so we can verify it is not modified.
        let skill_path = home.join("agents/charlie/skills/brainstorm/skill.yaml");
        assert!(skill_path.exists(), "skill should exist after first import");
        let original_content = fs::read_to_string(&skill_path).unwrap();

        // Remove the addon record and MCP entry directly from the on-disk profile
        // so the duplicate-addon / MCP-collision guards don't fire before the
        // skill collision guard (the path we want to exercise).
        let profile_path = home.join("agents/charlie/profile.yaml");
        let yaml = fs::read_to_string(&profile_path).unwrap();
        let mut profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        profile.addons.retain(|a| a.id != "sample");
        profile.mcp_servers.retain(|m| m.name != "echo");
        let new_yaml = serde_yaml_ng::to_string(&profile).unwrap();
        fs::write(&profile_path, new_yaml).unwrap();

        // Second import must fail with the overwrite refusal message.
        let err = cmd_addon_import("charlie", plugin.to_str().unwrap(), false)
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("already exists") && err.contains("brainstorm"),
            "expected overwrite-refusal error, got: {err}"
        );

        // The original skill file must be unchanged (no partial write).
        let after_content = fs::read_to_string(&skill_path).unwrap();
        assert_eq!(
            original_content, after_content,
            "skill file was modified despite collision bail"
        );
    }
}
