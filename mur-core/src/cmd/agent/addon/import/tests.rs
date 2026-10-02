use super::*;
use std::fs;

#[test]
fn resolve_addon_source_classifies_git_and_local() {
    // GitHub `owner/repo` shorthand → clone from github over https.
    assert_eq!(
        resolve_addon_source("mur-run/skill-registry"),
        AddonSource::Git {
            clone_url: "https://github.com/mur-run/skill-registry.git".into(),
            cache_key: "github.com-mur-run-skill-registry".into(),
        }
    );
    // Explicit https URL kept as-is; same cache key as shorthand.
    assert_eq!(
        resolve_addon_source("https://github.com/mur-run/skill-registry.git"),
        AddonSource::Git {
            clone_url: "https://github.com/mur-run/skill-registry.git".into(),
            cache_key: "github.com-mur-run-skill-registry".into(),
        }
    );
    // SSH URL.
    assert!(matches!(
        resolve_addon_source("git@github.com:o/r.git"),
        AddonSource::Git { .. }
    ));
    // Local paths stay local.
    assert_eq!(
        resolve_addon_source("./plugins/ponytail"),
        AddonSource::Local
    );
    assert_eq!(resolve_addon_source("/abs/path"), AddonSource::Local);
    assert_eq!(resolve_addon_source("~/x"), AddonSource::Local);
    // A multi-segment relative path is NOT owner/repo shorthand.
    assert_eq!(resolve_addon_source("a/b/c"), AddonSource::Local);
}

// Delegate to the shared lock defined at the addon module level so that
// tests in import.rs and mod.rs are serialized against each other.

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
    // MCP points at a real file we create here (absolute path) so
    // resolve_command canonicalizes + sha256 pins on ANY OS. A hardcoded
    // `/bin/echo` doesn't exist on Windows CI. Build the JSON via serde_json
    // so the (possibly back-slashed) Windows path is escaped correctly.
    let mcp_bin = root.join("mcp-bin");
    fs::write(&mcp_bin, b"dummy mcp binary\n").unwrap();
    let mcp = serde_json::json!({
        "mcpServers": {
            "echo": {
                "command": mcp_bin.to_str().unwrap(),
                "args": ["hi"],
                "env": { "TOKEN": "x" },
            }
        }
    });
    fs::write(root.join(".mcp.json"), serde_json::to_string(&mcp).unwrap()).unwrap();
}

#[test]
fn copy_bundle_preserves_scripts_skips_skill_md() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    fs::create_dir_all(src.join("scripts")).unwrap();
    fs::write(src.join("SKILL.md"), "body").unwrap();
    fs::write(src.join("scripts/start-server.sh"), "#!/bin/sh\n").unwrap();
    fs::write(src.join("helper.js"), "x").unwrap();

    copy_bundle(&src, &dest).unwrap();

    assert!(dest.join("scripts/start-server.sh").is_file());
    assert!(dest.join("helper.js").is_file());
    assert!(
        !dest.join("SKILL.md").exists(),
        "SKILL.md must not be copied"
    );
}

#[cfg(unix)]
#[test]
fn copy_bundle_rejects_symlink_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dest = tmp.path().join("dest");
    fs::create_dir_all(&src).unwrap();
    let secret = tmp.path().join("secret.txt");
    fs::write(&secret, "s").unwrap();
    std::os::unix::fs::symlink(&secret, src.join("link")).unwrap();

    let err = copy_bundle(&src, &dest);
    assert!(err.is_err(), "symlink in bundle must be rejected");
}

#[cfg(unix)]
#[test]
fn validate_bundle_rejects_nested_symlink() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    fs::create_dir_all(src.join("scripts")).unwrap();
    let secret = tmp.path().join("secret.txt");
    fs::write(&secret, "s").unwrap();
    // Symlink one level deep — must still be caught.
    std::os::unix::fs::symlink(&secret, src.join("scripts/link")).unwrap();

    assert!(
        validate_bundle(&src).is_err(),
        "a symlink nested in a subdir must be rejected"
    );
}

#[cfg(unix)]
#[test]
fn import_with_symlink_bundle_is_atomic_no_orphan() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    envg.set_var("MUR_HOME", home);
    write_agent(home, "alice");
    let plugin = home.join("sample-plugin");
    write_plugin(&plugin);
    // Bundle carries a symlink → import must reject it.
    fs::create_dir_all(plugin.join("skills/brainstorm/scripts")).unwrap();
    let secret = home.join("secret.txt");
    fs::write(&secret, "s").unwrap();
    std::os::unix::fs::symlink(&secret, plugin.join("skills/brainstorm/scripts/link")).unwrap();

    let res = cmd_addon_import("alice", plugin.to_str().unwrap(), None, false);
    assert!(res.is_err(), "symlink bundle must fail the import");
    // Atomicity: no half-written skill dir left behind.
    assert!(
        !home.join("agents/alice/skills/brainstorm").exists(),
        "failed import must not orphan a skill dir"
    );
}

#[test]
fn import_installs_skill_bundle_scripts() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    envg.set_var("MUR_HOME", home);
    write_agent(home, "alice");
    let plugin = home.join("sample-plugin");
    write_plugin(&plugin);
    // Add a bundled script sibling next to SKILL.md.
    fs::create_dir_all(plugin.join("skills/brainstorm/scripts")).unwrap();
    fs::write(
        plugin.join("skills/brainstorm/scripts/run.sh"),
        "#!/bin/sh\necho hi\n",
    )
    .unwrap();

    cmd_addon_import("alice", plugin.to_str().unwrap(), None, false).unwrap();

    assert!(
        home.join("agents/alice/skills/brainstorm/scripts/run.sh")
            .is_file()
    );
}

#[test]
fn import_is_fail_closed_isolated_and_pins_mcp() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    // Point the importer at this home.
    envg.set_var("MUR_HOME", home);
    write_agent(home, "alice");
    write_agent(home, "bob");
    let plugin = home.join("sample-plugin");
    write_plugin(&plugin);

    cmd_addon_import("alice", plugin.to_str().unwrap(), None, false).unwrap();

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
fn import_finds_plugin_json_under_dot_claude_plugin() {
    // Stock Claude marketplace layout: plugin.json lives in .claude-plugin/,
    // while skills/ stay at the dir root. Import must still resolve it.
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    envg.set_var("MUR_HOME", home);
    write_agent(home, "dana");
    let plugin = home.join("claude-layout-plugin");
    fs::create_dir_all(plugin.join(".claude-plugin")).unwrap();
    fs::create_dir_all(plugin.join("skills/brainstorm")).unwrap();
    fs::write(
        plugin.join(".claude-plugin/plugin.json"),
        r#"{"name":"claudefmt","version":"1.0.0","description":"d","author":"Acme"}"#,
    )
    .unwrap();
    fs::write(
        plugin.join("skills/brainstorm/SKILL.md"),
        "---\nname: brainstorm\ndescription: think\n---\nbody\n",
    )
    .unwrap();

    cmd_addon_import("dana", plugin.to_str().unwrap(), None, false).unwrap();

    let (_p, dana) = crate::cmd::agent::load_profile_for_edit("dana").unwrap();
    let g = dana.addons.iter().find(|g| g.id == "claudefmt").unwrap();
    assert!(g.skills.contains(&"brainstorm".to_string()));
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
fn safe_member_name_rejects_dot() {
    // "." resolves to the skills dir itself — remove_dir_all would nuke all skills.
    assert!(safe_member_name(".").is_err());
    // ".." is also blocked (covered by contains("..") but belt-and-suspenders).
    assert!(safe_member_name("..").is_err());
    // Any dotfile name is rejected (leading-dot rule).
    assert!(safe_member_name(".hidden").is_err());
    // Normal names are still allowed.
    assert!(safe_member_name("my-skill").is_ok());
    assert!(safe_member_name("skill_2").is_ok());
}

#[test]
fn import_refuses_to_overwrite_existing_skill() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    envg.set_var("MUR_HOME", home);
    write_agent(home, "charlie");
    let plugin = home.join("sample-plugin2");
    write_plugin(&plugin);

    // First import must succeed.
    cmd_addon_import("charlie", plugin.to_str().unwrap(), None, false).unwrap();

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
    let err = cmd_addon_import("charlie", plugin.to_str().unwrap(), None, false)
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

#[test]
fn import_skips_skill_that_shadows_the_global_store() {
    let mut envg = mur_common::test_env::EnvGuard::hold();
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    envg.set_var("MUR_HOME", home);
    write_agent(home, "eve");

    // Seed a global-store skill named "brainstorm" (~/.mur/skills/brainstorm/skill.yaml).
    let global_skill = mur_common::skill::parse_canonical(
        r#"
name: brainstorm
version: 1.0.0
publisher: human:t
description: d
category: context
content:
  abstract: a
  context: body
"#,
    )
    .unwrap();
    mur_common::skill::write_to_dir(&home.join("skills").join("brainstorm"), &global_skill)
        .unwrap();

    // Plugin bundles two skills: "brainstorm" (collides with the global
    // store) and "unique" (does not).
    let plugin = home.join("shadow-plugin");
    fs::create_dir_all(plugin.join("skills/brainstorm")).unwrap();
    fs::create_dir_all(plugin.join("skills/unique")).unwrap();
    fs::write(
        plugin.join("plugin.json"),
        r#"{"name":"shadow","version":"1.0.0","description":"d","author":"Acme"}"#,
    )
    .unwrap();
    fs::write(
        plugin.join("skills/brainstorm/SKILL.md"),
        "---\nname: brainstorm\ndescription: think\n---\nbody\n",
    )
    .unwrap();
    fs::write(
        plugin.join("skills/unique/SKILL.md"),
        "---\nname: unique\ndescription: distinct\n---\nbody\n",
    )
    .unwrap();

    let plugin_dir_arg = plugin.to_str().unwrap().to_string();
    cmd_addon_import("eve", &plugin_dir_arg, None, false).unwrap();

    // The colliding skill must never be written under the agent.
    assert!(
        !home.join("agents/eve/skills/brainstorm").exists(),
        "shadowing skill must not be installed for the agent"
    );
    // The non-colliding skill must be installed.
    assert!(
        home.join("agents/eve/skills/unique").exists(),
        "non-colliding skill must still be installed"
    );

    let (_p, eve) = crate::cmd::agent::load_profile_for_edit("eve").unwrap();
    let g = eve.addons.iter().find(|g| g.id == "shadow").unwrap();
    assert!(
        g.skills.contains(&"unique".to_string()),
        "AddonRef.skills must contain the non-colliding skill"
    );
    assert!(
        !g.skills.contains(&"brainstorm".to_string()),
        "AddonRef.skills must NOT contain the shadowing skill"
    );
    assert!(
        g.content_hash.as_deref().is_some_and(|h| !h.is_empty()),
        "content_hash must be Some(non-empty)"
    );
    assert_eq!(
        g.fetch_ref.as_deref(),
        Some(plugin_dir_arg.as_str()),
        "fetch_ref must record the original plugin_dir argument"
    );
}
