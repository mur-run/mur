//! Claude plugin importer (Phase 2). Expands local plugin dir into
//! per-agent skills + command-skills + MCP entries, recorded under one
//! fail-closed (disabled) `AddonRef`.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

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
    if n.is_empty() || n.contains('/') || n.contains('\\') || n.contains("..") || n.starts_with('.')
    {
        bail!("unsafe add-on member name: {n:?}");
    }
    Ok(())
}

/// Reject a bundle entry that could let a copy escape its install directory.
/// Shared by the phase-1 validation walk (atomicity: fail before any write)
/// and the phase-2 copy walk (TOCTOU: enforce again at the moment of copy).
fn check_bundle_entry(name_str: &str, ft: &std::fs::FileType) -> anyhow::Result<()> {
    // Reject `.`, `..`, path separators, absolute-ish names.
    if name_str == "." || name_str == ".." || name_str.contains('/') {
        anyhow::bail!("unsafe bundle entry name: {name_str}");
    }
    // Reject symlinks outright (escape vector).
    if ft.is_symlink() {
        anyhow::bail!("bundle contains a symlink ({name_str}); refusing import");
    }
    Ok(())
}

/// Phase-1 validation: walk the source bundle rejecting symlinks / unsafe names
/// WITHOUT writing anything. Run before `write_to_dir`, this preserves the
/// importer's "all checks must pass before a single byte is written" invariant —
/// a bad bundle fails the whole import instead of orphaning a half-written dir.
pub(crate) fn validate_bundle(src_dir: &Path) -> anyhow::Result<()> {
    validate_bundle_inner(src_dir, true)
}

fn validate_bundle_inner(src: &Path, top: bool) -> anyhow::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_str().context("non-UTF8 bundle filename")?;
        if top && name_str == "SKILL.md" {
            continue;
        }
        let ft = entry.file_type()?;
        check_bundle_entry(name_str, &ft)?;
        if ft.is_dir() {
            validate_bundle_inner(&entry.path(), false)?;
        }
    }
    Ok(())
}

/// Recursively copy every entry of `src_dir` into `dest_dir`, skipping a
/// top-level `SKILL.md` (the manifest is written separately). Re-checks each
/// entry (TOCTOU) so a symlink planted after phase-1 validation is still
/// refused before it can escape the install directory.
pub(crate) fn copy_bundle(src_dir: &Path, dest_dir: &Path) -> anyhow::Result<()> {
    copy_bundle_inner(src_dir, dest_dir, true)
}

fn copy_bundle_inner(src: &Path, dest: &Path, top: bool) -> anyhow::Result<()> {
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_str().context("non-UTF8 bundle filename")?;
        if top && name_str == "SKILL.md" {
            continue;
        }
        let ft = entry.file_type()?;
        check_bundle_entry(name_str, &ft)?;
        let src_path = entry.path();
        let dest_path = dest.join(name_str);
        if ft.is_dir() {
            fs::create_dir_all(&dest_path)?;
            copy_bundle_inner(&src_path, &dest_path, false)?;
        } else {
            if let Some(parent) = dest_path.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&src_path, &dest_path)?;
        }
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

/// Import a Claude plugin into `name`. `plugin_dir` is a local dir or a git
/// source (owner/repo, url). `plugin` selects one plugin when the source is a
/// marketplace (`.claude-plugin/marketplace.json` indexing many plugins).
pub fn cmd_addon_import(
    name: &str,
    plugin_dir: &str,
    plugin: Option<&str>,
    force: bool,
) -> Result<()> {
    let requested = plugin_dir; // original user input, for messages
    let (profile_path, mut profile) = load_profile_for_edit(name)?;
    let mur_home = crate::cmd::resolve_mur_home()?;
    let agent_skills_dir = mur_home.join("agents").join(name).join("skills");

    // Network install: when the source is a git ref (`owner/repo` shorthand or
    // a git URL) and not an existing local path, shallow-clone it into the addon
    // cache and import from there. The fetched plugin still goes through the same
    // security scan + fail-closed (disabled) install below.
    let fetched;
    let plugin_dir: &str = if std::path::Path::new(plugin_dir).exists() {
        plugin_dir
    } else if let AddonSource::Git {
        clone_url,
        cache_key,
    } = resolve_addon_source(plugin_dir)
    {
        let dest = mur_home.join("cache").join("addons").join(&cache_key);
        crate::cmd::skill_registry::git_clone_or_pull(&clone_url, &dest)?;
        println!("Fetched {clone_url} → {}", dest.display());
        fetched = dest.to_string_lossy().into_owned();
        &fetched
    } else {
        plugin_dir
    };

    // Canonicalize the plugin root (rejects a non-existent dir).
    let root = fs::canonicalize(plugin_dir)
        .map_err(|e| anyhow::anyhow!("plugin dir {plugin_dir:?}: {e}"))?;
    // Marketplace: if this source indexes multiple plugins, resolve the one
    // requested by `--plugin` (cloning its source if it lives in another repo).
    let root = resolve_marketplace(&root, plugin, requested, &mur_home)?;
    // plugin.json sits at the dir root (flat layout) or under .claude-plugin/
    // (the canonical Claude marketplace layout). Try the root first, then fall
    // back so a stock marketplace plugin dir imports without restructuring.
    // skills/ commands/ .mcp.json stay at the root in both layouts.
    let manifest_path = {
        let flat = root.join("plugin.json");
        if flat.is_file() {
            flat
        } else {
            root.join(".claude-plugin").join("plugin.json")
        }
    };
    let plugin: PluginJson = serde_json::from_str(
        &fs::read_to_string(&manifest_path)
            .map_err(|e| anyhow::anyhow!("read {}: {e}", manifest_path.display()))?,
    )?;

    let addon_id = plugin.name.clone();
    if profile.addons.iter().any(|g| g.id == addon_id) {
        bail!("add-on '{addon_id}' already imported into '{name}'; remove it first");
    }

    // ── Phase 1: Collect + validate (NO disk writes) ──────────────────────────
    // All checks must pass before a single byte is written.

    // Pending skills/commands: (dest_path, manifest)
    let mut pending_skills: Vec<(PathBuf, SkillManifest, PathBuf)> = Vec::new();
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
            // Phase-1 bundle validation: reject symlink/unsafe entries before
            // any write, so a bad bundle can't orphan a half-written skill dir.
            validate_bundle(&d)?;
            let dest = agent_skills_dir.join(&manifest.name);
            if dest.exists() {
                bail!(
                    "skill '{}' already exists for agent '{name}'; remove it first",
                    manifest.name
                );
            }
            pending_skills.push((dest, manifest, d.clone()));
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
            // Remote servers (type:"http"/"sse") carry a url, not a launch
            // command — there's no local binary to pin, so skip with a notice
            // rather than aborting the whole import.
            if srv.command.trim().is_empty() {
                println!(
                    "note: MCP '{server}' is a remote (http/sse) server; NOT imported. Wire it manually with:\n  mur agent mcp add {name} {server} ..."
                );
                continue;
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

    // ── Never-shadow gate: drop skills/commands whose name collides with a
    // global-store (builtin/registry) skill, so an import can never shadow a
    // MUR-shipped skill. The agent uses MUR's copy via store-wide injection.
    let global: std::collections::HashSet<String> =
        mur_common::skill::local::list_installed(&mur_home)
            .unwrap_or_default()
            .into_iter()
            .collect();
    let retain_non_shadow = |name: &str| -> bool {
        if global.contains(name) {
            eprintln!(
                "skill '{name}' is already provided by MUR — skipping the plugin's copy to avoid shadowing"
            );
            false
        } else {
            true
        }
    };
    pending_skills.retain(|(_, m, _)| retain_non_shadow(&m.name));
    pending_cmds.retain(|(_, m)| retain_non_shadow(&m.name));

    // Content-hash pin over the installed skill + command manifests. Computed
    // here (before Phase 2 consumes `pending_skills`/`pending_cmds` by value).
    let mut member_hashes: Vec<String> = pending_skills
        .iter()
        .map(|(_, m, _)| m)
        .chain(pending_cmds.iter().map(|(_, m)| m))
        .filter_map(|m| mur_common::skill::content_hash_for_origin(m).ok())
        .collect();
    member_hashes.sort();
    let content_hash = if member_hashes.is_empty() {
        None
    } else {
        Some(mur_common::skill::sha256_hex(
            member_hashes.join(",").as_bytes(),
        ))
    };

    // ── Phase 2: Commit (writes only after all checks passed) ─────────────────

    // 2a. Write skills.
    let mut skill_members: Vec<String> = Vec::new();
    for (dest, manifest, src_dir) in pending_skills {
        let skill_name = manifest.name.clone();
        write_to_dir(&dest, &manifest)
            .map_err(|e| anyhow::anyhow!("write skill {}: {e}", skill_name))?;
        copy_bundle(&src_dir, &dest)
            .with_context(|| format!("copying bundle for skill '{skill_name}'"))?;
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
        content_hash,
        fetch_ref: Some(requested.to_string()),
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

/// How an addon import source was classified. An *existing* local path is
/// always treated as Local by the caller (checked before this runs); for a
/// non-existent input this recognizes git URLs and `owner/repo` GitHub
/// shorthand so `mur agent addon import <agent> owner/repo` installs from the
/// network instead of only a local directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AddonSource {
    Local,
    Git {
        clone_url: String,
        cache_key: String,
    },
}

/// Filesystem-safe cache dir name derived from a git URL (scheme/`.git`/user
/// stripped, separators flattened) so the same repo maps to one cache dir
/// whether referenced by https, ssh, or shorthand.
pub fn cache_key_for(url: &str) -> String {
    let s = url
        .trim()
        .strip_suffix(".git")
        .unwrap_or(url.trim())
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .trim_start_matches("ssh://")
        .replace("git@", "")
        .replace(':', "/");
    s.split('/')
        .filter(|seg| !seg.is_empty())
        .collect::<Vec<_>>()
        .join("-")
}

pub fn resolve_addon_source(input: &str) -> AddonSource {
    let s = input.trim();
    // Explicit git URLs.
    if s.starts_with("https://")
        || s.starts_with("http://")
        || s.starts_with("ssh://")
        || s.starts_with("git@")
        || s.ends_with(".git")
    {
        return AddonSource::Git {
            clone_url: s.to_string(),
            cache_key: cache_key_for(s),
        };
    }
    // `owner/repo` GitHub shorthand: exactly two git-safe segments, and not a
    // relative/absolute/home path.
    if !s.starts_with('.')
        && !s.starts_with('/')
        && !s.starts_with('~')
        && let Some((owner, repo)) = s.split_once('/')
    {
        let safe = |p: &str| {
            !p.is_empty()
                && p.chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        };
        if safe(owner) && safe(repo) {
            let clone_url = format!("https://github.com/{owner}/{repo}.git");
            let cache_key = cache_key_for(&clone_url);
            return AddonSource::Git {
                clone_url,
                cache_key,
            };
        }
    }
    AddonSource::Local
}

/// If `root` contains a `marketplace.json` (flat or under `.claude-plugin/`),
/// pick which plugin directory to import: `--plugin <name>` resolves one
/// (cloning its source if it lives in another repo); absent `--plugin` with
/// multiple plugins and no root `plugin.json` lists them and bails; otherwise
/// (the root is itself a plugin) `root` is returned unchanged.
fn resolve_marketplace(
    root: &Path,
    plugin: Option<&str>,
    requested: &str,
    mur_home: &Path,
) -> Result<PathBuf> {
    let mp = {
        let flat = root.join("marketplace.json");
        if flat.is_file() {
            Some(flat)
        } else {
            let nested = root.join(".claude-plugin").join("marketplace.json");
            nested.is_file().then_some(nested)
        }
    };
    let Some(mp) = mp else {
        return Ok(root.to_path_buf());
    };
    let manifest = super::marketplace::parse_marketplace(&fs::read_to_string(&mp)?)?;

    if let Some(want) = plugin {
        let entry = super::marketplace::find_plugin(&manifest, want).ok_or_else(|| {
            anyhow::anyhow!(
                "plugin '{want}' not in this marketplace; available: {}",
                manifest
                    .plugins
                    .iter()
                    .map(|p| p.name.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        })?;
        let cache = mur_home.join("cache").join("addons");
        let dir = super::marketplace::resolve_plugin_dir(entry, root, &cache)?;
        return fs::canonicalize(&dir).map_err(|e| anyhow::anyhow!("plugin dir {dir:?}: {e}"));
    }

    // No --plugin: if the root is itself a plugin, import it (single-plugin
    // behavior). Otherwise it's a pure marketplace — list and ask.
    let root_is_plugin = root.join("plugin.json").is_file()
        || root.join(".claude-plugin").join("plugin.json").is_file();
    if !root_is_plugin && !manifest.plugins.is_empty() {
        println!(
            "'{requested}' is a marketplace with {} plugin(s); choose one with --plugin <name>:",
            manifest.plugins.len()
        );
        for p in &manifest.plugins {
            println!("  {}\t{}", p.name, p.description);
        }
        bail!("specify --plugin <name>");
    }
    Ok(root.to_path_buf())
}

#[cfg(test)]
mod tests {
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
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
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
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
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
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
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
        let _guard = ENV_LOCK.lock().unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        unsafe {
            std::env::set_var("MUR_HOME", home);
        }
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
}
