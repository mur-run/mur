//! Per-agent add-on (Claude plugin) import + lifecycle (Phase 2).

pub mod import;
pub mod marketplace;
pub mod parse;

use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cmd::agent::{load_profile_for_edit, save_profile};
use crate::conversations::audit::{Audit, AuditAction};

pub use import::cmd_addon_import;

pub fn cmd_addon_list(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    if profile.addons.is_empty() {
        println!("No add-ons imported for '{name}'.");
        return Ok(());
    }
    for g in &profile.addons {
        println!(
            "{} {} [{}] (skills:{} mcp:{} commands:{})",
            if g.enabled { "on " } else { "off" },
            g.id,
            g.source,
            g.skills.len(),
            g.mcp.len(),
            g.commands.len(),
        );
    }
    Ok(())
}

pub fn cmd_addon_set_enabled(name: &str, addon_id: &str, enabled: bool) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile.set_addon_enabled(addon_id, enabled) {
        bail!("add-on '{addon_id}' not found on '{name}'");
    }
    save_profile(&path, &mut profile)?;
    audit_toggle(name, addon_id, enabled);
    let verb = if enabled { "Enabled" } else { "Disabled" };
    println!("{verb} add-on '{addon_id}' for '{name}' (restart the agent to apply).");
    Ok(())
}

/// Non-destructive remove: removes add-on skill dirs + MCP entries +
/// AddonRef. Does NOT remove agents or non-addon skills.
pub fn cmd_addon_remove(name: &str, addon_id: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let addon = profile
        .addons
        .iter()
        .find(|g| g.id == addon_id)
        .ok_or_else(|| anyhow::anyhow!("add-on '{addon_id}' not found on '{name}'"))?
        .clone();

    let mur_home = crate::cmd::resolve_mur_home()?;
    let agent_skills_dir = mur_home.join("agents").join(name).join("skills");

    // Remove skill dirs (skills + commands share the same dir).
    for skill_name in addon.skills.iter().chain(addon.commands.iter()) {
        let skill_dir = agent_skills_dir.join(skill_name);
        if skill_dir.exists() {
            fs::remove_dir_all(&skill_dir)
                .map_err(|e| anyhow::anyhow!("remove skill dir {}: {e}", skill_dir.display()))?;
        }
    }

    // Remove MCP entries.
    profile.mcp_servers.retain(|m| !addon.mcp.contains(&m.name));

    // Remove the AddonRef.
    profile.addons.retain(|g| g.id != addon_id);

    save_profile(&path, &mut profile)?;
    audit_toggle(name, addon_id, false);
    println!("Removed add-on '{addon_id}' from '{name}'.");
    Ok(())
}

/// Re-fetch an add-on from its recorded `fetch_ref` (or `source_override`),
/// re-apply it (re-running the never-shadow gate + security scan), and
/// preserve the prior `enabled` state.
///
/// Non-destructive: the old skill dirs + `AddonRef` (+ its MCP entries) are
/// backed up before the old copy is removed. If the re-import fails for any
/// reason (network flake, source gone, never-shadow filter drops every
/// member, security scan block, marketplace ambiguity), the backup is
/// restored so the agent ends up exactly as it was before the reimport was
/// attempted — never with neither the old copy nor the new one.
pub fn cmd_addon_reimport(name: &str, addon_id: &str, source_override: Option<&str>) -> Result<()> {
    let (_, profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    let existing = profile
        .addons
        .iter()
        .find(|a| a.id == addon_id)
        .ok_or_else(|| anyhow::anyhow!("add-on '{addon_id}' not found on '{name}'"))?;
    let old_ref = existing.clone();
    let was_enabled = old_ref.enabled;
    let fetch = source_override
        .map(str::to_string)
        .or_else(|| old_ref.fetch_ref.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "add-on '{addon_id}' has no recorded fetch source — pass one explicitly"
            )
        })?;
    // The marketplace `--plugin <name>` selector recorded at the original
    // import, if any (Bug A) — without this a marketplace source bails
    // "specify --plugin" on reimport, after the old copy is already gone.
    let fetch_plugin = old_ref.fetch_plugin.clone();
    let old_mcp_entries: Vec<mur_common::agent::McpServerEntry> = profile
        .mcp_servers
        .iter()
        .filter(|m| old_ref.mcp.contains(&m.name))
        .cloned()
        .collect();

    let mur_home = crate::cmd::resolve_mur_home()?;
    let agent_skills_dir = mur_home.join("agents").join(name).join("skills");
    let backup_root = mur_home.join("agents").join(name).join(format!(
        ".reimport-backup-{addon_id}-{}",
        uuid::Uuid::new_v4()
    ));
    let member_names: Vec<String> = old_ref
        .skills
        .iter()
        .chain(old_ref.commands.iter())
        .cloned()
        .collect();

    for member in &member_names {
        let src = agent_skills_dir.join(member);
        if src.exists() {
            copy_dir_all(&src, &backup_root.join(member)).map_err(|e| {
                anyhow::anyhow!("back up skill dir '{member}' before reimport: {e}")
            })?;
        }
    }

    // Remove the old copy, then re-import fresh (records a new fail-closed ref
    // + content_hash).
    cmd_addon_remove(name, addon_id)?;
    let import_result = import::cmd_addon_import(name, &fetch, fetch_plugin.as_deref(), false);

    match import_result {
        Ok(()) => {
            let _ = fs::remove_dir_all(&backup_root);
            if was_enabled {
                cmd_addon_set_enabled(name, addon_id, true)?;
            }
            Ok(())
        }
        Err(e) => {
            // Restore: copy the backed-up skill dirs back, then re-add the
            // old AddonRef + its MCP entries so the profile matches what it
            // was before this reimport attempt. Best-effort — a restore
            // failure is reported alongside the original error rather than
            // silently swallowed, but the original error always wins as the
            // returned cause.
            let mut restore_errs = Vec::new();
            for member in &member_names {
                let backup_src = backup_root.join(member);
                if backup_src.exists()
                    && let Err(re) = copy_dir_all(&backup_src, &agent_skills_dir.join(member))
                {
                    restore_errs.push(format!("restore skill dir '{member}': {re}"));
                }
            }
            match load_profile_for_edit(name) {
                Ok((path, mut profile)) => {
                    if !profile.addons.iter().any(|a| a.id == addon_id) {
                        profile.addons.push(old_ref.clone());
                    }
                    for m in &old_mcp_entries {
                        if !profile.mcp_servers.iter().any(|e| e.name == m.name) {
                            profile.mcp_servers.push(m.clone());
                        }
                    }
                    if let Err(se) = save_profile(&path, &mut profile) {
                        restore_errs.push(format!("restore add-on record: {se}"));
                    }
                }
                Err(le) => restore_errs.push(format!("reload profile for restore: {le}")),
            }
            let _ = fs::remove_dir_all(&backup_root);
            if restore_errs.is_empty() {
                Err(e.context(format!(
                    "reimport of '{addon_id}' failed; original add-on was restored"
                )))
            } else {
                Err(e.context(format!(
                    "reimport of '{addon_id}' failed AND restore encountered errors ({}); \
                     the agent may be left inconsistent — check '{addon_id}' manually",
                    restore_errs.join("; ")
                )))
            }
        }
    }
}

/// Recursively copy an entire directory tree from `src` to `dest`, creating
/// `dest` (and parents) as needed. Used to back up / restore an add-on's
/// skill dirs around a reimport. Unlike `import::copy_bundle` this does NOT
/// skip `SKILL.md` — a backup must round-trip byte-for-byte. Symlinks are
/// skipped rather than followed/copied (backups are of our own
/// already-validated on-disk content, which shouldn't contain any).
fn copy_dir_all(src: &Path, dest: &Path) -> Result<()> {
    fs::create_dir_all(dest).with_context(|| format!("create backup dir {}", dest.display()))?;
    for entry in fs::read_dir(src).with_context(|| format!("read dir {}", src.display()))? {
        let entry = entry?;
        let ft = entry.file_type()?;
        let src_path = entry.path();
        let dest_path = dest.join(entry.file_name());
        if ft.is_dir() {
            copy_dir_all(&src_path, &dest_path)?;
        } else if ft.is_file() {
            fs::copy(&src_path, &dest_path).with_context(|| {
                format!("copy {} -> {}", src_path.display(), dest_path.display())
            })?;
        }
    }
    Ok(())
}

pub fn cmd_addon_disable_all(name: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.disable_all_addons();
    save_profile(&path, &mut profile)?;
    audit_toggle(name, "*", false);
    println!("Kill-switch: disabled ALL add-ons for '{name}' (restart the agent to apply).");
    Ok(())
}

/// Best-effort audit; logging failure must not block toggle.
fn audit_toggle(agent: &str, target: &str, enabled: bool) {
    if let Ok(mur_home) = crate::cmd::resolve_mur_home()
        && let Ok(audit) = Audit::open(Some(mur_home.to_str().unwrap_or("")))
    {
        let _ = audit.append(
            AuditAction::AddonToggle {
                agent: agent.to_string(),
                target: target.to_string(),
                enabled,
            },
            String::new(),
        );
    }
}

/// Serializes all MUR_HOME-mutating tests across the addon submodules.
/// Defined at the `addon` module level so both `mod.rs` and `import.rs`
/// test blocks can reference it.  Not used outside `#[cfg(test)]`.
#[cfg(test)]
pub(super) static ADDON_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Use the shared lock so import.rs and mod.rs tests never race on MUR_HOME.
    use super::ADDON_TEST_LOCK as ENV_LOCK;

    fn write_agent(home: &std::path::Path, name: &str) {
        let dir = home.join("agents").join(name);
        fs::create_dir_all(&dir).unwrap();
        let p = mur_common::agent::AgentProfile::default_for_tests();
        let yaml = serde_yaml_ng::to_string(&p).unwrap();
        fs::write(dir.join("profile.yaml"), yaml).unwrap();
    }

    fn inject_addon(
        home: &std::path::Path,
        agent: &str,
        addon_id: &str,
        skill_names: &[&str],
        mcp_names: &[&str],
    ) {
        let profile_path = home.join("agents").join(agent).join("profile.yaml");
        let yaml = fs::read_to_string(&profile_path).unwrap();
        let mut profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();

        // Write skill dirs.
        for s in skill_names {
            let skill_dir = home.join("agents").join(agent).join("skills").join(s);
            fs::create_dir_all(&skill_dir).unwrap();
            fs::write(skill_dir.join("skill.yaml"), "name: placeholder\n").unwrap();
        }

        profile.addons.push(mur_common::agent::AddonRef {
            id: addon_id.to_string(),
            source: "claude-local:test@0.1.0".to_string(),
            enabled: false,
            skills: skill_names.iter().map(|s| s.to_string()).collect(),
            mcp: mcp_names.iter().map(|s| s.to_string()).collect(),
            commands: Vec::new(),
            content_hash: None,
            fetch_ref: None,
            fetch_plugin: None,
        });

        // Add a stub MCP entry for each mcp_name.
        for m in mcp_names {
            profile.mcp_servers.push(mur_common::agent::McpServerEntry {
                name: m.to_string(),
                command: "/bin/echo".to_string(),
                args: Vec::new(),
                binary_sha256: None,
                description_hash: None,
                publisher: None,
                installed_at: None,
                timeout_secs: None,
                network: None,
                url: None,
                auth: None,
                requires_programs: Vec::new(),
            });
        }

        let new_yaml = serde_yaml_ng::to_string(&profile).unwrap();
        fs::write(&profile_path, new_yaml).unwrap();
    }

    #[test]
    fn toggle_and_remove() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "tester");
        inject_addon(home, "tester", "myplugin", &["skill-a"], &["mcp-a"]);

        // enable is the ONLY verb that sets enabled=true
        cmd_addon_set_enabled("tester", "myplugin", true).unwrap();
        let (_p, profile) = crate::cmd::agent::load_profile_for_edit("tester").unwrap();
        assert!(
            profile
                .addons
                .iter()
                .find(|g| g.id == "myplugin")
                .unwrap()
                .enabled
        );

        // disable
        cmd_addon_set_enabled("tester", "myplugin", false).unwrap();
        let (_p, profile) = crate::cmd::agent::load_profile_for_edit("tester").unwrap();
        assert!(
            !profile
                .addons
                .iter()
                .find(|g| g.id == "myplugin")
                .unwrap()
                .enabled
        );

        // remove: skill dir gone, MCP gone, AddonRef gone
        cmd_addon_remove("tester", "myplugin").unwrap();
        let (_p, profile) = crate::cmd::agent::load_profile_for_edit("tester").unwrap();
        assert!(profile.addons.iter().all(|g| g.id != "myplugin"));
        assert!(profile.mcp_servers.iter().all(|m| m.name != "mcp-a"));
        assert!(!home.join("agents/tester/skills/skill-a").exists());
    }

    #[test]
    fn remove_does_not_touch_non_addon_skills() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "keeper");

        // Write a NON-add-on skill directly into the agent's skills dir.
        let skills_base = home.join("agents/keeper/skills");
        let keep_dir = skills_base.join("skill-keep");
        fs::create_dir_all(&keep_dir).unwrap();
        fs::write(keep_dir.join("skill.yaml"), "name: skill-keep\n").unwrap();

        // Inject an add-on with one skill AND one command (to exercise both branches).
        let profile_path = home.join("agents/keeper/profile.yaml");
        let yaml = fs::read_to_string(&profile_path).unwrap();
        let mut profile: mur_common::agent::AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();

        let addon_skill_dir = skills_base.join("skill-a");
        fs::create_dir_all(&addon_skill_dir).unwrap();
        fs::write(addon_skill_dir.join("skill.yaml"), "name: skill-a\n").unwrap();

        let addon_cmd_dir = skills_base.join("cmd-review");
        fs::create_dir_all(&addon_cmd_dir).unwrap();
        fs::write(addon_cmd_dir.join("skill.yaml"), "name: cmd-review\n").unwrap();

        profile.addons.push(mur_common::agent::AddonRef {
            id: "sampleplugin".to_string(),
            source: "claude-local:sample@1.0.0".to_string(),
            enabled: false,
            skills: vec!["skill-a".to_string()],
            mcp: Vec::new(),
            commands: vec!["cmd-review".to_string()],
            content_hash: None,
            fetch_ref: None,
            fetch_plugin: None,
        });
        let new_yaml = serde_yaml_ng::to_string(&profile).unwrap();
        fs::write(&profile_path, new_yaml).unwrap();

        // Remove the add-on.
        cmd_addon_remove("keeper", "sampleplugin").unwrap();

        // Add-on skill and command dirs must be gone.
        assert!(
            !addon_skill_dir.exists(),
            "add-on skill dir should be deleted"
        );
        assert!(
            !addon_cmd_dir.exists(),
            "add-on command dir should be deleted"
        );

        // AddonRef must be gone.
        let (_p, profile) = crate::cmd::agent::load_profile_for_edit("keeper").unwrap();
        assert!(profile.addons.iter().all(|g| g.id != "sampleplugin"));

        // Non-add-on skill must survive.
        assert!(
            keep_dir.exists(),
            "non-add-on skill-keep dir must NOT be deleted"
        );
        assert!(
            keep_dir.join("skill.yaml").exists(),
            "skill-keep/skill.yaml must NOT be deleted"
        );
    }

    #[test]
    fn list_empty() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "empty-agent");
        // Should not error on empty list.
        cmd_addon_list("empty-agent").unwrap();
    }

    #[test]
    fn disable_all_kills_switch() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "multi");
        inject_addon(home, "multi", "plugin-a", &[], &[]);
        inject_addon(home, "multi", "plugin-b", &[], &[]);

        // enable both first
        cmd_addon_set_enabled("multi", "plugin-a", true).unwrap();
        cmd_addon_set_enabled("multi", "plugin-b", true).unwrap();

        // kill-switch
        cmd_addon_disable_all("multi").unwrap();
        let (_p, profile) = crate::cmd::agent::load_profile_for_edit("multi").unwrap();
        assert!(profile.addons.iter().all(|g| !g.enabled));
    }

    #[test]
    fn missing_addon_errors() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "noop");
        let err = cmd_addon_set_enabled("noop", "nonexistent", true)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"));
    }

    #[test]
    fn reimport_replaces_and_preserves_enabled() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "reimp");

        // A minimal plugin dir: plugin.json + one skill, so import() records
        // a non-empty content_hash.
        let plugin = home.join("myplugin-src");
        fs::create_dir_all(plugin.join("skills/mytool")).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"myplugin","version":"1.0.0","description":"d","author":"Acme"}"#,
        )
        .unwrap();
        fs::write(
            plugin.join("skills/mytool/SKILL.md"),
            "---\nname: mytool\ndescription: does a thing\n---\nbody\n",
        )
        .unwrap();

        let src = plugin.to_str().unwrap();
        import::cmd_addon_import("reimp", src, None, false).unwrap();
        cmd_addon_set_enabled("reimp", "myplugin", true).unwrap();

        cmd_addon_reimport("reimp", "myplugin", None).unwrap();

        let (_p, profile) = crate::cmd::agent::load_profile_for_edit("reimp").unwrap();
        let g = profile
            .addons
            .iter()
            .find(|g| g.id == "myplugin")
            .expect("addon must still exist after reimport");
        assert!(g.enabled, "enabled state must be preserved across reimport");
        assert!(
            g.content_hash.as_deref().is_some_and(|h| !h.is_empty()),
            "content_hash must be refreshed (Some) after reimport"
        );

        let err = cmd_addon_reimport("reimp", "nope", None)
            .unwrap_err()
            .to_string();
        assert!(err.contains("not found"));
    }

    /// Bug A: the marketplace `--plugin <name>` selector used at import time
    /// must be recorded on the `AddonRef` and replayed on reimport — without
    /// it, `resolve_marketplace` bails "specify --plugin" on reimport, after
    /// `cmd_addon_remove` has already deleted the old copy.
    #[test]
    fn reimport_marketplace_uses_stored_plugin_selector() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "mkt");

        // A minimal marketplace indexing one plugin by relative path.
        let market = home.join("market-src");
        fs::create_dir_all(market.join(".claude-plugin")).unwrap();
        fs::write(
            market.join(".claude-plugin/marketplace.json"),
            r#"{"plugins":[{"name":"mytool","description":"d","source":"./plugins/mytool"}]}"#,
        )
        .unwrap();
        fs::create_dir_all(market.join("plugins/mytool/skills/mytool")).unwrap();
        fs::write(
            market.join("plugins/mytool/plugin.json"),
            r#"{"name":"mytool","version":"1.0.0","description":"d","author":"Acme"}"#,
        )
        .unwrap();
        fs::write(
            market.join("plugins/mytool/skills/mytool/SKILL.md"),
            "---\nname: mytool\ndescription: does a thing\n---\nbody\n",
        )
        .unwrap();

        let src = market.to_str().unwrap();
        import::cmd_addon_import("mkt", src, Some("mytool"), false).unwrap();

        // The selector must be recorded on the ref (independent of reimport
        // working — this is the minimum bar even if the marketplace layout
        // above turns out to need adjusting).
        let (_p, profile) = crate::cmd::agent::load_profile_for_edit("mkt").unwrap();
        let g = profile
            .addons
            .iter()
            .find(|g| g.id == "mytool")
            .expect("addon must be present after import");
        assert_eq!(
            g.fetch_plugin.as_deref(),
            Some("mytool"),
            "fetch_plugin must record the --plugin selector used at import"
        );
        assert_eq!(g.fetch_ref.as_deref(), Some(src));
        drop(profile);

        // Reimport with NO explicit plugin selector must still succeed by
        // replaying the stored one.
        cmd_addon_reimport("mkt", "mytool", None)
            .expect("reimport must succeed using the stored fetch_plugin selector");

        let (_p2, profile2) = crate::cmd::agent::load_profile_for_edit("mkt").unwrap();
        assert!(
            profile2.addons.iter().any(|g| g.id == "mytool"),
            "addon must still exist after marketplace reimport"
        );
    }

    /// Bug B: a reimport that fails partway (here: the recorded fetch source
    /// no longer resolving) must leave the original add-on fully intact —
    /// both its on-disk skill dirs and its `AddonRef` (with `enabled`
    /// unchanged) — never neither-old-nor-new.
    #[test]
    fn reimport_restores_addon_on_import_failure() {
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
        write_agent(home, "restoreme");

        let plugin = home.join("myplugin2-src");
        fs::create_dir_all(plugin.join("skills/mytool2")).unwrap();
        fs::write(
            plugin.join("plugin.json"),
            r#"{"name":"myplugin2","version":"1.0.0","description":"d","author":"Acme"}"#,
        )
        .unwrap();
        fs::write(
            plugin.join("skills/mytool2/SKILL.md"),
            "---\nname: mytool2\ndescription: does a thing\n---\nbody\n",
        )
        .unwrap();

        let src = plugin.to_str().unwrap();
        import::cmd_addon_import("restoreme", src, None, false).unwrap();
        cmd_addon_set_enabled("restoreme", "myplugin2", true).unwrap();

        let (_p, profile_before) = crate::cmd::agent::load_profile_for_edit("restoreme").unwrap();
        let before = profile_before
            .addons
            .iter()
            .find(|g| g.id == "myplugin2")
            .cloned()
            .expect("addon must exist before reimport");
        assert!(before.enabled);
        drop(profile_before);

        let skills_dir = home.join("agents/restoreme/skills");
        for member in before.skills.iter().chain(before.commands.iter()) {
            assert!(
                skills_dir.join(member).exists(),
                "precondition: skill dir '{member}' must exist before reimport"
            );
        }

        // Point reimport at a source that cannot possibly resolve, forcing
        // the import half of reimport to fail after the old copy is removed.
        let err = cmd_addon_reimport(
            "restoreme",
            "myplugin2",
            Some("/nonexistent/does-not-exist"),
        )
        .expect_err("reimport must fail when the source cannot be resolved");
        assert!(
            err.to_string().contains("myplugin2")
                || err.chain().any(|c| c.to_string().contains("myplugin2")),
            "error should mention the addon being restored: {err:#}"
        );

        // The original add-on must be restored: same AddonRef (enabled
        // unchanged) and its skill dirs back on disk.
        let (_p2, profile_after) = crate::cmd::agent::load_profile_for_edit("restoreme").unwrap();
        let after = profile_after
            .addons
            .iter()
            .find(|g| g.id == "myplugin2")
            .expect("addon must still be present after a failed reimport (restored)");
        assert_eq!(
            after.enabled, before.enabled,
            "enabled state must be unchanged"
        );
        assert_eq!(after.skills, before.skills);
        assert_eq!(after.commands, before.commands);
        assert_eq!(after.fetch_ref, before.fetch_ref);

        for member in before.skills.iter().chain(before.commands.iter()) {
            assert!(
                skills_dir.join(member).exists(),
                "skill dir '{member}' must be restored on disk after a failed reimport"
            );
        }
    }
}
