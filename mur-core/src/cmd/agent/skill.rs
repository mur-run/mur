//! `mur agent skill` — list / add / remove / show skills attached to an agent.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow, bail};
use mur_common::AgentProfile as _AgentProfile;

use super::{load_profile_for_edit, resolve_mur_home, save_profile};

/// Resolve a user-supplied skill query against a profile's skill list.
/// Tries exact match, then trailing path component, then trailing path component
/// without the `.md` suffix. Returns the canonical stored id on success.
fn resolve_skill_id<'a>(profile: &'a _AgentProfile, query: &str) -> Option<&'a String> {
    if let Some(s) = profile.skills.iter().find(|s| s.as_str() == query) {
        return Some(s);
    }
    if let Some(s) = profile.skills.iter().find(|s| {
        Path::new(s.as_str())
            .file_name()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f == query)
    }) {
        return Some(s);
    }
    if let Some(s) = profile.skills.iter().find(|s| {
        Path::new(s.as_str())
            .file_stem()
            .and_then(|f| f.to_str())
            .is_some_and(|f| f == query)
    }) {
        return Some(s);
    }
    None
}

/// Fail-closed validation for `profile.yaml` skill refs (#717).
///
/// Every ref must resolve to an installed, parseable skill under the agent's
/// home directory (resolution reuses the runtime loader's rules via
/// `mur_common::skill::loader::skill_ref_status`). Returns a single error
/// listing every bad ref, distinguishing *missing* (never installed) from
/// *malformed* (installed but no longer parses), so a `skills:` entry written
/// without installing the backing files can never be saved silently.
pub fn validate_skill_refs(agent_home: &Path, refs: &[String]) -> Result<()> {
    use mur_common::skill::loader::{SkillRefStatus, skill_ref_status};

    let bad: Vec<String> = refs
        .iter()
        .filter_map(|r| match skill_ref_status(agent_home, r) {
            SkillRefStatus::Loadable => None,
            SkillRefStatus::Missing { path } => Some(format!(
                "{r}: file not found at {} (referenced by profile.yaml but never installed)",
                path.display()
            )),
            SkillRefStatus::Malformed { path, error } => Some(format!(
                "{r}: {} exists but does not load as a skill ({error})",
                path.display()
            )),
        })
        .collect();
    if bad.is_empty() {
        return Ok(());
    }
    bail!(
        "refusing to write dangling skill ref(s) into profile.yaml:\n  - {}\n\
         Install each skill onto the agent first (`mur agent skill add <agent> <path>`, \
         `mur agent skill registry-add`, or `mur agent skill install-pack`), \
         or remove the ref.",
        bad.join("\n  - ")
    );
}

pub fn cmd_skill_list(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    if profile.skills.is_empty() {
        println!("(no skills attached)");
        return Ok(());
    }
    for s in &profile.skills {
        println!("{s}");
    }
    Ok(())
}

pub fn cmd_skill_add(name: &str, source: &str) -> Result<()> {
    let src = PathBuf::from(source);
    if !src.exists() {
        bail!("skill source '{source}' not found");
    }

    let text = fs::read_to_string(&src)
        .with_context(|| format!("read skill source '{}'", src.display()))?;

    // Parse the input into a canonical manifest regardless of extension:
    // `.yaml`/`.yml` via the canonical parser, anything else (`.md`, no ext)
    // via the markdown-frontmatter parser. A source that does not parse into a
    // manifest is rejected — we never copy a dead file that the loader would
    // silently ignore.
    let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let manifest = if ext == "yaml" || ext == "yml" {
        mur_common::skill::parse_canonical(&text)
            .map_err(|e| anyhow!("not a valid skill manifest: {e}. See `mur skill new`."))?
    } else {
        mur_common::skill::parse_markdown(&text)
            .map_err(|e| anyhow!("not a valid skill manifest: {e}. See `mur skill new`."))?
    };

    // Schema validation + security content scan for ALL inputs.
    mur_common::skill::validate(&manifest).map_err(|e| anyhow!("skill validation failed: {e}"))?;
    let report =
        mur_common::skill::scan::scan_skill(&manifest).map_err(|e| anyhow!("scan skill: {e}"))?;

    // The subdir name equals the manifest name; the loader/validator require a
    // single safe path component (lowercase-kebab) here.
    let skill_name = manifest.name.clone();
    if !mur_common::skill::loader::is_valid_skill_name(&skill_name) {
        bail!("refusing to install skill with unsafe name {skill_name:?}");
    }

    let (path, mut profile) = load_profile_for_edit(name)?;

    // Write into the loadable layout: agents/<agent>/skills/<name>/skill.yaml,
    // reusing the same canonical writer the runtime loader reads from.
    let agent_home = path.parent().unwrap_or(Path::new(""));
    let dest_dir = agent_home.join("skills").join(&skill_name);
    mur_common::skill::write_to_dir(&dest_dir, &manifest)
        .map_err(|e| anyhow!("write skill to {}: {e}", dest_dir.display()))?;

    if report.has_blocking_findings() {
        eprintln!("⚠ {skill_name}: security findings — review before trusting");
        for line in report.human_summary() {
            eprintln!("    {line}");
        }
    }

    let skill_id = format!("skills/{skill_name}");
    if !profile.skills.iter().any(|s| s == &skill_id) {
        profile.skills.push(skill_id);
    }
    save_profile(&path, &mut profile)
}

/// Convert a legacy path-style skill ref (`profile.skills`) into a structured
/// `installed_skills` card in place. The on-disk `skills/<name>/skill.yaml` is
/// already the loadable source of truth — installed skills read from the same
/// layout — so nothing moves on disk; only the profile pointer migrates.
pub fn cmd_skill_convert(name: &str, query: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let resolved = resolve_skill_id(&profile, query)
        .ok_or_else(|| anyhow!("legacy skill '{query}' not found on '{name}'"))?
        .clone();
    let agent_home = path.parent().unwrap_or(Path::new(""));
    convert_ref_in_profile(agent_home, &mut profile, &resolved)?;
    save_profile(&path, &mut profile)
}

/// Pure core of the legacy→installed migration: read the backing manifest and
/// move the ref from `profile.skills` into `profile.installed_skills`.
fn convert_ref_in_profile(
    agent_home: &Path,
    profile: &mut _AgentProfile,
    resolved: &str,
) -> Result<()> {
    use mur_common::agent::{SkillCardEntry, SkillCardTrigger};

    let backing = agent_home.join(resolved);
    if !backing.is_dir() {
        bail!(
            "'{resolved}' is not a loadable skill dir — cannot convert (remove + re-install instead)"
        );
    }
    let m = mur_common::skill::read_from_dir(&backing)
        .map_err(|e| anyhow!("read skill {}: {e}", backing.display()))?;

    let entry = SkillCardEntry {
        name: m.name.clone(),
        version: m.version.clone(),
        publisher: m.publisher.clone(),
        category: serde_yaml_ng::to_string(&m.category)
            .unwrap_or_default()
            .trim()
            .to_string(),
        description: m.description.clone(),
        tags: m.tags.clone(),
        triggers: m
            .triggers
            .iter()
            .map(|t| SkillCardTrigger {
                kind: serde_yaml_ng::to_string(&t.kind)
                    .unwrap_or_default()
                    .trim()
                    .to_string(),
                pattern: t.pattern.clone().unwrap_or_default(),
            })
            .collect(),
        abstract_text: m.content.r#abstract.clone(),
        transfer_chain: m.transfer_chain.clone(),
    };

    if let Some(slot) = profile
        .installed_skills
        .iter_mut()
        .find(|e| e.name == m.name)
    {
        *slot = entry;
    } else {
        profile.installed_skills.push(entry);
    }
    profile.skills.retain(|s| s != resolved);
    Ok(())
}

/// Remove a skill from every place it can live on an agent: the `skills:`
/// reference list, the `installed_skills:` card list, and the on-disk
/// `skills/<name>/` dir under `home`. Returns whether it was present anywhere.
/// Takes an explicit `home` so the doctor repair can operate on any store.
pub(crate) fn depin_skill(home: &Path, agent: &str, skill: &str) -> Result<bool> {
    let path = home.join("agents").join(agent).join("profile.yaml");
    if !path.exists() {
        bail!("agent '{agent}' not found");
    }
    let yaml = fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let mut profile: _AgentProfile =
        serde_yaml_ng::from_str(&yaml).with_context(|| format!("parse {}", path.display()))?;

    // Bare skill name, accepting `skills/foo`, `foo.yaml`, or `foo`.
    let base = Path::new(skill)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(skill)
        .to_string();

    let mut removed = false;

    // 1. skills: ref (any resolvable form)
    if let Some(resolved) = resolve_skill_id(&profile, skill).cloned() {
        profile.skills.retain(|s| s != &resolved);
        removed = true;
    }
    // 2. installed_skills: card by name
    let before = profile.installed_skills.len();
    profile.installed_skills.retain(|c| c.name != base);
    if profile.installed_skills.len() != before {
        removed = true;
    }
    if removed {
        save_profile(&path, &mut profile)?;
    }
    // 3. on-disk vendored dir (only if no surviving ref still points at it)
    let dir = home.join("agents").join(agent).join("skills").join(&base);
    let still_referenced = profile
        .skills
        .iter()
        .any(|s| Path::new(s).file_stem().and_then(|f| f.to_str()) == Some(base.as_str()));
    if dir.is_dir() && !still_referenced {
        let _ = fs::remove_dir_all(&dir);
        removed = true;
    }
    Ok(removed)
}

pub fn cmd_skill_remove(name: &str, query: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    if !depin_skill(&home, name, query)? {
        bail!("skill '{query}' not found on '{name}'");
    }
    Ok(())
}

pub fn cmd_skill_show(name: &str, query: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let resolved = resolve_skill_id(&profile, query)
        .ok_or_else(|| anyhow!("skill '{query}' not registered on '{name}'"))?;
    let agent_home = resolve_mur_home()?.join("agents").join(name);
    let backing = agent_home.join(resolved);

    if backing.is_dir() {
        // Modern per-skill subdir — read the canonical manifest the loader uses.
        let m = mur_common::skill::read_from_dir(&backing)
            .map_err(|e| anyhow!("read skill {}: {e}", backing.display()))?;
        let out = mur_common::skill::serialize_canonical(&m)?;
        print!("{out}");
        return Ok(());
    }

    // Legacy flat file.
    let ext = backing.extension().and_then(|e| e.to_str()).unwrap_or("");
    if matches!(ext, "yaml" | "yml") {
        let text = std::fs::read_to_string(&backing)?;
        let m = mur_common::skill::parse_canonical(&text)?;
        let out = mur_common::skill::serialize_canonical(&m)?;
        print!("{out}");
    } else {
        // Legacy .md — print raw
        let body = std::fs::read_to_string(&backing)?;
        print!("{body}");
    }
    Ok(())
}

/// Enable/disable a skill for an agent by editing the per-agent denylist.
/// Non-destructive. `skill_id` accepts the full id (`skills/foo`), a basename
/// (`foo.yaml`), or the bare manifest name (`foo`).
pub fn cmd_skill_set_enabled(name: &str, skill_id: &str, enabled: bool) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let skill_name = skill_id
        .rsplit('/')
        .next()
        .unwrap_or(skill_id)
        .trim_end_matches(".yaml")
        .trim_end_matches(".yml")
        .trim_end_matches(".md");
    profile.set_skill_enabled(skill_name, enabled);
    save_profile(&path, &mut profile)?;
    println!(
        "{} skill '{skill_name}' for '{name}' (restart the agent to apply)",
        if enabled { "Enabled" } else { "Disabled" }
    );
    Ok(())
}

#[cfg(test)]
mod skill_ref_validation_tests {
    use super::*;

    fn valid_manifest(name: &str) -> mur_common::skill::SkillManifest {
        mur_common::skill::parse_canonical(&format!(
            r#"name: {name}
version: 1.0.0
publisher: human:t
description: test skill for ref validation
category: context
content:
  abstract: hi
  context: body
"#
        ))
        .unwrap()
    }

    #[test]
    fn validate_skill_refs_distinguishes_missing_from_malformed() {
        let home = tempfile::tempdir().unwrap();

        // Missing: nothing installed at the ref.
        let err = validate_skill_refs(home.path(), &["skills/executing-plans".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("file not found"), "got: {err}");
        assert!(err.contains("skills/executing-plans"), "got: {err}");

        // Malformed: file exists but is garbage.
        let bad = home.path().join("skills").join("broken");
        fs::create_dir_all(&bad).unwrap();
        fs::write(bad.join("skill.yaml"), "{{{ not yaml").unwrap();
        let err = validate_skill_refs(home.path(), &["skills/broken".into()])
            .unwrap_err()
            .to_string();
        assert!(err.contains("does not load as a skill"), "got: {err}");

        // Loadable: properly installed skill passes.
        mur_common::skill::write_to_dir(
            &home.path().join("skills").join("ok"),
            &valid_manifest("ok"),
        )
        .unwrap();
        validate_skill_refs(home.path(), &["skills/ok".into()]).unwrap();
    }

    #[test]
    fn save_profile_rejects_newly_added_dangling_ref() {
        let agent_dir = tempfile::tempdir().unwrap();
        let path = agent_dir.path().join("profile.yaml");
        let mut profile = mur_common::AgentProfile::default_for_tests();

        // Baseline save with no skill refs succeeds.
        save_profile(&path, &mut profile).unwrap();

        // Adding a ref with no installed files must fail-closed (#717) …
        profile.skills.push("skills/ghost".into());
        let err = save_profile(&path, &mut profile).unwrap_err().to_string();
        assert!(err.contains("skills/ghost"), "got: {err}");
        assert!(err.contains("file not found"), "got: {err}");
        // … and the dangling ref must not have been persisted.
        let on_disk: mur_common::AgentProfile =
            serde_yaml_ng::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert!(on_disk.skills.is_empty());

        // Installing the files first makes the same save succeed.
        mur_common::skill::write_to_dir(
            &agent_dir.path().join("skills").join("ghost"),
            &valid_manifest("ghost"),
        )
        .unwrap();
        save_profile(&path, &mut profile).unwrap();
        let on_disk: mur_common::AgentProfile =
            serde_yaml_ng::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(on_disk.skills, vec!["skills/ghost".to_string()]);
    }

    #[test]
    fn save_profile_tolerates_preexisting_dangling_ref_on_unrelated_edit() {
        let agent_dir = tempfile::tempdir().unwrap();
        let path = agent_dir.path().join("profile.yaml");

        // Simulate the incident: a profile already on disk with a dangling
        // ref (written by an external tool, bypassing save_profile).
        let mut profile = mur_common::AgentProfile::default_for_tests();
        profile.skills.push("skills/executing-plans".into());
        fs::write(&path, serde_yaml_ng::to_string(&profile).unwrap()).unwrap();

        // An unrelated edit must still save — only *newly added* refs gate.
        profile.display_name = "Renamed".into();
        save_profile(&path, &mut profile).unwrap();
    }

    #[test]
    fn convert_moves_ref_from_legacy_to_installed() {
        let home = tempfile::tempdir().unwrap();
        mur_common::skill::write_to_dir(
            &home.path().join("skills").join("ok"),
            &valid_manifest("ok"),
        )
        .unwrap();

        let mut profile = mur_common::AgentProfile::default_for_tests();
        profile.skills = vec!["skills/ok".into()];
        assert!(profile.installed_skills.is_empty());

        convert_ref_in_profile(home.path(), &mut profile, "skills/ok").unwrap();

        assert!(profile.skills.is_empty(), "legacy ref should be removed");
        assert_eq!(profile.installed_skills.len(), 1);
        assert_eq!(profile.installed_skills[0].name, "ok");

        // Idempotent: converting again upserts, never duplicates.
        profile.skills = vec!["skills/ok".into()];
        convert_ref_in_profile(home.path(), &mut profile, "skills/ok").unwrap();
        assert_eq!(profile.installed_skills.len(), 1);
    }

    #[test]
    fn convert_rejects_missing_backing_dir() {
        let home = tempfile::tempdir().unwrap();
        let mut profile = mur_common::AgentProfile::default_for_tests();
        profile.skills = vec!["skills/ghost".into()];
        assert!(convert_ref_in_profile(home.path(), &mut profile, "skills/ghost").is_err());
    }

    fn write_profile(home: &std::path::Path, agent: &str, body: &str) -> std::path::PathBuf {
        let dir = home.join("agents").join(agent);
        std::fs::create_dir_all(dir.join("skills")).unwrap();
        let path = dir.join("profile.yaml");
        std::fs::write(&path, body).unwrap();
        path
    }

    fn write_agent_skill_dir(home: &std::path::Path, agent: &str, name: &str) {
        let d = home.join("agents").join(agent).join("skills").join(name);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("skill.yaml"),
            format!(
                "name: {name}\nversion: 1.0.0\npublisher: human:t\ndescription: d\ncategory: context\ncontent:\n  abstract: a\n  context: b\n"
            ),
        )
        .unwrap();
    }

    // A minimal profile carrying `name` as an installed_skills card + on-disk dir,
    // but NOT as a skills: ref (the exact concierge shadow shape).
    const CARD_ONLY_PROFILE: &str = "\
schema: 1
id: 0192f5a1-28ab-7111-8000-000000000099
name: mur
display_name: MUR
version: \"0.1.0\"
persona:
  category: research
  description: test profile for depin_skill tests
  traits: { tone: concise, risk: cautious, verbosity: low }
sys_prompt_file: \"sys_prompt.md\"
model: { provider: ollama, name: \"m\", params: {} }
mcp_servers: []
skills:
  - skills/concierge
installed_skills:
- name: mur-compress
  version: 1.0.0
  publisher: human:mur
  description: d
  category: context
  abstract: a
transport: { stdio: true, socket: { enabled: true, bind: \"unix:///tmp/a.sock\" } }
communication: { accepts_from: [\"*\"], sends_to: [] }
capabilities: [\"a2a.message.send\",\"a2a.tasks\"]
entitlements:
  network:
    inbound: { ports: [] }
    outbound: { mode: restricted, allow_hosts: [], protocols: [\"tcp\"], resolve_dns: { mode: system } }
  filesystem: { read: [], write: [], deny: [\"~/.ssh\"] }
  processes: { spawn: { mode: allowlist, allowed: [] } }
  syscalls: { mode: default }
  limits: { memory_mb: 512, file_descriptors: 1024, processes: 32 }
notifications: { on_task_complete: [], on_error: [], on_shutdown: [] }
retry:
  llm: { max_retries: 3, backoff: exponential, initial_delay_ms: 1000, max_delay_ms: 30000, retry_on: [\"rate_limit\"] }
  tool: { max_retries: 1, backoff: fixed, initial_delay_ms: 500 }
lifecycle: { restart: on_failure, max_restarts: 3, restart_window_secs: 600, stop_timeout_secs: 15, mcp_required: true }
created_at: \"2026-04-22T10:00:00+08:00\"
updated_at: \"2026-04-22T10:00:00+08:00\"
";

    #[test]
    fn depin_removes_card_and_dir_when_not_a_ref() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_profile(home, "mur", CARD_ONLY_PROFILE);
        write_agent_skill_dir(home, "mur", "mur-compress");
        write_agent_skill_dir(home, "mur", "concierge");

        let removed = depin_skill(home, "mur", "mur-compress").unwrap();
        assert!(removed, "should report removal");

        // card gone
        let yaml = std::fs::read_to_string(home.join("agents/mur/profile.yaml")).unwrap();
        let p: _AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(p.installed_skills.iter().all(|c| c.name != "mur-compress"));
        // dir gone
        assert!(!home.join("agents/mur/skills/mur-compress").exists());
        // untouched skill dir preserved
        assert!(home.join("agents/mur/skills/concierge").exists());
    }

    #[test]
    fn depin_returns_false_when_absent() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        write_profile(home, "mur", CARD_ONLY_PROFILE);
        let removed = depin_skill(home, "mur", "does-not-exist").unwrap();
        assert!(!removed);
    }

    #[test]
    fn depin_removes_ref_form_too() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // concierge is a skills: ref + on-disk dir
        write_profile(home, "mur", CARD_ONLY_PROFILE);
        write_agent_skill_dir(home, "mur", "concierge");
        let removed = depin_skill(home, "mur", "concierge").unwrap();
        assert!(removed);
        let yaml = std::fs::read_to_string(home.join("agents/mur/profile.yaml")).unwrap();
        let p: _AgentProfile = serde_yaml_ng::from_str(&yaml).unwrap();
        assert!(!p.skills.iter().any(|s| s == "skills/concierge"));
        assert!(!home.join("agents/mur/skills/concierge").exists());
    }
}
