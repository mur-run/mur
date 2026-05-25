# MuR Skill Ecosystem — M3a (Dependency Resolution + Lock File) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use `superpowers:subagent-driven-development` (recommended) or `superpowers:executing-plans`. Steps use checkbox (`- [ ]`) syntax.

**Goal:** Make `mur skill install <name>` recursively resolve `requires:`, pick versions satisfying semver constraints, detect cycles, and persist the resolved graph to `skill.lock`. `mur skill update` bumps locks; `mur skill deps` prints the tree.

**Out of scope (deferred to M3b/M3c):** agent-generated skills (`generate --from-session`), pattern promotion, self-evolution loop. Filed in separate plans.

---

## Codebase Reality Check (read before executing)

Verified against `main` after M2 merge (`62fb129`):

| Assumption | Reality |
|---|---|
| Install entry point | `mur-core/src/cmd/skill_install.rs::cmd_install` — single-file flow, no recursion, no lock write. Calls `install_from_file` or `install_from_registry`. |
| `requires:` parsing | `mur_common::skill::manifest::Requirement { name, version }` already exists. `version` defaults to `"*"`. No semver validation yet. |
| Registry layout | `~/.mur/cache/registry/index.yaml` carries `latest` + `content_sha256` per skill. Per-version YAML lives at `skills/<name>/versions/<ver>.yaml`. **`RegistryIndex` does NOT list all versions** — must enumerate `versions/*.yaml` on disk. |
| `skill_yaml_path` helper | `skill_registry::skill_yaml_path(reg_dir, name, version)` already exists. |
| `semver` crate | Already in `mur-core/Cargo.toml` (`semver = "1"`). **Not in `mur-common`** — M3a adds it there too so `lockfile` + `requirement::Constraint` types live alongside `Requirement`. |
| Existing install side-effects | Writes `~/.mur/skills/<name>/skill.yaml`, registers trust entry. No dependency awareness. |
| Trust + scan | `scan_skill` + `SkillTrustStore::insert` already fire on every install. Transitive installs must reuse them — no bypass. |
| `mur skill` subcommands | Dispatch happens in `mur-core/src/main.rs` (or `cmd/mod.rs`); confirm before adding `mur skill deps` / `mur skill update --dry-run`. Grep `cmd_install` callers to find the clap arm. |
| Lock file location | Spec §3.1 puts it at `~/.mur/skills/<name>/skill.lock` (one lock per top-level installed skill, captures the transitive closure pinned at install time). |

---

## File Structure

**Create:**
- `mur-common/src/skill/constraint.rs` — semver constraint parsing (`>=1.0.0`, `^1.2.3`, `~1.2.3`, `1.2.3`, `*`) + matching
- `mur-common/src/skill/lockfile.rs` — `SkillLock { locked: BTreeMap<String, String>, installed_at: String }` with atomic read/write
- `mur-core/src/cmd/skill_resolver.rs` — DFS resolver with cycle detection and version selection
- `mur-core/src/cmd/skill_deps.rs` — `mur skill deps <name>` tree printer
- `mur-core/tests/skill_install_recursive_e2e.rs` — E2E install/cycle/idempotency over a real-git `file://` registry
- `mur-core/tests/common/mod.rs` — `TestRegistry` helper (git-init + publish + commit + `file://` URL)

**Modify:**
- `mur-common/src/skill/mod.rs` — re-export `constraint`, `lockfile`
- `mur-common/Cargo.toml` — add `semver = "1"` dep
- `mur-core/src/cmd/skill_install.rs` — split `cmd_install_cli` (env-resolving shim) from pure `cmd_install(home, registry_url, source)`; call resolver; install resolved closure; write lock
- `mur-core/src/cmd/skill_registry.rs` — add `available_versions(reg_dir, name) -> Vec<semver::Version>`
- `mur-core/src/main.rs` (or wherever `cmd_install` is wired) — switch caller to `cmd_install_cli`; add `deps` subcommand

---

## Self-contained Type Sketch

```rust
// mur-common/src/skill/constraint.rs
use semver::{Version, VersionReq};

#[derive(Debug, Clone)]
pub struct Constraint(pub VersionReq);

impl Constraint {
    /// Parse the strings written in `requires:`. "*" → any.
    pub fn parse(s: &str) -> Result<Self, ConstraintError>;
    pub fn matches(&self, v: &Version) -> bool;
}

#[derive(Debug, thiserror::Error)]
pub enum ConstraintError { /* … */ }

// mur-common/src/skill/lockfile.rs
use std::collections::BTreeMap;

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillLock {
    pub locked: BTreeMap<String, String>,    // name → exact version
    pub installed_at: String,                 // RFC 3339 UTC
    #[serde(default)]
    pub schema_version: u32,                  // = 1
}
impl SkillLock {
    pub fn read(skill_dir: &Path) -> Result<Self, LockfileError>; // empty if missing
    pub fn write(&self, skill_dir: &Path) -> Result<(), LockfileError>; // temp+rename
}

// mur-core/src/cmd/skill_resolver.rs
pub struct ResolvedNode {
    pub name: String,
    pub version: semver::Version,
    pub yaml_path: PathBuf,    // path inside registry cache OR the requested local file
    pub manifest: SkillManifest,
}

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("cyclic dependency: {0}")]
    Cycle(String),
    #[error("no version of '{name}' satisfies '{req}'; available: {available:?}")]
    NoMatch { name: String, req: String, available: Vec<String> },
    #[error("skill '{0}' not found in registry")] NotFound(String),
    #[error(transparent)] Other(#[from] anyhow::Error),
}

pub fn resolve(
    home: &Path,
    root: ResolveSource,                   // File(path) or Registry { name, constraint }
) -> Result<Vec<ResolvedNode>, ResolveError>;
// post-condition: returns nodes in install order (leaves first); root is last.
```

---

### Task 1 — Constraint type (`mur-common::skill::constraint`)

**Files:** `mur-common/Cargo.toml`, `mur-common/src/skill/constraint.rs`, `mur-common/src/skill/mod.rs`.

- [ ] **1.1** Add `semver = "1"` to `[dependencies]` of `mur-common/Cargo.toml`.

- [ ] **1.2** Create `constraint.rs`:

```rust
use semver::{Version, VersionReq};

#[derive(Debug, Clone)]
pub struct Constraint(pub VersionReq);

#[derive(Debug, thiserror::Error)]
pub enum ConstraintError {
    #[error("invalid version requirement '{0}': {1}")]
    Parse(String, String),
}

impl Constraint {
    /// Parse "requires:[].version".
    /// Accepts the spec set: `>=1.0.0`, `^1.2.3`, `~1.2.3`, exact `1.2.3`, `*`.
    /// Empty string and `*` both mean "any".
    pub fn parse(s: &str) -> Result<Self, ConstraintError> {
        let trimmed = s.trim();
        if trimmed.is_empty() || trimmed == "*" {
            return Ok(Self(VersionReq::STAR));
        }
        // Bare "1.2.3" — semver treats as ^1.2.3 by default which is fine for our spec.
        VersionReq::parse(trimmed)
            .map(Self)
            .map_err(|e| ConstraintError::Parse(trimmed.into(), e.to_string()))
    }
    pub fn matches(&self, v: &Version) -> bool { self.0.matches(v) }
    pub fn any() -> Self { Self(VersionReq::STAR) }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn star_matches_anything() {
        let c = Constraint::parse("*").unwrap();
        assert!(c.matches(&Version::parse("0.0.1").unwrap()));
        assert!(c.matches(&Version::parse("99.99.99").unwrap()));
    }
    #[test] fn ge_pins_minimum() {
        let c = Constraint::parse(">=1.0.0").unwrap();
        assert!(!c.matches(&Version::parse("0.9.9").unwrap()));
        assert!(c.matches(&Version::parse("1.0.0").unwrap()));
        assert!(c.matches(&Version::parse("2.0.0").unwrap()));
    }
    #[test] fn caret_locks_major() {
        let c = Constraint::parse("^1.2.3").unwrap();
        assert!(c.matches(&Version::parse("1.9.0").unwrap()));
        assert!(!c.matches(&Version::parse("2.0.0").unwrap()));
    }
    #[test] fn tilde_locks_minor() {
        let c = Constraint::parse("~1.2.3").unwrap();
        assert!(c.matches(&Version::parse("1.2.9").unwrap()));
        assert!(!c.matches(&Version::parse("1.3.0").unwrap()));
    }
    #[test] fn exact_pins_exact() {
        let c = Constraint::parse("=1.2.3").unwrap();
        assert!(c.matches(&Version::parse("1.2.3").unwrap()));
        assert!(!c.matches(&Version::parse("1.2.4").unwrap()));
    }
    #[test] fn empty_means_any() {
        let c = Constraint::parse("").unwrap();
        assert!(c.matches(&Version::parse("0.0.1").unwrap()));
    }
    #[test] fn garbage_fails() {
        assert!(Constraint::parse("not-a-version").is_err());
    }
}
```

- [ ] **1.3** Re-export from `mur-common/src/skill/mod.rs`:
  ```rust
  pub mod constraint;
  pub use constraint::{Constraint, ConstraintError};
  ```

- [ ] **1.4** Build + commit:
  ```bash
  cargo test -p mur-common skill::constraint
  git add mur-common/Cargo.toml mur-common/src/skill/constraint.rs mur-common/src/skill/mod.rs
  git commit -m "feat(skill): semver Constraint parser"
  ```

---

### Task 2 — Lock file (`mur-common::skill::lockfile`)

**Files:** `mur-common/src/skill/lockfile.rs`, `mur-common/src/skill/mod.rs`.

- [ ] **2.1** Create `lockfile.rs` with atomic temp+rename write (mirror the pattern used in `trust/skills.rs::save`):

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::Path;

pub const SCHEMA_VERSION: u32 = 1;
pub const FILE_NAME: &str = "skill.lock";

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SkillLock {
    #[serde(default = "default_schema")]
    pub schema_version: u32,
    #[serde(default)]
    pub locked: BTreeMap<String, String>,
    #[serde(default)]
    pub installed_at: String,
}

fn default_schema() -> u32 { SCHEMA_VERSION }

#[derive(Debug, thiserror::Error)]
pub enum LockfileError {
    #[error("io: {0}")] Io(#[from] io::Error),
    #[error("parse: {0}")] Parse(#[from] serde_yaml_ng::Error),
}

impl SkillLock {
    pub fn path(skill_dir: &Path) -> std::path::PathBuf {
        skill_dir.join(FILE_NAME)
    }
    pub fn read(skill_dir: &Path) -> Result<Self, LockfileError> {
        let p = Self::path(skill_dir);
        if !p.exists() { return Ok(Self { schema_version: SCHEMA_VERSION, ..Default::default() }); }
        let s = fs::read_to_string(&p)?;
        if s.trim().is_empty() { return Ok(Self { schema_version: SCHEMA_VERSION, ..Default::default() }); }
        Ok(serde_yaml_ng::from_str(&s)?)
    }
    pub fn write(&self, skill_dir: &Path) -> Result<(), LockfileError> {
        fs::create_dir_all(skill_dir)?;
        let yaml = serde_yaml_ng::to_string(self)?;
        let final_path = Self::path(skill_dir);
        let tmp = skill_dir.join(format!(".{FILE_NAME}.tmp"));
        fs::write(&tmp, yaml)?;
        fs::rename(tmp, final_path)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;
    #[test] fn empty_when_missing() {
        let d = tempdir().unwrap();
        let l = SkillLock::read(d.path()).unwrap();
        assert_eq!(l.schema_version, SCHEMA_VERSION);
        assert!(l.locked.is_empty());
    }
    #[test] fn round_trip() {
        let d = tempdir().unwrap();
        let mut l = SkillLock { schema_version: SCHEMA_VERSION, locked: BTreeMap::new(), installed_at: "2026-05-25T00:00:00Z".into() };
        l.locked.insert("web-browsing".into(), "1.2.0".into());
        l.locked.insert("data-table-export".into(), "0.6.1".into());
        l.write(d.path()).unwrap();
        let back = SkillLock::read(d.path()).unwrap();
        assert_eq!(back.locked["web-browsing"], "1.2.0");
        assert_eq!(back.installed_at, l.installed_at);
    }
    #[test] fn corrupt_yaml_returns_parse_err() {
        let d = tempdir().unwrap();
        fs::write(SkillLock::path(d.path()), "this is :: not yaml :: at all").unwrap();
        assert!(matches!(SkillLock::read(d.path()), Err(LockfileError::Parse(_))));
    }
}
```

- [ ] **2.2** Re-export in `skill/mod.rs`:
  ```rust
  pub mod lockfile;
  pub use lockfile::{SkillLock, LockfileError};
  ```

- [ ] **2.3** Commit:
  ```bash
  cargo test -p mur-common skill::lockfile
  git add mur-common/src/skill/lockfile.rs mur-common/src/skill/mod.rs
  git commit -m "feat(skill): SkillLock with atomic write"
  ```

---

### Task 3 — Registry: enumerate available versions

**Files:** `mur-core/src/cmd/skill_registry.rs`.

- [ ] **3.1** Add a function listing every `<name>/versions/*.yaml` file as a parsed `semver::Version`:

```rust
use semver::Version;

/// List the versions of `name` available in the registry cache.
/// Returns sorted ascending. Returns `Ok(vec![])` if the skill dir doesn't exist.
pub fn available_versions(registry_dir: &Path, name: &str) -> Result<Vec<Version>> {
    let dir = registry_dir.join("skills").join(name).join("versions");
    if !dir.exists() { return Ok(vec![]); }
    let mut out = Vec::new();
    for entry in std::fs::read_dir(&dir).with_context(|| format!("read {}", dir.display()))? {
        let entry = entry?;
        if !entry.file_type()?.is_file() { continue; }
        let file_name = entry.file_name();
        let name_str = file_name.to_string_lossy();
        let Some(stripped) = name_str.strip_suffix(".yaml") else { continue; };
        match Version::parse(stripped) {
            Ok(v) => out.push(v),
            Err(e) => tracing::warn!(file = %name_str, error = %e, "skipping non-semver version filename"),
        }
    }
    out.sort();
    Ok(out)
}
```

- [ ] **3.2** Tests in `skill_registry.rs`:
  1. Missing directory → `Ok(vec![])`.
  2. Three valid version files → sorted ascending.
  3. A `0.0.1-not-semver.yaml` is skipped with warn, others returned.

> **Note for T7 readers:** the offline-registry concern raised in earlier drafts (a `MUR_SKILL_REGISTRY_LOCAL` env-gate) is **not done at this layer**. T7 instead exercises real `git clone` against a `file://` URL pointing at a tempdir git repo. The CLI shim added in T5 reads `MUR_SKILL_REGISTRY_URL` so production users can also point at private mirrors — that env var is a real product feature, not a test knob.

- [ ] **3.3** Commit:
  ```bash
  cargo test -p mur-core skill_registry::tests::available_versions
  git add mur-core/src/cmd/skill_registry.rs
  git commit -m "feat(skill): enumerate available registry versions"
  ```

---

### Task 4 — Resolver with cycle detection (`mur-core::cmd::skill_resolver`)

**Files:** `mur-core/src/cmd/skill_resolver.rs`, `mur-core/src/cmd/mod.rs`.

Resolver algorithm (DFS, depth-first install order):

```
resolve(root):
  state = {
    selected: BTreeMap<name, ResolvedNode>,     # version pinned per skill
    on_stack: Vec<name>,                         # current DFS path for cycle detection
    install_order: Vec<name>,                    # leaves-first traversal
    constraints: BTreeMap<name, Vec<(name_of_dep_requester, Constraint)>>,
  }

  visit(node, requested_constraint):
    if node.name in on_stack:
      ERROR Cycle(format!("{} -> {} -> ... -> {}", path))

    if node.name in selected:
      # Already chose a version — must satisfy new constraint too.
      if !requested_constraint.matches(selected[node.name].version):
        ERROR NoMatch (existing version conflicts with new constraint)
      return

    on_stack.push(node.name)
    selected[node.name] = node
    for req in node.manifest.requires:
      c = Constraint::parse(req.version)?
      best = pick_best(req.name, c)?         # highest version satisfying c
      visit(best, c)
    install_order.push(node.name)
    on_stack.pop()
```

`pick_best`:
- File install (root only): the file itself, version from manifest.version.
- Registry: list `available_versions(registry, name)`, filter by constraint, pick the highest. If none, `NoMatch`.

- [ ] **4.1** Implementation skeleton:

```rust
//! Recursive resolver for `mur skill install` — DFS with cycle detection,
//! semver constraint matching, leaves-first install ordering.

use anyhow::{Context, Result, anyhow};
use mur_common::skill::{
    Constraint, ConstraintError, SkillManifest, parse_canonical, validate,
};
use semver::Version;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ResolveError {
    #[error("cyclic dependency: {0}")]
    Cycle(String),
    #[error("no version of '{name}' satisfies '{req}' (existing pin: {existing}); available: {available:?}")]
    Conflict { name: String, req: String, existing: String, available: Vec<String> },
    #[error("no version of '{name}' satisfies '{req}'; available: {available:?}")]
    NoMatch { name: String, req: String, available: Vec<String> },
    #[error("skill '{0}' not found in registry cache")]
    NotFound(String),
    #[error("constraint parse: {0}")] BadConstraint(#[from] ConstraintError),
    #[error("manifest parse: {0}")] BadManifest(String),
    #[error(transparent)] Other(#[from] anyhow::Error),
}

#[derive(Debug, Clone)]
pub struct ResolvedNode {
    pub name: String,
    pub version: Version,
    pub yaml_path: PathBuf,
    pub manifest: SkillManifest,
}

pub enum ResolveSource<'a> {
    LocalFile(&'a Path),
    RegistryLatest(&'a str),
}

pub struct ResolverInput {
    pub mur_home: PathBuf,
    pub registry_dir: PathBuf,
}

pub fn resolve(input: &ResolverInput, source: ResolveSource<'_>) -> Result<Vec<ResolvedNode>, ResolveError> {
    let mut state = State::default();
    let root = load_root(input, source)?;
    visit(input, &mut state, root, Constraint::any())?;
    Ok(state.install_order.into_iter().map(|n| state.selected.remove(&n).unwrap()).collect())
}

#[derive(Default)]
struct State {
    selected: BTreeMap<String, ResolvedNode>,
    on_stack: Vec<String>,
    install_order: Vec<String>,
}

fn load_root(input: &ResolverInput, src: ResolveSource<'_>) -> Result<ResolvedNode, ResolveError> {
    match src {
        ResolveSource::LocalFile(p) => load_from_path(p),
        ResolveSource::RegistryLatest(name) => {
            let versions = crate::cmd::skill_registry::available_versions(&input.registry_dir, name)
                .map_err(ResolveError::Other)?;
            let v = versions.last().cloned().ok_or_else(|| ResolveError::NotFound(name.into()))?;
            let path = crate::cmd::skill_registry::skill_yaml_path(&input.registry_dir, name, &v.to_string());
            load_from_path(&path)
        }
    }
}

fn load_from_path(path: &Path) -> Result<ResolvedNode, ResolveError> {
    let text = std::fs::read_to_string(path).map_err(|e| ResolveError::Other(e.into()))?;
    let m = parse_canonical(&text).map_err(|e| ResolveError::BadManifest(e.to_string()))?;
    validate(&m).map_err(|e| ResolveError::BadManifest(e.to_string()))?;
    let v = Version::parse(&m.version).map_err(|e| ResolveError::BadManifest(format!("version: {e}")))?;
    Ok(ResolvedNode { name: m.name.clone(), version: v, yaml_path: path.to_path_buf(), manifest: m })
}

fn pick_best(
    input: &ResolverInput,
    name: &str,
    c: &Constraint,
) -> Result<ResolvedNode, ResolveError> {
    let versions = crate::cmd::skill_registry::available_versions(&input.registry_dir, name)
        .map_err(ResolveError::Other)?;
    let mut candidates: Vec<&Version> = versions.iter().filter(|v| c.matches(v)).collect();
    candidates.sort();
    let pick = candidates.last().ok_or_else(|| ResolveError::NoMatch {
        name: name.into(),
        req: c.0.to_string(),
        available: versions.iter().map(|v| v.to_string()).collect(),
    })?;
    let path = crate::cmd::skill_registry::skill_yaml_path(&input.registry_dir, name, &pick.to_string());
    load_from_path(&path)
}

fn visit(
    input: &ResolverInput,
    state: &mut State,
    node: ResolvedNode,
    requested: Constraint,
) -> Result<(), ResolveError> {
    if state.on_stack.iter().any(|n| n == &node.name) {
        let mut path = state.on_stack.clone();
        path.push(node.name.clone());
        return Err(ResolveError::Cycle(path.join(" -> ")));
    }
    if let Some(existing) = state.selected.get(&node.name) {
        if !requested.matches(&existing.version) {
            return Err(ResolveError::Conflict {
                name: node.name,
                req: requested.0.to_string(),
                existing: existing.version.to_string(),
                available: vec![existing.version.to_string()],
            });
        }
        return Ok(());
    }
    state.on_stack.push(node.name.clone());
    let name = node.name.clone();
    let requires = node.manifest.requires.clone();
    state.selected.insert(name.clone(), node);

    for req in requires {
        let c = Constraint::parse(&req.version)?;
        let chosen = pick_best(input, &req.name, &c)?;
        visit(input, state, chosen, c)?;
    }

    state.install_order.push(name);
    state.on_stack.pop();
    Ok(())
}
```

- [ ] **4.2** Register the module in `mur-core/src/cmd/mod.rs`.

- [ ] **4.3** Unit tests in `skill_resolver.rs` (using an in-memory tempdir as the "registry cache"):
  1. Single skill, no `requires:` → resolves to one node.
  2. A requires B (>=1.0.0); B has 1.0.0, 1.1.0 published → picks B 1.1.0; install order = [B, A].
  3. A requires B 1.x AND C requires B 2.x; A also requires C → Conflict error mentions B.
  4. A requires B; B requires A → Cycle("A -> B -> A").
  5. A requires B "^2.0.0"; only B 1.x published → NoMatch with available = ["1.0.0", "1.1.0"].
  6. Diamond: A requires B and C; both B and C require D 1.x. D appears exactly once in install_order.
  7. Garbage version string in `requires:` → BadConstraint error.

- [ ] **4.4** Build + commit:
  ```bash
  cargo test -p mur-core skill_resolver
  git add mur-core/src/cmd/skill_resolver.rs mur-core/src/cmd/mod.rs
  git commit -m "feat(skill): dependency resolver with cycle + semver"
  ```

---

### Task 5 — Wire resolver into `cmd_install` + lock file write

**Files:** `mur-core/src/cmd/skill_install.rs`.

**Refactor first**: today `cmd_install(source: &str)` reads `MUR_HOME` via `resolve_mur_home()`. We split it:

- `cmd_install(home: &Path, registry_url: &str, source: &str)` — **pure**, accepts everything as args. Used by tests and by future M4 peer-transfer code.
- `cmd_install_cli(source: &str)` — thin shim that resolves `home` from env (via `resolve_mur_home()`) and reads `MUR_SKILL_REGISTRY_URL` (default = `skill_registry::DEFAULT_REGISTRY`), then delegates. This is the function the CLI dispatch arm calls.

Reading `MUR_SKILL_REGISTRY_URL` is a real product feature (private mirrors, enterprise air-gapped, dev/staging registries) — not a test affordance.

- [ ] **5.1** Pure `cmd_install`:

```rust
pub fn cmd_install(home: &Path, registry_url: &str, source: &str) -> Result<()> {
    let src_path = Path::new(source);

    // Fetch registry once up front (resolver needs it for any non-file install).
    let (reg_dir, _idx) = crate::cmd::skill_registry::fetch_and_load(home, registry_url)
        .context("fetch registry")?;

    let input = crate::cmd::skill_resolver::ResolverInput {
        mur_home: home.to_path_buf(),
        registry_dir: reg_dir,
    };

    let source_enum = if src_path.exists() && src_path.is_file() {
        crate::cmd::skill_resolver::ResolveSource::LocalFile(src_path)
    } else {
        crate::cmd::skill_resolver::ResolveSource::RegistryLatest(source)
    };

    let order = crate::cmd::skill_resolver::resolve(&input, source_enum)?;
    if order.is_empty() { bail!("resolver returned empty install order"); }

    // Install leaves first. The root is the last entry.
    for node in &order {
        install_resolved_node(home, node)?;
    }

    // Write lock at the root skill dir.
    let root = order.last().unwrap();
    let mut lock = mur_common::skill::SkillLock {
        schema_version: mur_common::skill::lockfile::SCHEMA_VERSION,
        installed_at: chrono::Utc::now().to_rfc3339(),
        locked: order.iter().map(|n| (n.name.clone(), n.version.to_string())).collect(),
    };
    let root_dir = mur_common::skill::global_skill_dir(home, &root.name);
    lock.write(&root_dir).context("write skill.lock")?;

    println!("installed: {} v{}", root.name, root.version);
    if order.len() > 1 {
        println!("  + {} transitive dependencies", order.len() - 1);
    }
    Ok(())
}

fn install_resolved_node(home: &Path, node: &ResolvedNode) -> Result<()> {
    let report = scan_skill(&node.manifest)?;
    let dir = global_skill_dir(home, &node.name);
    write_to_dir(&dir, &node.manifest)?;
    let hash = content_sha256(&node.manifest)?;
    let mut trust = SkillTrustStore::load(home).map_err(|e| anyhow!("load trust: {e}"))?;
    let level = if report.has_blocking_findings() {
        TrustLevel::Sandboxed
    } else {
        TrustLevel::Verified
    };
    trust.insert(hash, TrustEntry {
        name: node.name.clone(),
        version: node.version.to_string(),
        level,
        installed_at: chrono::Utc::now().to_rfc3339(),
        publisher: Some(node.manifest.publisher.clone()),
    });
    trust.save(home).map_err(|e| anyhow!("save trust: {e}"))?;
    if report.has_blocking_findings() {
        eprintln!("⚠ {} v{}: security findings — installed Sandboxed", node.name, node.version);
        for line in report.human_summary() { eprintln!("    {line}"); }
    }
    Ok(())
}

// CLI shim — what main.rs / the dispatch arm calls.
pub fn cmd_install_cli(source: &str) -> Result<()> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
        .unwrap_or_else(|_| crate::cmd::skill_registry::DEFAULT_REGISTRY.to_string());
    cmd_install(&home, &registry_url, source)
}
```

- [ ] **5.2** Same split for `cmd_update`:

```rust
pub fn cmd_update(home: &Path, registry_url: &str, name: &str) -> Result<()> {
    cmd_install(home, registry_url, name)?;
    println!("updated: {name}");
    Ok(())
}
pub fn cmd_update_cli(name: &str) -> Result<()> {
    let home = resolve_mur_home()?;
    let registry_url = std::env::var("MUR_SKILL_REGISTRY_URL")
        .unwrap_or_else(|_| crate::cmd::skill_registry::DEFAULT_REGISTRY.to_string());
    cmd_update(&home, &registry_url, name)
}
```

Remove the old `install_from_file` / `install_from_registry` private helpers — they're now subsumed by the resolver.

- [ ] **5.3** Update the CLI dispatch site to call `cmd_install_cli` / `cmd_update_cli` instead of the old `cmd_install` / `cmd_update`. Grep:
  ```bash
  grep -rn "cmd_install\b\|cmd_update\b" mur-core/src/ 2>/dev/null
  ```
  Update every call site.

- [ ] **5.4** Unit tests in `skill_install.rs` (use the pure form, no env):
  - Local file install with no `requires:` → file installed, `skill.lock` written with one entry.
  - Local file install where `requires:` references a name unavailable in the registry tempdir → `ResolveError::NoMatch` propagates as `anyhow::Error`.
  - `cmd_install_cli` honors `MUR_SKILL_REGISTRY_URL` (set env, point at a known-failing URL, observe `fetch registry` error). Tests that mutate env must use `#[serial]` from `serial_test` if you add one; better: skip this assertion at the unit level and let T7 cover env-vs-arg parity via direct calls to the pure form.

- [ ] **5.5** Build + commit:
  ```bash
  cargo test -p mur-core skill_install
  git add mur-core/src/cmd/skill_install.rs mur-core/src/main.rs
  git commit -m "feat(skill): recursive install with lock file (home + registry_url as args)"
  ```

---

### Task 6 — `mur skill deps <name>` tree printer + CLI wiring

**Files:** `mur-core/src/cmd/skill_deps.rs`, `mur-core/src/cmd/mod.rs`, CLI dispatch site (likely `mur-core/src/main.rs` or `mur-core/src/cmd/skill.rs` — grep `cmd_install` callers and add the new arm next to it).

- [ ] **6.1** `skill_deps.rs`:

```rust
//! `mur skill deps <name>` — print the resolved dependency tree from `skill.lock`,
//! falling back to live resolution if the lock is missing.

use anyhow::{Context, Result, bail};
use mur_common::skill::{SkillLock, global_skill_dir, load_installed};
use std::collections::BTreeMap;
use std::path::Path;

pub fn cmd_deps(name: &str) -> Result<()> {
    let home = crate::cmd::agent::resolve_mur_home()?;
    let root_dir = global_skill_dir(&home, name);
    if !root_dir.exists() {
        bail!("'{name}' is not installed");
    }
    let lock = SkillLock::read(&root_dir).context("read skill.lock")?;
    let root_manifest = load_installed(&home, name).context("read root manifest")?;

    println!("{} v{}", name, root_manifest.version);
    print_subtree(&home, &lock.locked, &root_manifest.requires, "  ")?;
    Ok(())
}

fn print_subtree(
    home: &Path,
    locked: &BTreeMap<String, String>,
    reqs: &[mur_common::skill::Requirement],
    indent: &str,
) -> Result<()> {
    for r in reqs {
        let pinned = locked.get(&r.name).map(String::as_str).unwrap_or("?");
        println!("{indent}{} ({}) -> {pinned}", r.name, r.version);
        if let Ok(m) = load_installed(home, &r.name) {
            let next = format!("{indent}  ");
            print_subtree(home, locked, &m.requires, &next)?;
        }
    }
    Ok(())
}
```

- [ ] **6.2** Wire into the CLI. The exact location depends on where `cmd_install` is dispatched today — grep for it and mirror the pattern:
  ```bash
  grep -rn "cmd_install\|SkillCmd\|skill::Install\|Skill ::" mur-core/src/main.rs mur-core/src/cmd/*.rs 2>/dev/null | head
  ```
  Add a `Deps { name: String }` arm that calls `cmd_deps(&name)`.

- [ ] **6.3** Smoke test (unit, against a temp mur_home):
  - Install A v1.0.0 with `requires: [{name: B, version: ">=1.0.0"}]` plus B v1.0.0 mock.
  - Call `cmd_deps("A")` capturing stdout via `print!` indirection or a thin `Write`-based variant.

  If capturing stdout is awkward, refactor `cmd_deps` to take a `&mut dyn Write` and have the CLI binding pass `std::io::stdout().lock()` — this keeps the smoke test cheap. Make this refactor part of 6.1 if you anticipate it.

- [ ] **6.4** Commit:
  ```bash
  cargo test -p mur-core skill_deps
  git add mur-core/src/cmd/skill_deps.rs mur-core/src/cmd/mod.rs mur-core/src/main.rs
  git commit -m "feat(skill): mur skill deps <name> tree printer"
  ```

---

### Task 7 — End-to-end integration test

**Files:** `mur-core/tests/skill_install_recursive_e2e.rs`, `mur-core/tests/common/mod.rs`.

**Strategy:** drive the **pure** `cmd_install(home, registry_url, source)` from T5 against a `file://` URL pointing at a tempdir git repo. This:

- exercises real `git clone` — full production code-path fidelity
- needs **zero test-only env vars** and zero prod-code knobs
- doesn't touch `MUR_HOME` env (the pure form takes `home: &Path`), so tests run in parallel safely
- doubles as documentation: `MUR_SKILL_REGISTRY_URL` (added in T5) works exactly the same way for private mirrors

T4's unit tests already cover diamond / cycle / no-match / conflict at the resolver layer; T7's job is to confirm the **wiring** (cmd_install → resolver → install → lock → trust). Three tests cover that contract.

- [ ] **7.1** `tests/common/mod.rs` — shared `TestRegistry` helper:

```rust
//! Test-only: filesystem git repo that behaves like the real mur skill registry.
//! Tests clone it via `git clone file://...` exactly as production does.

use std::path::Path;
use std::process::Command;
use tempfile::TempDir;

pub struct TestRegistry { dir: TempDir }

impl TestRegistry {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        run(dir.path(), &["init", "-q", "-b", "main"]);
        run(dir.path(), &["config", "user.email", "test@mur.local"]);
        run(dir.path(), &["config", "user.name", "test"]);
        // Seed an empty index.yaml so `load_index` succeeds even before publish().
        std::fs::write(dir.path().join("index.yaml"), "schema_version: 1\nupdated_at: 2026-01-01T00:00:00Z\nskills: {}\n").unwrap();
        Self { dir }
    }

    /// Publish a skill version. Each call adds a new `versions/<v>.yaml` and
    /// updates `index.yaml`'s `latest` to this version.
    pub fn publish(&self, name: &str, version: &str, requires: &[(&str, &str)]) {
        let vdir = self.dir.path().join("skills").join(name).join("versions");
        std::fs::create_dir_all(&vdir).unwrap();
        std::fs::write(vdir.join(format!("{version}.yaml")), build_skill_yaml(name, version, requires)).unwrap();
        bump_index_latest(self.dir.path(), name, version);
    }

    /// `git add . && git commit` so the repo has a real HEAD that `git clone` can fetch.
    pub fn commit(&self) {
        run(self.dir.path(), &["add", "."]);
        run(self.dir.path(), &["commit", "-q", "-m", "test fixture"]);
    }

    /// `file://` URL accepted by `git clone` on macOS/Linux/Windows alike.
    pub fn url(&self) -> String {
        // Windows: file:///C:/Users/... — std's Display already uses forward slashes
        // for the temp path in Rust, but if not, use url::Url::from_file_path to be safe.
        format!("file://{}", self.dir.path().display())
    }
}

fn run(cwd: &Path, args: &[&str]) {
    let st = Command::new("git").args(args).current_dir(cwd).status().expect("git available");
    assert!(st.success(), "git {:?} failed in {}", args, cwd.display());
}

fn build_skill_yaml(name: &str, version: &str, requires: &[(&str, &str)]) -> String {
    let mut s = format!(
        "name: {name}\nversion: {version}\npublisher: human:test\ndescription: test\ncategory: context\ncontent:\n  abstract: a\n  context: b\n"
    );
    if !requires.is_empty() {
        s.push_str("requires:\n");
        for (n, v) in requires {
            s.push_str(&format!("  - name: {n}\n    version: \"{v}\"\n"));
        }
    }
    s
}

fn bump_index_latest(reg_root: &Path, name: &str, version: &str) {
    use mur_common::skill::registry::{RegistryIndex, RegistrySkillEntry};
    let p = reg_root.join("index.yaml");
    let mut idx: RegistryIndex = serde_yaml_ng::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
    let entry = idx.skills.entry(name.into()).or_insert(RegistrySkillEntry {
        latest: version.into(), description: "test".into(), publisher: "human:test".into(),
        category: "context".into(), tags: vec![], content_sha256: String::new(), install_count: 0,
    });
    // Always set latest to the most-recently-published version of this name.
    entry.latest = version.into();
    std::fs::write(&p, idx.to_yaml().unwrap()).unwrap();
}
```

- [ ] **7.2** The three tests:

```rust
mod common;
use common::TestRegistry;
use mur_core::cmd::skill_install::cmd_install;
use mur_common::skill::SkillLock;
use mur_common::trust::skills::SkillTrustStore;
use std::collections::HashSet;

#[test]
fn install_with_transitive_deps_writes_full_lock_and_trust() {
    let home = tempfile::tempdir().unwrap();
    let reg = TestRegistry::new();
    reg.publish("dep-c", "1.0.0", &[]);
    reg.publish("dep-b", "1.2.0", &[("dep-c", ">=1.0.0")]);
    reg.publish("root",  "0.1.0", &[("dep-b", "^1.0.0")]);
    reg.commit();

    cmd_install(home.path(), &reg.url(), "root").expect("install ok");

    // All three skill.yaml files written
    for name in ["root", "dep-b", "dep-c"] {
        assert!(home.path().join("skills").join(name).join("skill.yaml").exists(), "{name} skill.yaml");
    }

    // skill.lock at root with all three pinned versions
    let lock = SkillLock::read(&home.path().join("skills/root")).unwrap();
    assert_eq!(lock.locked.get("root").map(String::as_str), Some("0.1.0"));
    assert_eq!(lock.locked.get("dep-b").map(String::as_str), Some("1.2.0"));
    assert_eq!(lock.locked.get("dep-c").map(String::as_str), Some("1.0.0"));
    // installed_at — shape, not value
    chrono::DateTime::parse_from_rfc3339(&lock.installed_at).expect("rfc3339");

    // Trust store has all three (look up by name, not hash)
    let trust = SkillTrustStore::load(home.path()).unwrap();
    let names: HashSet<String> = trust.entries.values().map(|e| e.name.clone()).collect();
    assert_eq!(names, ["root", "dep-b", "dep-c"].iter().map(|s| s.to_string()).collect());
}

#[test]
fn cycle_propagates_error_and_leaves_clean_filesystem() {
    let home = tempfile::tempdir().unwrap();
    let reg = TestRegistry::new();
    reg.publish("alpha", "1.0.0", &[("beta", "*")]);
    reg.publish("beta",  "1.0.0", &[("alpha", "*")]);
    reg.commit();

    let err = cmd_install(home.path(), &reg.url(), "alpha").unwrap_err();
    assert!(err.to_string().to_lowercase().contains("cyclic"),
            "expected cyclic-dependency error, got: {err}");

    // Resolver fails before any disk install — neither alpha nor beta should be installed.
    assert!(!home.path().join("skills/alpha/skill.yaml").exists());
    assert!(!home.path().join("skills/beta/skill.yaml").exists());
    assert!(!home.path().join("skills/alpha/skill.lock").exists());
}

#[test]
fn re_install_is_idempotent_and_rewrites_lock() {
    let home = tempfile::tempdir().unwrap();
    let reg = TestRegistry::new();
    reg.publish("solo", "1.0.0", &[]);
    reg.commit();

    cmd_install(home.path(), &reg.url(), "solo").unwrap();
    let lock1 = SkillLock::read(&home.path().join("skills/solo")).unwrap();

    // Second call: must not panic, must produce equivalent end state.
    cmd_install(home.path(), &reg.url(), "solo").unwrap();
    let lock2 = SkillLock::read(&home.path().join("skills/solo")).unwrap();

    assert_eq!(lock1.locked, lock2.locked);
    // installed_at may differ (it's a fresh timestamp); only the locked map matters here.
}
```

- [ ] **7.3** **Pre-flight**: confirm git is on PATH (CI images all have it; this is mostly a local-dev sanity check). On Windows runners, the test relies on `file://` URLs being accepted by `git clone` — they are, but the path translation matters; if the test fails on Windows with a path issue, use `url::Url::from_file_path` instead of `format!("file://{}", ...)`. Add `url = "2"` to `mur-core/[dev-dependencies]` if needed.

- [ ] **7.4** Commit:
  ```bash
  cargo test -p mur-core --test skill_install_recursive_e2e
  git add mur-core/tests/skill_install_recursive_e2e.rs mur-core/tests/common/mod.rs
  git commit -m "test(skill): e2e recursive install over file:// git registry"
  ```

---

## Self-Review

**Spec §14 M3 coverage (M3a slice only):**

| M3 item | Status | Task |
|---|---|---|
| `requires:` resolution + transitive install | ✅ | T4 + T5 |
| Security audit of transitive deps (`scan_skill` runs on every node) | ✅ | T5.1 `install_resolved_node` |
| `skill.lock` file | ✅ | T2 + T5 |
| Circular dependency detection | ✅ | T4 (`State.on_stack`) |
| Version constraints (semver) | ✅ | T1 |
| `mur skill update` bumps locked versions | ✅ | T5.3 |
| `mur skill generate --from-session` | ⛔ deferred to **M3b** | — |
| `mur skill from-pattern` | ⛔ deferred to **M3b** | — |
| Self-evolution loop (Failure Analyzer + Skill Optimizer) | ⛔ deferred to **M3c** | — |
| `evolution_log` field on manifest | ⛔ deferred to **M3c** | — |

**Risks called out explicitly:**

1. **Lock file scope**: spec says "checked into version control" — implication is that the `skill.lock` is part of the skill repo. M3a writes it to the install-side `~/.mur/skills/<name>/skill.lock`. If skill authors want to commit a lock alongside their `skill.yaml`, that's a separate workflow (`mur skill publish` extension); flagged but not addressed in this milestone.

2. **Registry source override**: `cmd_install` is split into a pure `cmd_install(home, registry_url, source)` plus a `cmd_install_cli` shim that resolves `MUR_HOME` + `MUR_SKILL_REGISTRY_URL` from env. The env var is a real product feature (private mirrors / enterprise air-gapped / dev registries), not a test affordance. Tests call the pure form directly with a `file://` URL pointing at a tempdir git repo — see T7.

3. **Update semantics**: `mur skill update <name>` rewrites the lock to the registry's current best match. M3a does NOT add an `--only` flag for partial updates. Defer if asked.

4. **Diamond + version disagreement**: T4 currently rejects with `Conflict`. A future SAT-style solver could backtrack to find a version satisfying both branches, but it's overkill for current scale and not in scope.

5. **No `published_at` ordering**: ties broken by `semver::Version` ordering only, which is fine for the spec set.

**Placeholder scan:** clean. The only `// TODO` candidate is the offline-registry env var, called out in Task 3 + 7 with explicit instructions.

---

## Execution Handoff

Plan saved to `docs/superpowers/plans/2026-05-25-mur-skill-ecosystem-m3a.md`.

Suggested branch: `feat/skill-ecosystem-m3a`. Two execution options:

1. **Subagent-driven (recommended)** — one subagent per task with reviews; lock down resolver logic in T4 before chaining T5/T7.
2. **Inline executing-plans** — checkpoints after T2 (data types), T4 (resolver), T7 (E2E).

No outstanding decisions — `cmd_install` split + `file://` test registry are both committed in the plan above.
