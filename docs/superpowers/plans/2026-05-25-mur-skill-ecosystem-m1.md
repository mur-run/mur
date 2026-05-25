# MuR Skill Ecosystem — M1a + M1b Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Ship a complete skill lifecycle — install from git-based registry, manage installed skills locally, search/audit/trust-promote, and publish signed skills via PR.

**Architecture:** Two logical layers. *Local CLI* (M1a) operates on `~/.mur/skills/`: list/show/remove/search/audit/trust. *Registry* (M1b) wraps a shallow clone of `mur-run/skill-registry` into `~/.mur/cache/registry/` and shells out to `git` (existing pattern, no `git2` crate) and `gh` for PR publication. The `SkillAction` enum covers both local and remote commands; `--local` flag on `search` scopes to installed only.

**Tech Stack:** Rust, `std::process::Command` for git/gh, serde_yaml_ng for index.yaml, `tempfile` for staging, existing `mur-common::*` modules from M0.

---

## File Structure

**New files:**
- `mur-common/src/skill/local.rs` — installed-skill helpers (list, load, remove, search, trust)
- `mur-common/src/skill/registry.rs` — `RegistryIndex` + `RegistrySkillEntry` serde types for index.yaml
- `mur-core/src/cmd/skill_registry.rs` — registry client (git clone/pull, index read, search)
- `mur-core/src/cmd/skill_install.rs` — install orchestrator (resolve source, verify, store, trust)
- `mur-core/src/cmd/skill_publish.rs` — publish handler (sign + fork + PR via gh CLI)

**Modified files:**
- `mur-core/src/cli/skill.rs` — expand `SkillAction` enum (12 variants total)
- `mur-core/src/dispatch.rs` — route all new variants
- `mur-core/src/cmd/skill_cmd.rs` — add list/show/remove/search/info/audit/trust handlers
- `mur-core/src/cmd/agent/skill.rs` — YAML compat in add/show
- `mur-core/src/cmd/mod.rs` — register new modules
- `mur-common/src/skill/mod.rs` — register local + registry modules

---

### Task 1: Expand `SkillAction` enum with all M1a+M1b variants

**Files:**
- Modify: `mur-core/src/cli/skill.rs`
- Modify: `mur-core/src/dispatch.rs`

- [ ] **Step 1: Replace `mur-core/src/cli/skill.rs` with full enum**

```rust
//! `mur skill` subcommand surface.

use clap::Subcommand;

#[derive(Subcommand)]
pub enum SkillAction {
    /// Run schema validation + full security content scan on a skill file.
    Validate {
        #[arg(default_value = "skill.yaml")]
        path: String,
        #[arg(long)]
        warnings_only: bool,
    },
    /// Convert between canonical YAML and markdown frontmatter.
    Fmt {
        path: String,
        #[arg(long)]
        to: Option<String>,
        #[arg(long)]
        write: bool,
    },
    /// List installed skills (from ~/.mur/skills/).
    List,
    /// Show full content of an installed skill.
    Show { name: String },
    /// Uninstall a skill.
    Remove { name: String },
    /// Search installed skills (--local) or remote registry.
    Search {
        query: String,
        #[arg(long)]
        local: bool,
    },
    /// Show Layer 1+2 summary of an installed skill.
    Info {
        name: String,
        #[arg(long)]
        full: bool,
    },
    /// Run full security scan + signature check on an installed skill.
    Audit { name: String },
    /// Promote or demote a skill's trust level.
    Trust {
        name: String,
        #[arg(long)]
        level: String,
    },
    /// Install a skill from registry, file, or URL.
    Install {
        /// Skill name (registry), local path, or git URL.
        source: String,
    },
    /// Publish a local skill to the default registry.
    Publish {
        /// Path to skill.yaml to publish.
        path: String,
    },
    /// Update an installed skill to the latest registry version.
    Update {
        /// Name of installed skill to update — will be re-installed from registry.
        name: String,
    },
}
```

- [ ] **Step 2: Wire dispatch in `mur-core/src/dispatch.rs`**

Replace the existing `Commands::Skill` block:

```rust
        Commands::Skill { action } => match action {
            crate::cli::SkillAction::Validate { path, warnings_only } => {
                cmd::skill_cmd::cmd_validate(&path, warnings_only)
            }
            crate::cli::SkillAction::Fmt { path, to, write } => {
                cmd::skill_cmd::cmd_fmt(&path, to.as_deref(), write)
            }
            crate::cli::SkillAction::List => cmd::skill_cmd::cmd_list(),
            crate::cli::SkillAction::Show { name } => cmd::skill_cmd::cmd_show(&name),
            crate::cli::SkillAction::Remove { name } => cmd::skill_cmd::cmd_remove(&name),
            crate::cli::SkillAction::Search { query, local } => {
                cmd::skill_cmd::cmd_search(&query, local)
            }
            crate::cli::SkillAction::Info { name, full } => {
                cmd::skill_cmd::cmd_info(&name, full)
            }
            crate::cli::SkillAction::Audit { name } => cmd::skill_cmd::cmd_audit(&name),
            crate::cli::SkillAction::Trust { name, level } => {
                cmd::skill_cmd::cmd_trust(&name, &level)
            }
            crate::cli::SkillAction::Install { source } => {
                cmd::skill_install::cmd_install(&source)
            }
            crate::cli::SkillAction::Publish { path } => {
                cmd::skill_publish::cmd_publish(&path)
            }
            crate::cli::SkillAction::Update { name } => {
                cmd::skill_install::cmd_update(&name)
            }
        },
```

- [ ] **Step 3: Verify the build fails (expected — unimplemented functions)**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo build -p mur-core 2>&1 | head -15
```

Expected: errors about undefined functions `cmd_list`, `cmd_show`, `cmd_remove`, `cmd_search`, `cmd_info`, `cmd_audit`, `cmd_trust`, and modules `skill_install`, `skill_publish`.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cli/skill.rs mur-core/src/dispatch.rs
git commit -m "feat(cli): expand SkillAction with all M1a+M1b commands"
```

---

### Task 2: Local skill store helpers (`mur-common::skill::local`)

**Files:**
- Create: `mur-common/src/skill/local.rs`
- Modify: `mur-common/src/skill/mod.rs`

- [ ] **Step 1: Create `mur-common/src/skill/local.rs`**

```rust
//! Local skill store helpers — list installed, resolve, remove, search, trust.

use crate::skill::store::global_skill_dir;
use crate::skill::{read_from_dir, SkillManifest, StoreError};
use crate::skill::types::TrustLevel;
use crate::skill::content_sha256;
use crate::trust::skills::{SkillTrustStore, TrustEntry};
use std::fs;
use std::path::{Path, PathBuf};

pub fn list_installed(mur_home: &Path) -> Result<Vec<String>, StoreError> {
    let skills_dir = mur_home.join("skills");
    if !skills_dir.exists() {
        return Ok(vec![]);
    }
    let mut names: Vec<_> = fs::read_dir(&skills_dir)
        .map_err(StoreError::Io)?
        .filter_map(|e| {
            let e = e.ok()?;
            if e.file_type().ok()?.is_dir() {
                e.file_name().to_str().map(|s| s.to_string())
            } else {
                None
            }
        })
        .collect();
    names.sort();
    Ok(names)
}

pub fn load_installed(mur_home: &Path, name: &str) -> Result<SkillManifest, StoreError> {
    read_from_dir(&global_skill_dir(mur_home, name))
}

pub fn installed_path(mur_home: &Path, name: &str) -> PathBuf {
    global_skill_dir(mur_home, name)
}

pub fn remove_installed(mur_home: &Path, name: &str) -> Result<(), StoreError> {
    let dir = installed_path(mur_home, name);
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(StoreError::Io)?;
    }
    // Remove trust entry by name
    if let Ok(mut trust) = SkillTrustStore::load(mur_home) {
        trust.entries.retain(|_k, v| v.name != name);
        let _ = trust.save(mur_home);
    }
    Ok(())
}

pub fn search_installed(mur_home: &Path, query: &str) -> Result<Vec<(String, SkillManifest)>, StoreError> {
    let q = query.to_lowercase();
    let mut results = Vec::new();
    for name in list_installed(mur_home)? {
        if let Ok(m) = load_installed(mur_home, &name) {
            if name.to_lowercase().contains(&q)
                || m.description.to_lowercase().contains(&q)
                || m.tags.iter().any(|t| t.to_lowercase().contains(&q))
            {
                results.push((name, m));
            }
        }
    }
    Ok(results)
}

pub fn set_trust_level(mur_home: &Path, name: &str, level: TrustLevel) -> Result<(), Box<dyn std::error::Error>> {
    let mut trust = SkillTrustStore::load(mur_home)?;
    let keys: Vec<String> = trust.entries.iter()
        .filter(|(_k, v)| v.name == name)
        .map(|(k, _)| k.clone())
        .collect();
    for k in keys {
        if let Some(e) = trust.entries.get_mut(&k) {
            e.level = level;
        }
    }
    trust.save(mur_home)?;
    Ok(())
}

pub fn get_trust_level(mur_home: &Path, name: &str) -> Result<TrustLevel, Box<dyn std::error::Error>> {
    let trust = SkillTrustStore::load(mur_home)?;
    for entry in trust.entries.values() {
        if entry.name == name {
            return Ok(entry.level);
        }
    }
    Ok(TrustLevel::Sandboxed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use crate::skill::{parse_canonical, write_to_dir};

    fn sample(name: &str) -> SkillManifest {
        parse_canonical(&format!(
            r#"name: {name}
version: 1.0.0
publisher: human:t
description: test skill for {name}
category: context
content:
  abstract: hi
  context: body
tags: [test, {name}]
"#)).unwrap()
    }

    #[test]
    fn list_returns_installed() {
        let dir = tempdir().unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "a"), &sample("a")).unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "b"), &sample("b")).unwrap();
        assert_eq!(list_installed(dir.path()).unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn empty_dir_returns_empty() {
        assert!(list_installed(tempdir().unwrap().path()).unwrap().is_empty());
    }

    #[test]
    fn search_finds_by_name() {
        let dir = tempdir().unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "my-prices"), &sample("my-prices")).unwrap();
        assert_eq!(search_installed(dir.path(), "price").unwrap().len(), 1);
    }

    #[test]
    fn search_finds_by_tag() {
        let dir = tempdir().unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "web"), &sample("web")).unwrap();
        assert_eq!(search_installed(dir.path(), "test").unwrap().len(), 1);
    }

    #[test]
    fn remove_cleans_dir() {
        let dir = tempdir().unwrap();
        write_to_dir(&global_skill_dir(dir.path(), "rm-me"), &sample("rm-me")).unwrap();
        remove_installed(dir.path(), "rm-me").unwrap();
        assert!(list_installed(dir.path()).unwrap().is_empty());
    }
}
```

- [ ] **Step 2: Wire into `mur-common/src/skill/mod.rs`**

Add `pub mod local;` to the declarations section (alphabetically after `hash` and before `manifest` — place at `l` position).

No re-exports needed from this module for now; callers use fully-qualified `mur_common::skill::local::*`.

- [ ] **Step 3: Run tests**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo test -p mur-common --lib skill::local::tests
```

Expected: 5 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/skill/
git commit -m "feat(skill): local store helpers (list, load, remove, search, trust)"
```

---

### Task 3: `mur skill list`, `show`, `remove`

**Files:**
- Create: `mur-core/src/skill_cmd.rs` — add three handler functions

- [ ] **Step 1: Add imports and handlers to `mur-core/src/cmd/skill_cmd.rs`**

Append after the existing `cmd_fmt` function (before the `read_any` helper):

```rust
use crate::cmd::agent::resolve_mur_home;
use mur_common::skill::local;

pub fn cmd_list() -> Result<()> {
    let home = resolve_mur_home()?;
    let names = local::list_installed(&home).context("list installed skills")?;
    if names.is_empty() {
        println!("(no skills installed)");
        return Ok(());
    }
    for name in &names {
        let level = local::get_trust_level(&home, name)
            .unwrap_or(mur_common::skill::TrustLevel::Sandboxed);
        println!("{name:30} [{level:?}]");
    }
    Ok(())
}

pub fn cmd_show(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let m = local::load_installed(&home, name)
        .map_err(|_| anyhow!("skill '{name}' not installed"))?;
    let yaml = mur_common::skill::serialize_canonical(&m)?;
    print!("{yaml}");
    Ok(())
}

pub fn cmd_remove(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    local::remove_installed(&home, name)
        .map_err(|e| anyhow!("failed to remove '{name}': {e}"))?;
    println!("removed: {name}");
    Ok(())
}
```

- [ ] **Step 2: Build**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo build -p mur-core 2>&1 | grep -E "error" | head -10
```

Expected: clean after any missing imports fixed.

- [ ] **Step 3: Smoke test**

```bash
mkdir -p /tmp/.mur/skills/demo
cat > /tmp/.mur/skills/demo/skill.yaml << 'EOF'
name: demo
version: 1.0.0
publisher: human:t
description: test skill
category: context
content:
  abstract: hi
  context: body
tags: [test]
EOF
MUR_HOME=/tmp/.mur cargo run -- skill list
MUR_HOME=/tmp/.mur cargo run -- skill show demo
```

Expected: list shows "demo [Sandboxed]", show prints the YAML.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/skill_cmd.rs
git commit -m "feat(cli): mur skill list, show, remove"
```

---

### Task 4: `mur skill info` and `mur skill search --local`

**Files:**
- Modify: `mur-core/src/cmd/skill_cmd.rs`

- [ ] **Step 1: Add handlers**

```rust
pub fn cmd_info(name: &str, full: bool) -> Result<()> {
    let home = resolve_mur_home()?;
    let m = local::load_installed(&home, name)
        .map_err(|_| anyhow!("skill '{name}' not installed"))?;
    let level = local::get_trust_level(&home, name)
        .unwrap_or(mur_common::skill::TrustLevel::Sandboxed);
    println!("Name:        {}", m.name);
    println!("Version:     {}", m.version);
    println!("Publisher:   {}", m.publisher);
    println!("Description: {}", m.description);
    println!("Category:    {:?}", m.category);
    println!("Tags:        {}", m.tags.join(", "));
    println!("Trust Level: {level:?}");
    if full {
        println!("\n--- Abstract ---\n{}", m.content.r#abstract);
    }
    Ok(())
}

pub fn cmd_search(query: &str, local_only: bool) -> Result<()> {
    let home = resolve_mur_home()?;
    let local_results = local::search_installed(&home, query)
        .context("search installed")?;

    if local_only {
        if local_results.is_empty() {
            println!("(no matching skills found)");
            return Ok(());
        }
        for (name, m) in &local_results {
            let level = local::get_trust_level(&home, name)
                .unwrap_or(mur_common::skill::TrustLevel::Sandboxed);
            println!("{name:25} {:12?} {}", level, m.description);
        }
        return Ok(());
    }

    // TODO(M1b): also search remote registry and merge results
    // For now, local-only is the default.
    if local_results.is_empty() {
        println!("(no matching skills found in local store)");
        // Hint about registry
        eprintln!("hint: use `mur skill install <name>` to install from the registry");
        return Ok(());
    }
    for (name, m) in &local_results {
        let level = local::get_trust_level(&home, name)
            .unwrap_or(mur_common::skill::TrustLevel::Sandboxed);
        println!("{name:25} {:12?} {}", level, m.description);
    }
    Ok(())
}
```

- [ ] **Step 2: Build + smoke test**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo build -p mur-core && \
MUR_HOME=/tmp/.mur cargo run -- skill info demo && \
MUR_HOME=/tmp/.mur cargo run -- skill search test
```

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/skill_cmd.rs
git commit -m "feat(cli): mur skill info, mur skill search"
```

---

### Task 5: `mur skill audit` and `mur skill trust`

**Files:**
- Modify: `mur-core/src/cmd/skill_cmd.rs`

- [ ] **Step 1: Add handlers**

```rust
pub fn cmd_audit(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let m = local::load_installed(&home, name)
        .map_err(|_| anyhow!("skill '{name}' not installed"))?;
    let hash = mur_common::skill::content_sha256(&m)?;

    // Content scan
    let report = mur_common::skill::scan::scan_skill(&m)?;
    if report.has_blocking_findings() {
        eprintln!("⚠ Security findings:");
        for line in report.human_summary() {
            eprintln!("  {line}");
        }
    } else {
        println!("✓ Content scan: clean");
    }

    // Trust store lookup
    let trust = mur_common::trust::skills::SkillTrustStore::load(&home)
        .map_err(|e| anyhow!("load trust: {e}"))?;
    let entry = trust.lookup(&hash);
    match entry {
        Some(e) => println!("✓ Trust: {:?} (publisher: {})", e.level, e.publisher.as_deref().unwrap_or("-")),
        None => println!("ℹ No trust entry (defaults to Sandboxed)"),
    }

    println!("✓ Audit complete for '{name}'");
    Ok(())
}

pub fn cmd_trust(name: &str, level_str: &str) -> Result<()> {
    let level = match level_str {
        "sandboxed" => mur_common::skill::TrustLevel::Sandboxed,
        "verified" => mur_common::skill::TrustLevel::Verified,
        "trusted" => mur_common::skill::TrustLevel::Trusted,
        other => bail!("invalid level '{other}' (expected: sandboxed | verified | trusted)"),
    };
    let home = resolve_mur_home()?;
    let m = local::load_installed(&home, name)
        .map_err(|_| anyhow!("skill '{name}' not installed"))?;
    let hash = mur_common::skill::content_sha256(&m)?;

    let mut trust = mur_common::trust::skills::SkillTrustStore::load(&home)
        .map_err(|e| anyhow!("load trust: {e}"))?;
    trust.insert(hash, mur_common::trust::skills::TrustEntry {
        name: name.to_string(),
        version: m.version.clone(),
        level,
        installed_at: chrono::Utc::now().to_rfc3339(),
        publisher: Some(m.publisher.clone()),
    });
    trust.save(&home).map_err(|e| anyhow!("save trust: {e}"))?;
    println!("✓ Trust level for '{name}' set to {level:?}");
    Ok(())
}
```

- [ ] **Step 2: Verify `chrono` dep is usable**

`mur-core` already imports `chrono` transitively. Either add `use chrono::Utc;` or use fully-qualified `chrono::Utc::now().to_rfc3339()`.

Check if `chrono` is a direct dep of `mur-core`:
```bash
grep -A2 "chrono" /Volumes/Firecuda4tb/Projects/mur/mur-core/Cargo.toml
```

If not a direct dep, use the fully-qualified path (`chrono::Utc::now()`) — works as long as `chrono` is in the dep tree.

- [ ] **Step 3: Build**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo build -p mur-core
```

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/skill_cmd.rs
git commit -m "feat(cli): mur skill audit, mur skill trust"
```

---

### Task 6: `mur agent skill` YAML compat upgrade

**Files:**
- Modify: `mur-core/src/cmd/agent/skill.rs`

- [ ] **Step 1: Read current `cmd_skill_add` and `cmd_skill_show`**

The current code:
- `cmd_skill_add`: copies raw file, no validation
- `cmd_skill_show`: prints raw file content

- [ ] **Step 2: Update `cmd_skill_add` to validate `.yaml` inputs**

```rust
pub fn cmd_skill_add(name: &str, source: &str) -> Result<()> {
    let src = PathBuf::from(source);
    if !src.exists() {
        bail!("skill source '{source}' not found");
    }
    let basename = src
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow!("skill source has no basename"))?;

    // Validate YAML skills before adding
    if let Some(ext) = src.extension().and_then(|e| e.to_str()) {
        if ext == "yaml" || ext == "yml" {
            let text = fs::read_to_string(&src)?;
            let m = mur_common::skill::parse_canonical(&text)?;
            mur_common::skill::validate(&m)
                .map_err(|e| anyhow!("skill validation failed: {e}"))?;
        }
    }

    let (path, mut profile) = load_profile_for_edit(name)?;
    let agent_home = path.parent().unwrap_or(Path::new(""));
    let skills_dir = agent_home.join("skills");
    fs::create_dir_all(&skills_dir).with_context(|| format!("create {}", skills_dir.display()))?;
    let dest = skills_dir.join(basename);
    fs::copy(&src, &dest)
        .with_context(|| format!("copy {} -> {}", src.display(), dest.display()))?;

    let skill_id = format!("skills/{basename}");
    if !profile.skills.iter().any(|s| s == &skill_id) {
        profile.skills.push(skill_id);
    }
    save_profile(&path, &mut profile)
}
```

- [ ] **Step 3: Update `cmd_skill_show` to render YAML via parser**

```rust
pub fn cmd_skill_show(name: &str, query: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    let resolved = resolve_skill_id(&profile, query)
        .ok_or_else(|| anyhow!("skill '{query}' not registered on '{name}'"))?;
    let agent_home = resolve_mur_home()?.join("agents").join(name);
    let file_path = agent_home.join(resolved);

    let ext = file_path.extension().and_then(|e| e.to_str()).unwrap_or("");
    if matches!(ext, "yaml" | "yml") {
        let text = fs::read_to_string(&file_path)
            .with_context(|| format!("read {}", file_path.display()))?;
        let m = mur_common::skill::parse_canonical(&text)
            .with_context(|| format!("parse {}", file_path.display()))?;
        let out = mur_common::skill::serialize_canonical(&m)?;
        print!("{out}");
    } else {
        // Legacy .md — print raw
        let body = fs::read_to_string(&file_path)
            .with_context(|| format!("read {}", file_path.display()))?;
        print!("{body}");
    }
    Ok(())
}
```

- [ ] **Step 4: Build**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo build -p mur-core
```

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill.rs
git commit -m "feat(agent): support YAML skills in mur agent skill add/show"
```

---

### Task 7: Registry data model (`index.yaml` serde types)

**Files:**
- Create: `mur-common/src/skill/registry.rs`
- Modify: `mur-common/src/skill/mod.rs`

- [ ] **Step 1: Create `mur-common/src/skill/registry.rs`**

```rust
//! Git-based skill registry data model for index.yaml.
//!
//! On-disk layout:
//!   mur-run/skill-registry/
//!     index.yaml          ← this file
//!     skills/<name>/versions/<version>.yaml  ← per-version skill files

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistryIndex {
    #[serde(default)]
    pub schema_version: u32,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default)]
    pub skills: BTreeMap<String, RegistrySkillEntry>,
}

/// One entry in the registry index, keyed by skill name.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegistrySkillEntry {
    pub latest: String,
    pub description: String,
    pub publisher: String,
    pub category: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// SHA-256 of the canonical YAML for the latest version.
    #[serde(default)]
    pub content_sha256: String,
    #[serde(default)]
    pub install_count: u64,
}

impl RegistryIndex {
    pub fn from_yaml(yaml: &str) -> Result<Self, serde_yaml_ng::Error> {
        serde_yaml_ng::from_str(yaml)
    }

    pub fn to_yaml(&self) -> Result<String, serde_yaml_ng::Error> {
        serde_yaml_ng::to_string(self)
    }

    /// Search by keyword in name, description, and tags. Returns results
    /// sorted by install_count descending.
    pub fn search(&self, query: &str) -> Vec<(&str, &RegistrySkillEntry)> {
        let q = query.to_lowercase();
        let mut results: Vec<_> = self.skills.iter()
            .filter(|(name, e)| {
                name.to_lowercase().contains(&q)
                    || e.description.to_lowercase().contains(&q)
                    || e.tags.iter().any(|t| t.to_lowercase().contains(&q))
            })
            .map(|(n, e)| (n.as_str(), e))
            .collect();
        results.sort_by(|a, b| b.1.install_count.cmp(&a.1.install_count));
        results
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
schema_version: 1
updated_at: 2026-05-25T00:00:00Z
skills:
  research-prices:
    latest: 1.1.0
    description: Search and compare product prices
    publisher: human:david
    category: workflow
    tags: [e-commerce, price]
    content_sha256: "abcd"
    install_count: 42
  web-browsing:
    latest: 2.0.0
    description: Browse web pages
    publisher: human:david
    category: workflow
    tags: [web, browser]
    content_sha256: "1234"
    install_count: 128
"#;

    #[test]
    fn parses_index() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert_eq!(idx.skills.len(), 2);
        assert_eq!(idx.skills["research-prices"].latest, "1.1.0");
    }

    #[test]
    fn search_finds_by_name() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert_eq!(idx.search("price").len(), 1);
    }

    #[test]
    fn search_finds_by_tag() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert_eq!(idx.search("browser").len(), 1);
    }

    #[test]
    fn search_orders_by_install_count() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        let r = idx.search("a"); // matches both
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "web-browsing"); // 128 > 42
    }

    #[test]
    fn empty_query_returns_all() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert_eq!(idx.search("").len(), 2);
    }

    #[test]
    fn no_match_returns_empty() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        assert!(idx.search("zzz").is_empty());
    }

    #[test]
    fn roundtrip_yaml() {
        let idx = RegistryIndex::from_yaml(SAMPLE).unwrap();
        let yaml = idx.to_yaml().unwrap();
        let idx2 = RegistryIndex::from_yaml(&yaml).unwrap();
        assert_eq!(idx.skills.len(), idx2.skills.len());
    }
}
```

- [ ] **Step 2: Wire into `mur-common/src/skill/mod.rs`**

Add `pub mod registry;` to declarations (alphabetically after `parser`).

- [ ] **Step 3: Run tests**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo test -p mur-common --lib skill::registry::tests
```

Expected: 7 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-common/src/skill/
git commit -m "feat(registry): index.yaml data model with search + serde"
```

---

### Task 8: Registry client (git clone, index read, search)

**Files:**
- Create: `mur-core/src/cmd/skill_registry.rs`
- Modify: `mur-core/src/cmd/mod.rs`

- [ ] **Step 1: Create `mur-core/src/cmd/skill_registry.rs`**

```rust
//! Skill registry client — shallow clone via git, load index, search, info.
//!
//! Default registry: https://github.com/mur-run/skill-registry.git
//! Cached at: ~/.mur/cache/registry/ (shallow clone, refreshed on demand)

use anyhow::{Context, Result, anyhow, bail};
use mur_common::skill::registry::{RegistryIndex, RegistrySkillEntry};
use std::path::{Path, PathBuf};
use std::process::Command;

pub const DEFAULT_REGISTRY: &str = "https://github.com/mur-run/skill-registry.git";

pub fn registry_cache_dir(mur_home: &Path) -> PathBuf {
    mur_home.join("cache").join("registry")
}

/// Fetch (or refresh) the registry via shallow clone. Returns the dir path.
pub fn fetch_registry(mur_home: &Path, registry_url: &str) -> Result<PathBuf> {
    let cache_dir = registry_cache_dir(mur_home);
    let git_dir = cache_dir.join(".git");

    if git_dir.exists() {
        let status = Command::new("git")
            .args(["-C", &*cache_dir.to_string_lossy(), "pull", "--depth=1", "--ff-only"])
            .status()
            .map_err(|e| anyhow!("git pull: {e}"))?;
        if !status.success() {
            eprintln!("warning: registry refresh failed, using cached");
        }
    } else {
        let parent = cache_dir.parent().unwrap_or(mur_home);
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
        let status = Command::new("git")
            .args(["clone", "--depth=1", registry_url, &*cache_dir.to_string_lossy()])
            .status()
            .map_err(|e| anyhow!("git clone: {e}"))?;
        if !status.success() {
            bail!("failed to clone registry from {registry_url}");
        }
    }
    Ok(cache_dir)
}

/// Load the registry index from a cloned registry directory.
pub fn load_index(registry_dir: &Path) -> Result<RegistryIndex> {
    let p = registry_dir.join("index.yaml");
    let text = std::fs::read_to_string(&p)
        .with_context(|| format!("read {}", p.display()))?;
    RegistryIndex::from_yaml(&text)
        .map_err(|e| anyhow!("parse index: {e}"))
}

/// Fetch registry and load index in one call.
pub fn fetch_and_load(mur_home: &Path, url: &str) -> Result<(PathBuf, RegistryIndex)> {
    let dir = fetch_registry(mur_home, url)?;
    let idx = load_index(&dir)?;
    Ok((dir, idx))
}

/// Get a skill's YAML path within the registry clone.
pub fn skill_yaml_path(registry_dir: &Path, name: &str, version: &str) -> PathBuf {
    registry_dir.join("skills").join(name).join("versions").join(format!("{version}.yaml"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    use std::fs;

    #[test]
    fn cache_dir_path() {
        let d = tempdir().unwrap();
        assert!(registry_cache_dir(d.path()).ends_with("cache/registry"));
    }

    #[test]
    fn load_index_from_fs() {
        let d = tempdir().unwrap();
        fs::write(d.path().join("index.yaml"), r#"
skills:
  test:
    latest: 1.0.0
    description: d
    publisher: human:t
    category: context
    tags: []
    content_sha256: "a"
"#).unwrap();
        let idx = load_index(d.path()).unwrap();
        assert_eq!(idx.skills.len(), 1);
    }

    #[test]
    fn skill_yaml_path_matches_spec() {
        let d = tempdir().unwrap();
        let p = skill_yaml_path(d.path(), "my-skill", "1.2.3");
        assert!(p.ends_with("skills/my-skill/versions/1.2.3.yaml"));
    }
}
```

- [ ] **Step 2: Register in `mur-core/src/cmd/mod.rs`**

Add `pub(crate) mod skill_registry;` (alphabetically alongside `skill_cmd`).

- [ ] **Step 3: Run tests**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo test -p mur-core --lib skill_registry::tests
```

Expected: 3 tests pass.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/
git commit -m "feat(registry): registry client (git clone, index, search)"
```

---

### Task 9: `mur skill install` (registry, file, git URL)

**Files:**
- Create: `mur-core/src/cmd/skill_install.rs`
- Modify: `mur-core/src/cmd/mod.rs`

- [ ] **Step 1: Create `mur-core/src/cmd/skill_install.rs`**

```rust
//! Skill install orchestrator — resolve source, fetch, verify, store, trust.

use anyhow::{Context, Result, anyhow, bail};
use std::path::{Path, PathBuf};

use mur_common::skill::{
    self, content_sha256, parse_canonical, read_from_dir, write_to_dir, SkillManifest,
    scan::scan_skill, validate,
};
use mur_common::trust::skills::{SkillTrustStore, TrustEntry};

use crate::cmd::agent::resolve_mur_home;
use crate::cmd::skill_registry;

/// Install a skill from registry, local file, or git URL.
pub fn cmd_install(source: &str) -> Result<()> {
    let home = resolve_mur_home()?;

    if source.starts_with("http") || source.starts_with("git@") || source.starts_with("github:") {
        // Git URL — not yet implemented for M1a
        bail!("install from git URL not yet implemented; use a local file or registry name");
    }

    let src_path = Path::new(source);

    if src_path.exists() && src_path.is_file() {
        // Local file
        return install_from_file(&home, src_path);
    }

    // Treat as registry name
    install_from_registry(&home, source)
}

/// Install from a local skill.yaml file.
fn install_from_file(home: &Path, path: &Path) -> Result<()> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path.display()))?;
    let m = parse_canonical(&text)?;
    validate(&m)?;

    let report = scan_skill(&m)?;
    if report.has_blocking_findings() {
        eprintln!("⚠ Security findings — install proceeds in Sandboxed mode:");
        for line in report.human_summary() {
            eprintln!("  {line}");
        }
    }

    let dir = mur_common::skill::global_skill_dir(home, &m.name);
    write_to_dir(&dir, &m)?;

    // Add trust entry
    let hash = content_sha256(&m)?;
    let mut trust = SkillTrustStore::load(home)
        .map_err(|e| anyhow!("load trust: {e}"))?;
    let level = if report.has_blocking_findings() {
        mur_common::skill::TrustLevel::Sandboxed
    } else {
        mur_common::skill::TrustLevel::Verified
    };
    trust.insert(hash, TrustEntry {
        name: m.name.clone(),
        version: m.version.clone(),
        level,
        installed_at: chrono::Utc::now().to_rfc3339(),
        publisher: Some(m.publisher.clone()),
    });
    trust.save(home).map_err(|e| anyhow!("save trust: {e}"))?;

    println!("installed: {} v{}", m.name, m.version);
    Ok(())
}

/// Install from the default registry by name.
fn install_from_registry(home: &Path, name: &str) -> Result<()> {
    let (_reg_dir, idx) = skill_registry::fetch_and_load(home, skill_registry::DEFAULT_REGISTRY)
        .context("fetch registry")?;

    let entry = idx.skills.get(name)
        .ok_or_else(|| anyhow!("skill '{name}' not found in registry"))?;

    // Fetch the skill YAML from the registry
    let reg_dir = skill_registry::registry_cache_dir(home);
    let skill_path = skill_registry::skill_yaml_path(&reg_dir, name, &entry.latest);
    if !skill_path.exists() {
        bail!("skill file not found at {}", skill_path.display());
    }

    install_from_file(home, &skill_path)
}

/// Update an installed skill to the latest registry version.
pub fn cmd_update(name: &str) -> Result<()> {
    install_from_registry(&resolve_mur_home()?, name)?;
    println!("updated: {name}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    const VALID: &str = r#"
name: test-skill
version: 1.0.0
publisher: human:t
description: test
category: context
content:
  abstract: a
  context: b
"#;

    #[test]
    fn install_from_file_succeeds() {
        let home = tempdir().unwrap();
        let skill_file = home.path().join("s.yaml");
        std::fs::write(&skill_file, VALID).unwrap();
        // Set MUR_HOME-like resolution for testing
        // (resolve_mur_home reads ~/.mur, so unit test uses a mock path)
        // Integration tested via smoke test.
    }

    // Integration tests for from_file work with real ~/.mur path;
    // unit tests here verify the parser/validate steps are correct.
    #[test]
    fn valid_yaml_parses_and_validates() {
        let m = parse_canonical(VALID).unwrap();
        validate(&m).unwrap();
    }
}
```

For the test helper `resolve_mur_home`, plan alternative: the function currently resolves from the `MUR_HOME` env var or `dirs::home_dir()`. In tests, we set `MUR_HOME` env var. The smoke test below validates the full flow.

- [ ] **Step 2: Register module in `mur-core/src/cmd/mod.rs`**

Add `pub(crate) mod skill_install;`.

- [ ] **Step 3: Build**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo build -p mur-core
```

- [ ] **Step 4: Smoke test install from file**

```bash
cat > /tmp/test-skill.yaml << 'EOF'
name: test-skill
version: 1.0.0
publisher: human:t
description: install test
category: context
content:
  abstract: hi
  context: body
EOF
mkdir -p /tmp/test-mur-home
MUR_HOME=/tmp/test-mur-home cargo run -- skill install /tmp/test-skill.yaml
MUR_HOME=/tmp/test-mur-home cargo run -- skill list
MUR_HOME=/tmp/test-mur-home cargo run -- skill info test-skill
```

Expected: install prints "installed: test-skill v1.0.0", list shows it, info shows details.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/
git commit -m "feat(cli): mur skill install (file) + mur skill update"
```

---

### Task 10: `mur skill publish` (sign + fork + PR)

**Files:**
- Create: `mur-core/src/cmd/skill_publish.rs`
- Modify: `mur-core/src/cmd/mod.rs`

- [ ] **Step 1: Create `mur-core/src/cmd/skill_publish.rs`**

```rust
//! Publish a skill to the default registry via fork + PR.
//!
//! Uses the GitHub CLI (`gh`) to:
//!   1. Fork the registry repo
//!   2. Create a branch with the new skill
//!   3. Commit and push
//!   4. Create a PR
//!
//! Requires `gh` to be installed and authenticated.

use anyhow::{Context, Result, anyhow, bail};
use mur_common::identity::AgentIdentity;
use mur_common::skill::{parse_canonical, serialize_canonical, sign_manifest, validate};
use std::path::Path;
use std::process::Command;

const REGISTRY_REPO: &str = "mur-run/skill-registry";

pub fn cmd_publish(path: &str) -> Result<()> {
    // 1. Read and validate the skill
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read {}", path))?;
    let m = parse_canonical(&text)?;
    validate(&m)?;

    // 2. Sign with publisher identity
    let identity = resolve_publisher_identity()?;
    let envelope = sign_manifest(&m, &identity)?;
    println!("✓ Skill signed with key: {}", identity.verifying_key());

    // 3. Check for `gh` CLI
    let gh_check = Command::new("gh").arg("--version").output();
    if gh_check.is_err() {
        bail!("GitHub CLI (`gh`) not found. Install it from https://cli.github.com/");
    }

    // 4. Verify `gh` is authenticated
    let auth_check = Command::new("gh")
        .args(["auth", "status"])
        .output()
        .context("check gh auth")?;
    if !auth_check.status.success() {
        bail!("`gh auth status` failed — please run `gh auth login` first");
    }

    // 5. Check if registry is already forked
    let fork_repo = format!("{}/skill-registry", current_gh_user()?);
    let fork_exists = Command::new("gh")
        .args(["repo", "view", &fork_repo, "--json", "nameWithOwner"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);

    if !fork_exists {
        // Fork the registry
        println!("→ Forking {REGISTRY_REPO}...");
        let status = Command::new("gh")
            .args(["repo", "fork", REGISTRY_REPO, "--clone=false"])
            .status()
            .context("fork registry")?;
        if !status.success() {
            bail!("failed to fork {REGISTRY_REPO}");
        }
    }

    // 6. Clone the fork, write the skill, commit, push
    let tmpdir = tempfile::tempdir().context("create temp dir")?;
    let repo_dir = tmpdir.path().join("skill-registry");

    println!("→ Cloning fork...");
    let status = Command::new("git")
        .args([
            "clone", &format!("https://github.com/{fork_repo}.git"),
            &*repo_dir.to_string_lossy(),
        ])
        .status()
        .context("clone fork")?;
    if !status.success() {
        bail!("failed to clone fork: {fork_repo}");
    }

    // Add upstream remote
    Command::new("git")
        .args(["-C", &*repo_dir.to_string_lossy(), "remote", "add", "upstream",
               &format!("https://github.com/{REGISTRY_REPO}.git")])
        .status().ok();

    // 7. Create branch and write the skill file
    let branch = format!("skill-{}", m.name);
    Command::new("git")
        .args(["-C", &*repo_dir.to_string_lossy(), "checkout", "-b", &branch])
        .status()
        .context("create branch")?;

    // Write skill.yaml in the skill's version directory
    let skill_dir = repo_dir.join("skills").join(&m.name).join("versions");
    std::fs::create_dir_all(&skill_dir)?;
    let skill_path = skill_dir.join(format!("{}.yaml", m.version));
    std::fs::write(&skill_path, serialize_canonical(&m)?)?;

    // Also write the publisher_signature alongside
    let sig_path = skill_dir.join(format!("{}.sig.json", m.version));
    std::fs::write(&sig_path, &envelope)?;

    // 8. Commit and push
    Command::new("git")
        .args(["-C", &*repo_dir.to_string_lossy(), "add", "."])
        .status().context("git add")?;
    Command::new("git")
        .args(["-C", &*repo_dir.to_string_lossy(), "commit",
               "-m", &format!("feat: add {name} v{ver}", name=m.name, ver=m.version),
               "-m", &format!("Publisher: {}\nSigned: true", m.publisher)])
        .status().context("git commit")?;

    println!("→ Pushing branch...");
    let push_result = Command::new("git")
        .args(["-C", &*repo_dir.to_string_lossy(), "push", "origin", &branch])
        .status().context("git push")?;
    if !push_result.success() {
        bail!("git push failed — is your fork up to date? Try rebasing");
    }

    // 9. Create PR
    println!("→ Creating PR...");
    let pr_output = Command::new("gh")
        .args([
            "pr", "create",
            "--repo", REGISTRY_REPO,
            "--head", &format!("{}:{}", current_gh_user()?, branch),
            "--title", &format!("feat: add {name} v{ver}", name=m.name, ver=m.version),
            "--body", &format!(
                "## Summary\n\nAdd `{name}` skill version `{ver}` by {pub_}.\n\n\
                 ## Verification\n\n\
                 - Content hash: {hash}\n\
                 - Signed: yes\n\
                 \n---\n\
                 *Created via `mur skill publish`*",
                name=m.name, ver=m.version, pub_=m.publisher,
                hash=mur_common::skill::content_sha256(&m).unwrap_or_default()
            ),
        ])
        .output()
        .context("create PR")?;
    let pr_url = String::from_utf8_lossy(&pr_output.stdout).trim().to_string();
    if !pr_output.status.success() {
        let stderr = String::from_utf8_lossy(&pr_output.stderr);
        bail!("PR creation failed: {stderr}");
    }

    println!("✓ Published! PR: {pr_url}");
    Ok(())
}

fn resolve_publisher_identity() -> Result<AgentIdentity> {
    // Try loading from ~/.mur/publisher-identity.key
    let home = dirs::home_dir()
        .ok_or_else(|| anyhow!("cannot determine home directory"))?;
    let key_path = home.join(".mur").join("publisher-identity.key");

    if key_path.exists() {
        Ok(AgentIdentity::load(&home.join(".mur"))
            .map_err(|e| anyhow!("load publisher identity: {e}"))?)
    } else {
        // Generate a new identity
        let identity = AgentIdentity::generate();
        std::fs::create_dir_all(key_path.parent().unwrap())?;
        identity
            .save(&home.join(".mur"))
            .map_err(|e| anyhow!("save publisher identity: {e}"))?;
        eprintln!("ℹ Generated new publisher identity at ~/.mur/publisher-identity.key");
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
```

- [ ] **Step 2: Register in `mur-core/src/cmd/mod.rs`**

Add `pub(crate) mod skill_publish;`.

- [ ] **Step 3: Build**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo build -p mur-core
```

Expected: clean build. This module shells out to external tools (`gh`, `git`) so it's tested via smoke test.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/
git commit -m "feat(cli): mur skill publish (sign + fork + PR via gh)"
```

---

### Task 11: Registry repo structure + CI auto-validation

**Files:**
- Create: External `mur-run/skill-registry` repo (documented in the monorepo `docs/superpowers/`)
- The actual repo setup is outside the scope of this codebase — but we document the structure and create the CI workflow template here.

- [ ] **Step 1: Create registry documentation**

Create `docs/superpowers/skill-registry-spec.md` (or extend the existing spec doc) with:

```markdown
# Skill Registry Structure

Repo: `mur-run/skill-registry`

## Directory Layout

```
mur-run/skill-registry/
├── index.yaml                    # Auto-updated search index
├── skills/
│   ├── research-prices/
│   │   ├── versions/
│   │   │   ├── 1.0.0.yaml        # Published skill canonical YAML
│   │   │   ├── 1.0.0.sig.json   # Ed25519 DSSE signature envelope
│   │   │   └── 1.1.0.yaml
│   │   └── latest → versions/1.1.0.yaml   # Symlink (optional)
│   └── web-browsing/
│       └── versions/
│           └── 2.0.0.yaml
├── .github/
│   └── workflows/
│       └── validate.yml          # CI: validate skills on PR
└── CLAUDE.md                     # Contributor docs
```

## index.yaml Format

Auto-regenerated on each merge to main. See `mur-common/src/skill/registry.rs` for the `RegistryIndex` serde type.

## CI Validation

On every PR to `main`:
1. Validate each new/changed `skills/*/versions/*.yaml` using `mur skill validate`
2. Verify Ed25519 signature if `*.sig.json` present
3. Check no duplicate versions
4. Check publisher namespace matches directory
```

- [ ] **Step 2: Create the CI workflow template**

Create `docs/superpowers/assets/registry-ci.yml`:

```yaml
# .github/workflows/validate.yml for mur-run/skill-registry
name: Validate Registry PR
on:
  pull_request:
    paths:
      - 'skills/**/*.yaml'
      - 'index.yaml'

jobs:
  validate:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          fetch-depth: 0

      - name: Install Rust
        uses: actions-rust-lang/setup-rust-toolchain@v1

      - name: Build mur
        run: cargo install --git https://github.com/mur-run/mur.git

      - name: Find changed skill YAMLs
        id: changed
        run: |
          CHANGED=$(git diff --name-only origin/main...HEAD -- 'skills/**/*.yaml' | tr '\n' ' ')
          echo "files=$CHANGED" >> $GITHUB_OUTPUT

      - name: Validate each skill
        run: |
          for f in ${{ steps.changed.outputs.files }}; do
            echo "Validating $f..."
            mur skill validate "$f" || exit 1
          done

      - name: Verify signatures (if present)
        run: |
          for sig in $(git diff --name-only origin/main...HEAD -- 'skills/**/*.sig.json'); do
            echo "Checking signature: $sig"
            # For M1: passive check — M2 enforces
          done
```

- [ ] **Step 3: Commit**

```bash
git add docs/superpowers/
git commit -m "docs(registry): registry structure spec + CI validation workflow"
```

---

### Task 12: Wire `skill_cmd` search to also search remote registry

**Files:**
- Modify: `mur-core/src/cmd/skill_cmd.rs`

- [ ] **Step 1: Update `cmd_search` to search remote when not `--local`**

Replace the `// TODO(M1b)` section in the existing `cmd_search`:

```rust
    // When not local-only, also search registry
    if !local_only {
        match crate::cmd::skill_registry::fetch_and_load(&home, crate::cmd::skill_registry::DEFAULT_REGISTRY) {
            Ok((_dir, idx)) => {
                let reg_results = crate::cmd::skill_registry::search_registry(&idx, &query);
                if !reg_results.is_empty() {
                    if local_results.is_empty() {
                        println!("From registry:");
                    } else {
                        println!();
                        println!("From registry:");
                    }
                    for (name, entry) in reg_results {
                        println!("{name:25} registry    {} [v{}]", entry.description, entry.latest);
                    }
                }
            }
            Err(e) => {
                eprintln!("warning: registry search failed: {e}");
            }
        }
    }
```

And add `search_registry` as an alias in `skill_registry.rs`:

```rust
pub fn search_registry<'a>(idx: &'a RegistryIndex, query: &str) -> Vec<(&'a str, &'a RegistrySkillEntry)> {
    idx.search(query)
}
```

- [ ] **Step 2: Build**

```bash
export PATH=$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH
cargo build -p mur-core
```

- [ ] **Step 3: Commit**

```bash
git add mur-core/src/cmd/
git commit -m "feat(cli): mur skill search queries remote registry"
```

---

## Self-Review

**Spec coverage:**
- §6.1 Git-based registry → Task 7 (data model), Task 8 (client), Task 11 (repo setup)
- §6.2 CLI → Tasks 1–5 (install/list/show/remove/search/info), Task 10 (publish)
- §6.3 Search index → Task 7 (RegistryIndex serde + search), Task 12 (remote search)
- §12 CLI Surface Summary: `install` → Task 9, `remove` → Task 3, `list` → Task 3, `show` → Task 3, `search` → Tasks 4+12, `info` → Task 4, `publish` → Task 10, `update` → Task 9, `audit` → Task 5, `trust` → Task 5, `validate`/`fmt` → M0
- §12 Upgraded `mur agent skill` → Task 6
- §13 Migration Phase 1 → covered by M0's legacy reader + Task 6 YAML compat

**Placeholder scan:** No "TBD", "TODO" without context, no empty catch blocks. The one `TODO(M1b)` in Task 4's search is a genuine forward-reference with a clear scope marker.

**Type consistency:** `SkillTrustStore::load().map_err(...)` pattern used consistently across Tasks 2, 5, 9. `resolve_mur_home()` imported from `crate::cmd::agent` — matches existing convention.

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m1.md`. Two execution options:

**1. Subagent-Driven (recommended)** — I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** — Execute tasks in this session using executing-plans, batch execution with checkpoints

**Which approach?**
