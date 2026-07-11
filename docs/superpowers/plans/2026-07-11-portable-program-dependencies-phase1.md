# Portable Program Dependencies — Phase 1 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let any shared MUR artifact (agent / fleet / skill / MCP) declare the external programs it needs, detect them cross-platform, report what's missing, and install MUR-curated ones under a consent gate — so a recipient of a shared bundle isn't silently broken.

**Architecture:** A `ProgramDep` declaration (`requires_programs`) is parsed from four sites (skill.yaml, MCP entry, agent profile.yaml, fleet.yaml). A cross-platform `detect` checks presence. A MUR-owned curated `registry` maps a key → per-`(os,arch)` recipe (url + pinned sha256 + install path). An `installer` downloads → verifies sha256 → writes atomically → chmods → extracts. `aggregate` collects deps across an artifact; `doctor` reports; `install-deps` installs curated deps with per-item consent. Detection is always-on and safe; installation is consent-gated (Phase 1 = curated only; unknown deps are detect-and-guide).

**Tech Stack:** Rust (edition 2024). Reuse existing deps: `sha2 = "0.10"`, `tar = "0.4"`, `flate2 = "1"` (mur-common + mur-core), `reqwest` (mur-core). Platform key from `std::env::consts::{ARCH, OS}`.

**Spec:** `docs/superpowers/specs/2026-07-11-portable-program-dependencies-design.md`

## Global Constraints

- **No hardcoded values** — curated recipe URLs/checksums/paths live in the embedded registry manifest, never inline in logic.
- **Cross-platform** — detect + install work on macOS (aarch64/x86_64), Linux (aarch64/x86_64), Windows (x86_64). Recipes keyed by `<arch>-<os>` (e.g. `aarch64-macos`).
- **Fail-safe / non-blocking** — a missing dependency NEVER hard-fails import or run; it warns and the artifact degrades.
- **Consent-gated installs** — nothing downloads/installs without an explicit `install-deps` invocation; per-item `[y/N]` unless `--yes`.
- **Integrity** — every curated download is verified against its pinned SHA-256 before it is written or made executable; mismatch fails closed (no file written).
- **Reuse** — extend `mur agent doctor` (`cmd/agent/doctor.rs`); Phase 2 (out of scope) reuses `publisher_trust.rs`.
- Files ≤ 800 lines; user-facing brand "MUR"; comments English.
- **Build/test env for mur-core:** `export ORT_STRATEGY=download; export MUR_WEB_DIST=$HOME/Projects/mur-web/dist; export PATH="$HOME/.rustup/toolchains/stable-aarch64-apple-darwin/bin:$PATH"`. mur-common needs no special env.

## File Structure

**mur-common (types, detect, registry — no I/O beyond detect's local checks):**
- Create `mur-common/src/deps/mod.rs` — `ProgramDep`, `DetectMethod`, `DepStatus`, `current_platform()`.
- Create `mur-common/src/deps/detect.rs` — `detect(dep, mur_home) -> DepStatus`.
- Create `mur-common/src/deps/registry.rs` — `CuratedRecipe`, `RecipeMember`, `recipe(key, platform) -> Option<CuratedRecipe>`.
- Create `mur-common/src/deps/registry_manifest.yaml` — embedded seed (lightpanda, obscura).
- Modify `mur-common/src/lib.rs` — `pub mod deps;`.
- Modify `mur-common/src/skill/manifest.rs` (`SkillManifest`), `mur-common/src/agent.rs` (`McpServerEntry`, `AgentProfile`), `mur-common/src/fleet.rs` (`Fleet`) — add `requires_programs: Vec<ProgramDep>`.

**mur-core (aggregate, installer, commands, integration):**
- Create `mur-core/src/cmd/deps/mod.rs` — `aggregate_agent`, `aggregate_fleet`, `AggregatedDep`.
- Create `mur-core/src/cmd/deps/installer.rs` — `verify_and_place(bytes, recipe, mur_home)`, `download(url)`, `install(recipe, mur_home)`.
- Create `mur-core/src/cmd/deps/doctor.rs` — `report(deps, mur_home) -> DoctorReport` + printer.
- Create `mur-core/src/cmd/deps/install.rs` — `cmd_install_deps(...)`.
- Modify `mur-core/src/cmd/agent/doctor.rs` — call the deps report.
- Modify `mur-core/src/cli/actions.rs` + `mur-core/src/dispatch.rs` — `fleet doctor`, `agent install-deps`, `fleet install-deps`.
- Modify fleet import + run/start entrypoints — non-blocking preflight report.

---

### Task 1: `ProgramDep` + `DetectMethod` types

**Files:**
- Create: `mur-common/src/deps/mod.rs`
- Modify: `mur-common/src/lib.rs` (add `pub mod deps;`)

**Interfaces:**
- Produces: `ProgramDep { name: String, detect: DetectMethod, reason: String, hint: Option<String>, registry: Option<String> }`; `DetectMethod` enum `{ File{file: String}, Command{command: String}, Version{version: VersionCheck} }` (serde-tagged by field); `VersionCheck { command: String, min: String }`; `DepStatus` enum `{ Present, Missing, PresentWrongVersion{found: String} }`; `fn current_platform() -> String`.

- [ ] **Step 1: Write the failing test**

Create `mur-common/src/deps/mod.rs` with only the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn program_dep_parses_all_detect_methods() {
        let y = r#"
- name: lightpanda
  detect: { file: "~/.mur/aura/lightpanda" }
  reason: "render tier"
  hint: "https://lightpanda.io/download"
  registry: lightpanda
- name: gh
  detect: { command: "gh" }
  reason: "github ops"
- name: node
  detect: { version: { command: "node --version", min: "18.0.0" } }
  reason: "js runtime"
"#;
        let deps: Vec<ProgramDep> = serde_yaml::from_str(y).unwrap();
        assert_eq!(deps.len(), 3);
        assert_eq!(deps[0].name, "lightpanda");
        assert!(matches!(&deps[0].detect, DetectMethod::File { file } if file == "~/.mur/aura/lightpanda"));
        assert_eq!(deps[0].registry.as_deref(), Some("lightpanda"));
        assert!(matches!(&deps[1].detect, DetectMethod::Command { command } if command == "gh"));
        assert!(deps[1].hint.is_none());
        assert!(matches!(&deps[2].detect, DetectMethod::Version { version } if version.min == "18.0.0"));
    }

    #[test]
    fn current_platform_is_arch_dash_os() {
        let p = current_platform();
        assert!(p.contains('-'));
        // arch and os are non-empty
        let (arch, os) = p.split_once('-').unwrap();
        assert!(!arch.is_empty() && !os.is_empty());
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common deps::tests::program_dep_parses -- --nocapture`
Expected: FAIL (types undefined).

- [ ] **Step 3: Implement the types**

Prepend to `mur-common/src/deps/mod.rs`:

```rust
//! Portable program dependencies: declaring, detecting, and (curated)
//! installing the external programs a shared MUR artifact needs.
//! See docs/superpowers/specs/2026-07-11-portable-program-dependencies-design.md

use serde::{Deserialize, Serialize};

pub mod detect;
pub mod registry;

/// One external-program requirement declared by a skill / MCP entry / agent
/// profile / fleet. Data only — no I/O.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProgramDep {
    /// Stable lowercase identifier (also the registry key when `registry` is None).
    pub name: String,
    /// How to check whether the program is present.
    pub detect: DetectMethod,
    /// Human-readable "why this is needed", shown in the doctor report.
    pub reason: String,
    /// Manual-install guidance (URL/command). Display only — never executed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
    /// Optional key into MUR's curated registry (enables auto-install).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry: Option<String>,
}

/// Exactly one detection method (serde picks the arm by which field is present).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DetectMethod {
    /// A file exists at this (tilde/`$MUR_HOME`-expanded) path.
    File { file: String },
    /// A command resolves on `PATH`.
    Command { command: String },
    /// A command's reported version is `>= min`.
    Version { version: VersionCheck },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VersionCheck {
    /// Full command line to run, e.g. "node --version".
    pub command: String,
    /// Minimum acceptable semver, e.g. "18.0.0".
    pub min: String,
}

/// Result of detecting a `ProgramDep`.
#[derive(Debug, Clone, PartialEq)]
pub enum DepStatus {
    Present,
    Missing,
    PresentWrongVersion { found: String },
}

/// Current platform key, `<arch>-<os>` (e.g. "aarch64-macos"), matching the
/// curated registry's per-platform keys. Uses the compiled target via
/// `std::env::consts` (no subprocess).
pub fn current_platform() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common deps:: -- --nocapture`
Expected: PASS. (`detect`/`registry` submodules are declared but empty — create empty files so it compiles: `mur-common/src/deps/detect.rs` and `registry.rs` with just `//! stub` for now; Tasks 3–4 fill them.)

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/deps/mod.rs mur-common/src/deps/detect.rs mur-common/src/deps/registry.rs mur-common/src/lib.rs
git commit -m "feat(deps): ProgramDep + DetectMethod types + current_platform"
```

---

### Task 2: `requires_programs` on the four declaration sites

**Files:**
- Modify: `mur-common/src/skill/manifest.rs` (`SkillManifest`)
- Modify: `mur-common/src/agent.rs` (`McpServerEntry`, `AgentProfile`)
- Modify: `mur-common/src/fleet.rs` (`Fleet`)

**Interfaces:**
- Consumes: `ProgramDep` (Task 1).
- Produces: `SkillManifest.requires_programs: Vec<ProgramDep>`, `McpServerEntry.requires_programs`, `AgentProfile.requires_programs`, `Fleet.requires_programs` — all `#[serde(default)]` (absent → empty).

- [ ] **Step 1: Write the failing test** (in `mur-common/src/agent.rs` test module)

```rust
#[test]
fn mcp_entry_parses_requires_programs_and_defaults_empty() {
    let with = r#"
name: research-gateway
command: mur-research-gateway
requires_programs:
  - name: lightpanda
    detect: { file: "~/.mur/aura/lightpanda" }
    reason: "render tier"
    registry: lightpanda
"#;
    let e: crate::agent::McpServerEntry = serde_yaml::from_str(with).unwrap();
    assert_eq!(e.requires_programs.len(), 1);
    assert_eq!(e.requires_programs[0].name, "lightpanda");

    // Absent block → empty (back-compat).
    let without = "name: x\ncommand: y\n";
    let e2: crate::agent::McpServerEntry = serde_yaml::from_str(without).unwrap();
    assert!(e2.requires_programs.is_empty());
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common mcp_entry_parses_requires_programs`
Expected: FAIL (field missing).

- [ ] **Step 3: Add the field to all four structs**

In each struct add (with the crate's import `use crate::deps::ProgramDep;` at the top of the file if not already imported):

```rust
    /// External programs this artifact needs at runtime (portable-deps spec).
    /// Absent → empty; resolved by `mur agent/fleet doctor` + `install-deps`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_programs: Vec<ProgramDep>,
```

Add to: `SkillManifest` (manifest.rs), `McpServerEntry` and `AgentProfile` (agent.rs), `Fleet` (fleet.rs). For any struct that has a hand-written `Default` impl or struct-literal constructors in non-test code, add `requires_programs: Vec::new()` there (grep each file for `StructName {` literal constructions and update them; the `#[serde(default)]` covers deserialization but not literal constructors).

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common 2>&1 | tail -5`
Expected: PASS (new + existing). Fix any struct-literal construction sites the compiler flags.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/manifest.rs mur-common/src/agent.rs mur-common/src/fleet.rs
git commit -m "feat(deps): requires_programs on skill/MCP/profile/fleet"
```

---

### Task 3: Cross-platform detection

**Files:**
- Modify: `mur-common/src/deps/detect.rs`

**Interfaces:**
- Consumes: `ProgramDep`, `DetectMethod`, `DepStatus` (Task 1).
- Produces: `fn detect(dep: &ProgramDep, mur_home: &std::path::Path) -> DepStatus`; helpers `fn expand_path(raw: &str, mur_home: &Path) -> PathBuf`, `fn command_on_path(cmd: &str) -> bool`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::{DetectMethod, DepStatus, ProgramDep};
    use std::path::Path;

    fn dep(detect: DetectMethod) -> ProgramDep {
        ProgramDep { name: "x".into(), detect, reason: "r".into(), hint: None, registry: None }
    }

    #[test]
    fn file_detect_present_and_absent() {
        let tmp = std::env::temp_dir().join(format!("murdep_{}", std::process::id()));
        std::fs::create_dir_all(tmp.join("aura")).unwrap();
        std::fs::write(tmp.join("aura/lightpanda"), b"x").unwrap();
        // mur_home-relative via the literal "aura/lightpanda" is expanded against mur_home
        let present = dep(DetectMethod::File { file: "aura/lightpanda".into() });
        assert_eq!(detect(&present, &tmp), DepStatus::Present);
        let absent = dep(DetectMethod::File { file: "aura/nope".into() });
        assert_eq!(detect(&absent, &tmp), DepStatus::Missing);
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn command_detect_missing_for_bogus() {
        let d = dep(DetectMethod::Command { command: "definitely-not-a-real-binary-xyz".into() });
        assert_eq!(detect(&d, Path::new("/")), DepStatus::Missing);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common deps::detect -- --nocapture`
Expected: FAIL (`detect` undefined).

- [ ] **Step 3: Implement detection**

Replace `mur-common/src/deps/detect.rs`:

```rust
//! Cross-platform, side-effect-free detection of a `ProgramDep`.

use crate::deps::{DepStatus, DetectMethod, ProgramDep, VersionCheck};
use std::path::{Path, PathBuf};

/// Detect whether `dep` is present. Never installs; the `version` arm runs a
/// bounded subprocess only to read a version string.
pub fn detect(dep: &ProgramDep, mur_home: &Path) -> DepStatus {
    match &dep.detect {
        DetectMethod::File { file } => {
            if expand_path(file, mur_home).exists() {
                DepStatus::Present
            } else {
                DepStatus::Missing
            }
        }
        DetectMethod::Command { command } => {
            if command_on_path(command) {
                DepStatus::Present
            } else {
                DepStatus::Missing
            }
        }
        DetectMethod::Version { version } => detect_version(version),
    }
}

/// Expand `~`/`$MUR_HOME` and resolve a bare relative path against `mur_home`
/// (so `aura/lightpanda` and `~/.mur/aura/lightpanda` both work).
pub fn expand_path(raw: &str, mur_home: &Path) -> PathBuf {
    let s = raw.trim();
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest);
        }
    }
    let p = PathBuf::from(s);
    if p.is_absolute() { p } else { mur_home.join(p) }
}

/// True if `cmd` resolves to an executable on `PATH` (Windows: also tries
/// PATHEXT-style `.exe`/`.cmd`).
pub fn command_on_path(cmd: &str) -> bool {
    let path = match std::env::var_os("PATH") {
        Some(p) => p,
        None => return false,
    };
    let exts: &[&str] = if cfg!(windows) { &["", ".exe", ".cmd", ".bat"] } else { &[""] };
    for dir in std::env::split_paths(&path) {
        for ext in exts {
            let candidate = dir.join(format!("{cmd}{ext}"));
            if candidate.is_file() {
                return true;
            }
        }
    }
    false
}

fn detect_version(v: &VersionCheck) -> DepStatus {
    let mut parts = v.command.split_whitespace();
    let bin = match parts.next() {
        Some(b) => b,
        None => return DepStatus::Missing,
    };
    let args: Vec<&str> = parts.collect();
    let out = std::process::Command::new(bin).args(&args).output();
    let out = match out {
        Ok(o) => o,
        Err(_) => return DepStatus::Missing,
    };
    let text = String::from_utf8_lossy(&out.stdout);
    match first_semver(&text) {
        Some(found) if semver_ge(&found, &v.min) => DepStatus::Present,
        Some(found) => DepStatus::PresentWrongVersion { found },
        // present but unparseable → report as present-unknown (never auto-action)
        None => DepStatus::PresentWrongVersion { found: "unknown".into() },
    }
}

/// First `MAJOR.MINOR.PATCH` (or `MAJOR.MINOR`) run in `s`.
fn first_semver(s: &str) -> Option<String> {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i].is_ascii_digit() {
            let start = i;
            while i < bytes.len() && (bytes[i].is_ascii_digit() || bytes[i] == b'.') {
                i += 1;
            }
            let cand = &s[start..i];
            if cand.contains('.') {
                return Some(cand.trim_end_matches('.').to_string());
            }
        } else {
            i += 1;
        }
    }
    None
}

fn semver_ge(a: &str, b: &str) -> bool {
    let pa: Vec<u64> = a.split('.').filter_map(|x| x.parse().ok()).collect();
    let pb: Vec<u64> = b.split('.').filter_map(|x| x.parse().ok()).collect();
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return x > y;
        }
    }
    true
}
```

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-common deps::detect`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/deps/detect.rs
git commit -m "feat(deps): cross-platform detect (file/command/version)"
```

---

### Task 4: Curated registry + seed manifest

**Files:**
- Modify: `mur-common/src/deps/registry.rs`
- Create: `mur-common/src/deps/registry_manifest.yaml`

**Interfaces:**
- Produces: `CuratedRecipe { description: String, url: String, sha256: String, install_to: String, executable: bool, archive: Option<ArchiveSpec> }`; `ArchiveSpec { members: Vec<RecipeMember> }`; `RecipeMember { path_in_archive: String, install_to: String, executable: bool, sha256: Option<String> }`; `fn recipe(key: &str, platform: &str) -> Option<CuratedRecipe>`; `fn is_curated(key: &str) -> bool`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lightpanda_recipe_resolves_per_platform() {
        let r = recipe("lightpanda", "aarch64-macos");
        assert!(r.is_some(), "lightpanda/aarch64-macos should exist");
        let r = r.unwrap();
        assert!(!r.url.is_empty() && r.sha256.len() == 64);
        assert!(r.install_to.starts_with("aura/"));
        assert!(is_curated("lightpanda"));
        assert!(!is_curated("totally-unknown-key"));
        assert!(recipe("lightpanda", "sparc-solaris").is_none());
    }

    #[test]
    fn obscura_recipe_is_an_archive_with_two_members() {
        let r = recipe("obscura", "aarch64-macos").expect("obscura/aarch64-macos");
        let a = r.archive.expect("obscura ships an archive");
        assert_eq!(a.members.len(), 2, "obscura + obscura-worker");
        assert!(a.members.iter().any(|m| m.install_to == "aura/obscura"));
        assert!(a.members.iter().any(|m| m.install_to == "aura/obscura-worker"));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-common deps::registry -- --nocapture`
Expected: FAIL.

- [ ] **Step 3: Write the embedded manifest**

Create `mur-common/src/deps/registry_manifest.yaml`. **Fetch the real per-platform SHA-256 at implementation time** with `curl -sL <url> | shasum -a 256` for each asset (release assets are immutable per tag). Lightpanda releases: `https://github.com/lightpanda-io/browser/releases` (pick a pinned tag, per-platform asset names). Obscura releases (verified this session): `https://github.com/h4ckf0r0day/obscura/releases/download/v0.1.9/obscura-<arch>-<os>.tar.gz` — the `aarch64-macos` tarball sha256 is `e470007e7d0be4f96f15ba00b21ea0e90dd26df03b3bea4bb547f406072fef5e` (verified 2026-07-10); fetch the other three. Shape:

```yaml
lightpanda:
  description: "Lightpanda native headless browser (render tier)"
  platforms:
    aarch64-macos:  { url: "https://github.com/lightpanda-io/browser/releases/download/<TAG>/lightpanda-aarch64-macos",  sha256: "<FETCH>", install_to: "aura/lightpanda", executable: true }
    x86_64-macos:   { url: "https://github.com/lightpanda-io/browser/releases/download/<TAG>/lightpanda-x86_64-macos",   sha256: "<FETCH>", install_to: "aura/lightpanda", executable: true }
    aarch64-linux:  { url: "https://github.com/lightpanda-io/browser/releases/download/<TAG>/lightpanda-aarch64-linux",  sha256: "<FETCH>", install_to: "aura/lightpanda", executable: true }
    x86_64-linux:   { url: "https://github.com/lightpanda-io/browser/releases/download/<TAG>/lightpanda-x86_64-linux",   sha256: "<FETCH>", install_to: "aura/lightpanda", executable: true }
obscura:
  description: "Obscura embedded-V8 browser (render tier); ships two binaries"
  platforms:
    aarch64-macos:  { url: "https://github.com/h4ckf0r0day/obscura/releases/download/v0.1.9/obscura-aarch64-macos.tar.gz", sha256: "e470007e7d0be4f96f15ba00b21ea0e90dd26df03b3bea4bb547f406072fef5e", archive: { members: [ { path_in_archive: "obscura", install_to: "aura/obscura", executable: true }, { path_in_archive: "obscura-worker", install_to: "aura/obscura-worker", executable: true } ] } }
    x86_64-macos:   { url: "https://github.com/h4ckf0r0day/obscura/releases/download/v0.1.9/obscura-x86_64-macos.tar.gz",  sha256: "<FETCH>", archive: { members: [ { path_in_archive: "obscura", install_to: "aura/obscura", executable: true }, { path_in_archive: "obscura-worker", install_to: "aura/obscura-worker", executable: true } ] } }
    aarch64-linux:  { url: "https://github.com/h4ckf0r0day/obscura/releases/download/v0.1.9/obscura-aarch64-linux.tar.gz", sha256: "<FETCH>", archive: { members: [ { path_in_archive: "obscura", install_to: "aura/obscura", executable: true }, { path_in_archive: "obscura-worker", install_to: "aura/obscura-worker", executable: true } ] } }
    x86_64-linux:   { url: "https://github.com/h4ckf0r0day/obscura/releases/download/v0.1.9/obscura-x86_64-linux.tar.gz",  sha256: "<FETCH>", archive: { members: [ { path_in_archive: "obscura", install_to: "aura/obscura", executable: true }, { path_in_archive: "obscura-worker", install_to: "aura/obscura-worker", executable: true } ] } }
```

Note: `install_to` for a bare file uses the recipe-level `install_to`; an archive uses per-member `install_to` (no recipe-level one). Windows lightpanda/obscura assets aren't published → omit (unknown platform → `recipe()` returns None → detect-and-guide via hint).

- [ ] **Step 4: Implement the accessor**

Replace `mur-common/src/deps/registry.rs`:

```rust
//! MUR-curated program registry. URLs + pinned SHA-256 are MUR-owned, so a
//! shared bundle can only *reference* a key — it cannot substitute the source.

use serde::Deserialize;
use std::collections::BTreeMap;

const MANIFEST: &str = include_str!("registry_manifest.yaml");

#[derive(Debug, Clone, Deserialize)]
pub struct CuratedRecipe {
    #[serde(default)]
    pub description: String,
    pub url: String,
    pub sha256: String,
    /// For a bare-binary recipe; `None` when `archive` is set.
    #[serde(default)]
    pub install_to: Option<String>,
    #[serde(default)]
    pub executable: bool,
    /// For a multi-file tarball (e.g. obscura's two binaries).
    #[serde(default)]
    pub archive: Option<ArchiveSpec>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArchiveSpec {
    pub members: Vec<RecipeMember>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RecipeMember {
    pub path_in_archive: String,
    pub install_to: String,
    #[serde(default)]
    pub executable: bool,
}

#[derive(Debug, Deserialize)]
struct Entry {
    #[serde(default)]
    description: String,
    platforms: BTreeMap<String, PlatformRaw>,
}

#[derive(Debug, Deserialize)]
struct PlatformRaw {
    url: String,
    sha256: String,
    #[serde(default)]
    install_to: Option<String>,
    #[serde(default)]
    executable: bool,
    #[serde(default)]
    archive: Option<ArchiveSpec>,
}

fn load() -> BTreeMap<String, Entry> {
    serde_yaml::from_str(MANIFEST).expect("embedded registry_manifest.yaml is valid")
}

/// Resolve a curated recipe for `key` on `platform` (`<arch>-<os>`).
pub fn recipe(key: &str, platform: &str) -> Option<CuratedRecipe> {
    let map = load();
    let entry = map.get(key)?;
    let p = entry.platforms.get(platform)?;
    Some(CuratedRecipe {
        description: entry.description.clone(),
        url: p.url.clone(),
        sha256: p.sha256.clone(),
        install_to: p.install_to.clone(),
        executable: p.executable,
        archive: p.archive.clone(),
    })
}

/// True if `key` names a curated program (on any platform).
pub fn is_curated(key: &str) -> bool {
    load().contains_key(key)
}
```

- [ ] **Step 5: Run to verify it passes**

Run: `cargo test -p mur-common deps::registry`
Expected: PASS. (If the `<FETCH>` placeholders are still literal strings, the `sha256.len()==64` assert will fail for non-macos-arm platforms — the implementer MUST fetch the real checksums before this passes. The test intentionally forces this.)

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/deps/registry.rs mur-common/src/deps/registry_manifest.yaml
git commit -m "feat(deps): curated registry + seed recipes (lightpanda/obscura)"
```

---

### Task 5: Installer — download, verify, place, extract

**Files:**
- Create: `mur-core/src/cmd/deps/mod.rs` (module decl + re-exports; `aggregate` added in Task 6)
- Create: `mur-core/src/cmd/deps/installer.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub mod deps;`)

**Interfaces:**
- Consumes: `mur_common::deps::registry::{CuratedRecipe, RecipeMember}`.
- Produces: `fn verify_and_place(bytes: &[u8], recipe: &CuratedRecipe, mur_home: &Path) -> Result<Vec<PathBuf>>` (verifies sha256, extracts/writes, chmods; returns installed paths); `async fn download(url: &str) -> Result<Vec<u8>>`; `async fn install(recipe: &CuratedRecipe, mur_home: &Path) -> Result<Vec<PathBuf>>` (download then verify_and_place).

- [ ] **Step 1: Write the failing test** (`installer.rs` test module — pure `verify_and_place`, no network)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::deps::registry::CuratedRecipe;
    use sha2::{Digest, Sha256};

    fn sha_hex(b: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(b);
        hex::encode(h.finalize())
    }

    #[test]
    fn bare_binary_sha_match_installs_and_chmods() {
        let tmp = std::env::temp_dir().join(format!("murinst_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let bytes = b"#!/bin/sh\necho hi\n";
        let recipe = CuratedRecipe {
            description: "t".into(),
            url: "unused".into(),
            sha256: sha_hex(bytes),
            install_to: Some("aura/lightpanda".into()),
            executable: true,
            archive: None,
        };
        let placed = verify_and_place(bytes, &recipe, &tmp).unwrap();
        assert_eq!(placed, vec![tmp.join("aura/lightpanda")]);
        assert!(tmp.join("aura/lightpanda").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(tmp.join("aura/lightpanda")).unwrap().permissions().mode();
            assert!(mode & 0o111 != 0, "executable bit set");
        }
        std::fs::remove_dir_all(&tmp).ok();
    }

    #[test]
    fn sha_mismatch_fails_closed_no_file_written() {
        let tmp = std::env::temp_dir().join(format!("murinstbad_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let recipe = CuratedRecipe {
            description: "t".into(), url: "u".into(),
            sha256: "0".repeat(64), // wrong
            install_to: Some("aura/x".into()), executable: true, archive: None,
        };
        let r = verify_and_place(b"payload", &recipe, &tmp);
        assert!(r.is_err(), "mismatch must error");
        assert!(!tmp.join("aura/x").exists(), "no file on mismatch");
        std::fs::remove_dir_all(&tmp).ok();
    }
}
```

(Requires `hex` — it's already a workspace dep via `sha2` usage elsewhere; if not, add `hex = "0.4"` to mur-core dev-deps. Verify with `grep '^hex' mur-core/Cargo.toml`.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core deps::installer -- --nocapture` (with the mur-core env exports)
Expected: FAIL.

- [ ] **Step 3: Implement the installer**

Create `mur-core/src/cmd/deps/installer.rs`:

```rust
//! Download a curated recipe, verify its pinned SHA-256, and place binaries.
//! `verify_and_place` is the security-critical, network-free core (unit-tested);
//! `download` is a thin reqwest wrapper.

use anyhow::{Context, Result, bail};
use mur_common::deps::registry::CuratedRecipe;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::path::{Path, PathBuf};

fn sha256_hex(bytes: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(bytes);
    hex::encode(h.finalize())
}

/// Verify `bytes` against `recipe.sha256`, then place file(s) under `mur_home`.
/// Fails closed on mismatch (nothing written). Returns installed absolute paths.
pub fn verify_and_place(bytes: &[u8], recipe: &CuratedRecipe, mur_home: &Path) -> Result<Vec<PathBuf>> {
    let got = sha256_hex(bytes);
    if !got.eq_ignore_ascii_case(&recipe.sha256) {
        bail!("sha256 mismatch: expected {}, got {}", recipe.sha256, got);
    }
    match &recipe.archive {
        None => {
            let rel = recipe
                .install_to
                .as_ref()
                .context("bare recipe missing install_to")?;
            let dst = mur_home.join(rel);
            place_file(&dst, bytes, recipe.executable)?;
            Ok(vec![dst])
        }
        Some(archive) => {
            // gzip tar; extract only declared members.
            let gz = flate2::read::GzDecoder::new(bytes);
            let mut tar = tar::Archive::new(gz);
            let mut wanted: std::collections::BTreeMap<&str, _> = archive
                .members
                .iter()
                .map(|m| (m.path_in_archive.as_str(), m))
                .collect();
            let mut placed = Vec::new();
            for entry in tar.entries().context("read tar")? {
                let mut entry = entry.context("tar entry")?;
                let path = entry.path().context("tar path")?.to_string_lossy().to_string();
                let base = path.rsplit('/').next().unwrap_or(&path);
                if let Some(member) = wanted.remove(base) {
                    let mut buf = Vec::new();
                    entry.read_to_end(&mut buf).context("read member")?;
                    let dst = mur_home.join(&member.install_to);
                    place_file(&dst, &buf, member.executable)?;
                    placed.push(dst);
                }
            }
            if !wanted.is_empty() {
                bail!("archive missing members: {:?}", wanted.keys().collect::<Vec<_>>());
            }
            Ok(placed)
        }
    }
}

/// Atomic write (temp + rename) + optional +x. Creates parent dirs.
fn place_file(dst: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    if let Some(parent) = dst.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("mkdir {}", parent.display()))?;
    }
    let tmp = dst.with_extension("mur-tmp");
    std::fs::write(&tmp, bytes).with_context(|| format!("write {}", tmp.display()))?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        let mut perm = std::fs::metadata(&tmp)?.permissions();
        perm.set_mode(0o755);
        std::fs::set_permissions(&tmp, perm)?;
    }
    let _ = executable; // silence unused on non-unix
    std::fs::rename(&tmp, dst).with_context(|| format!("rename to {}", dst.display()))?;
    Ok(())
}

/// Fetch the recipe URL into memory (bounded read via reqwest).
pub async fn download(url: &str) -> Result<Vec<u8>> {
    let resp = reqwest::Client::new()
        .get(url)
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("status for {url}"))?;
    Ok(resp.bytes().await.context("read body")?.to_vec())
}

/// Download + verify + place.
pub async fn install(recipe: &CuratedRecipe, mur_home: &Path) -> Result<Vec<PathBuf>> {
    let bytes = download(&recipe.url).await?;
    verify_and_place(&bytes, recipe, mur_home)
}
```

Create `mur-core/src/cmd/deps/mod.rs`:

```rust
//! Portable program dependencies: aggregate declarations, report, install.
pub mod installer;
```

Add `pub mod deps;` under `cmd/mod.rs`. If `hex` isn't a mur-core dep, add `hex = "0.4"` to `[dependencies]`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core deps::installer`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deps/ mur-core/src/cmd/mod.rs mur-core/Cargo.toml
git commit -m "feat(deps): installer — verify_and_place (sha256, atomic, chmod, extract)"
```

---

### Task 6: Aggregate declarations across an artifact

**Files:**
- Modify: `mur-core/src/cmd/deps/mod.rs`

**Interfaces:**
- Consumes: `AgentProfile`, `Fleet`, `SkillManifest`, `McpServerEntry` (their `requires_programs`); the profile loader + fleet store + skill loader already in mur-core.
- Produces: `struct AggregatedDep { dep: ProgramDep, sources: Vec<String> }`; `fn aggregate_agent(mur_home: &Path, agent: &str) -> Result<Vec<AggregatedDep>>`; `fn aggregate_fleet(mur_home: &Path, fleet: &str) -> Result<Vec<AggregatedDep>>`. Dedup by `dep.name`, merging `sources`. Synthesizes a `Command`-detect dep for every mounted MCP `command` not already declared.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod agg_tests {
    use super::*;
    use mur_common::deps::{DetectMethod, ProgramDep};

    fn pd(name: &str) -> ProgramDep {
        ProgramDep { name: name.into(), detect: DetectMethod::Command { command: name.into() },
                     reason: "r".into(), hint: None, registry: None }
    }

    #[test]
    fn dedup_merges_sources_by_name() {
        let raw = vec![
            (pd("lightpanda"), "mcp:research-gateway".to_string()),
            (pd("lightpanda"), "skill:render".to_string()),
            (pd("gh"), "profile".to_string()),
        ];
        let out = dedup(raw);
        assert_eq!(out.len(), 2);
        let lp = out.iter().find(|a| a.dep.name == "lightpanda").unwrap();
        assert_eq!(lp.sources.len(), 2);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core deps::agg_tests`
Expected: FAIL (`dedup`/`AggregatedDep` undefined).

- [ ] **Step 3: Implement aggregate + dedup**

Append to `mur-core/src/cmd/deps/mod.rs`:

```rust
use anyhow::Result;
use mur_common::deps::{DetectMethod, ProgramDep};
use std::path::Path;

pub mod doctor;
pub mod install;

/// A declared dependency plus which parts of the artifact declared it.
#[derive(Debug, Clone)]
pub struct AggregatedDep {
    pub dep: ProgramDep,
    pub sources: Vec<String>,
}

/// Dedup `(dep, source)` pairs by `dep.name`, merging sources (first dep wins
/// for detect/registry — they should agree; sources accumulate).
pub fn dedup(raw: Vec<(ProgramDep, String)>) -> Vec<AggregatedDep> {
    let mut out: Vec<AggregatedDep> = Vec::new();
    for (dep, src) in raw {
        if let Some(existing) = out.iter_mut().find(|a| a.dep.name == dep.name) {
            if !existing.sources.contains(&src) {
                existing.sources.push(src);
            }
        } else {
            out.push(AggregatedDep { dep, sources: vec![src] });
        }
    }
    out
}

/// Collect an agent's declared program deps: its profile.requires_programs,
/// each mounted MCP entry's requires_programs, a synthesized `command`-detect
/// dep per mounted MCP whose `command` is a bare name (not an absolute path),
/// and each installed skill's requires_programs.
pub fn aggregate_agent(mur_home: &Path, agent: &str) -> Result<Vec<AggregatedDep>> {
    let profile = crate::store::agent_profile::load_profile(mur_home, agent)?;
    let mut raw: Vec<(ProgramDep, String)> = Vec::new();
    for d in &profile.requires_programs {
        raw.push((d.clone(), "profile".into()));
    }
    for mcp in &profile.mcp_servers {
        for d in &mcp.requires_programs {
            raw.push((d.clone(), format!("mcp:{}", mcp.name)));
        }
        // Synthesize a command dep for a bare MCP command binary.
        if !mcp.command.contains(std::path::MAIN_SEPARATOR) {
            raw.push((
                ProgramDep {
                    name: mcp.command.clone(),
                    detect: DetectMethod::Command { command: mcp.command.clone() },
                    reason: format!("MCP server {}", mcp.name),
                    hint: None,
                    registry: None,
                },
                format!("mcp-cmd:{}", mcp.name),
            ));
        }
    }
    Ok(dedup(raw))
}

/// Collect a fleet's deps: its fleet.yaml.requires_programs plus every member
/// agent's aggregate.
pub fn aggregate_fleet(mur_home: &Path, fleet: &str) -> Result<Vec<AggregatedDep>> {
    let f = crate::cmd::fleet::store::load_fleet(mur_home, fleet)?;
    let mut raw: Vec<(ProgramDep, String)> = Vec::new();
    for d in &f.requires_programs {
        raw.push((d.clone(), "fleet".into()));
    }
    for member in &f.members {
        if let Ok(member_deps) = aggregate_agent(mur_home, member) {
            for a in member_deps {
                raw.push((a.dep, format!("member:{member}")));
            }
        }
    }
    Ok(dedup(raw))
}
```

**Interface note for the implementer:** confirm the exact loader fn names —
`crate::store::agent_profile::load_profile` and `crate::cmd::fleet::store::load_fleet`
are the expected signatures; grep `fn load_profile`/`fn load_fleet` and adjust the
call to whatever the codebase actually exposes (e.g. it may be `load_profile_for_edit`
or take a `&Path` differently). Keep the aggregation logic identical.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core deps::agg_tests`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deps/mod.rs
git commit -m "feat(deps): aggregate program deps across profile/MCP/skills/fleet"
```

---

### Task 7: Doctor report

**Files:**
- Create: `mur-core/src/cmd/deps/doctor.rs`

**Interfaces:**
- Consumes: `AggregatedDep` (Task 6), `mur_common::deps::detect::detect`, `mur_common::deps::registry::is_curated`, `current_platform`.
- Produces: `enum Tier { Curated, Manual }`; `struct DepReportLine { name, reason, status: DepStatus, tier: Tier, hint: Option<String> }`; `fn build_report(deps: &[AggregatedDep], mur_home: &Path) -> Vec<DepReportLine>`; `fn print_report(lines: &[DepReportLine], install_cmd: &str)`; `fn missing_count(lines: &[DepReportLine]) -> usize`.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::deps::{DetectMethod, ProgramDep};
    use crate::cmd::deps::AggregatedDep;

    fn agg(name: &str, registry: Option<&str>, detect_file: &str) -> AggregatedDep {
        AggregatedDep {
            dep: ProgramDep {
                name: name.into(),
                detect: DetectMethod::File { file: detect_file.into() },
                reason: "render".into(), hint: Some("http://x".into()),
                registry: registry.map(|s| s.into()),
            },
            sources: vec!["mcp:gw".into()],
        }
    }

    #[test]
    fn report_marks_missing_and_tier() {
        let tmp = std::env::temp_dir().join(format!("murdoc_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        // lightpanda is curated + missing (file absent under tmp)
        let deps = vec![agg("lightpanda", Some("lightpanda"), "aura/lightpanda")];
        let lines = build_report(&deps, &tmp);
        assert_eq!(lines.len(), 1);
        assert_eq!(lines[0].status, mur_common::deps::DepStatus::Missing);
        assert!(matches!(lines[0].tier, Tier::Curated));
        assert_eq!(missing_count(&lines), 1);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core deps::doctor`
Expected: FAIL.

- [ ] **Step 3: Implement**

Create `mur-core/src/cmd/deps/doctor.rs`:

```rust
//! Read-only report: what declared programs are present/missing, and how to get them.

use crate::cmd::deps::AggregatedDep;
use mur_common::deps::detect::detect;
use mur_common::deps::registry::is_curated;
use mur_common::deps::DepStatus;
use std::path::Path;

#[derive(Debug, Clone, PartialEq)]
pub enum Tier {
    /// Installable via `install-deps` from MUR's curated registry.
    Curated,
    /// Detect-and-guide only (unknown/untrusted source) — Phase 1.
    Manual,
}

#[derive(Debug, Clone)]
pub struct DepReportLine {
    pub name: String,
    pub reason: String,
    pub status: DepStatus,
    pub tier: Tier,
    pub hint: Option<String>,
    pub sources: Vec<String>,
}

/// Detect each dep and classify its install tier.
pub fn build_report(deps: &[AggregatedDep], mur_home: &Path) -> Vec<DepReportLine> {
    deps.iter()
        .map(|a| {
            let key = a.dep.registry.as_deref().unwrap_or(&a.dep.name);
            let tier = if is_curated(key) { Tier::Curated } else { Tier::Manual };
            DepReportLine {
                name: a.dep.name.clone(),
                reason: a.dep.reason.clone(),
                status: detect(&a.dep, mur_home),
                tier,
                hint: a.dep.hint.clone(),
                sources: a.sources.clone(),
            }
        })
        .collect()
}

pub fn missing_count(lines: &[DepReportLine]) -> usize {
    lines.iter().filter(|l| l.status != DepStatus::Present).count()
}

/// Print the human report. `install_cmd` is the exact `... install-deps <name>`
/// the caller should run for curated deps.
pub fn print_report(lines: &[DepReportLine], install_cmd: &str) {
    if lines.is_empty() {
        println!("No external program dependencies declared.");
        return;
    }
    println!("External program dependencies:");
    for l in lines {
        let mark = match l.status {
            DepStatus::Present => "\u{2713}",
            _ => "\u{2717}",
        };
        let tier = match l.tier {
            Tier::Curated => "[curated]",
            Tier::Manual => "[manual]",
        };
        println!("  {mark} {:<16} {}   {tier}", l.name, l.reason);
        if l.status != DepStatus::Present {
            if matches!(l.tier, Tier::Curated) {
                println!("      auto:   {install_cmd}");
            }
            if let Some(h) = &l.hint {
                println!("      manual: {h}");
            }
        }
    }
    let missing = missing_count(lines);
    if missing > 0 {
        println!("{missing} missing — the artifact runs without them (features degrade).");
    }
}
```

Add `pub mod doctor;` is already in Task 6's mod.rs. 

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core deps::doctor`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deps/doctor.rs
git commit -m "feat(deps): doctor report (present/missing + curated/manual tier)"
```

---

### Task 8: `install-deps` command + CLI wiring

**Files:**
- Create: `mur-core/src/cmd/deps/install.rs`
- Modify: `mur-core/src/cli/actions.rs` (add `Doctor`/`InstallDeps` to fleet + agent actions), `mur-core/src/dispatch.rs`

**Interfaces:**
- Consumes: `aggregate_agent`/`aggregate_fleet` (Task 6), `build_report` (Task 7), `installer::install` (Task 5), `registry::recipe`, `current_platform`.
- Produces: `async fn cmd_install_deps(mur_home: &Path, lines: &[DepReportLine], only: Option<&str>, yes: bool) -> Result<()>`; CLI: `mur agent doctor <name>` already exists (extended in Task 9), `mur fleet doctor <name>`, `mur agent install-deps <name> [--program X] [--yes]`, `mur fleet install-deps <name> [--program X] [--yes]`.

- [ ] **Step 1: Write the failing test** (`install.rs` — the selection/skip logic, no network)

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cmd::deps::doctor::{DepReportLine, Tier};
    use mur_common::deps::DepStatus;

    fn line(name: &str, tier: Tier, status: DepStatus) -> DepReportLine {
        DepReportLine { name: name.into(), reason: "r".into(), status, tier,
                        hint: Some("h".into()), sources: vec![] }
    }

    #[test]
    fn selects_only_missing_curated_respecting_program_filter() {
        let lines = vec![
            line("lightpanda", Tier::Curated, DepStatus::Missing),
            line("obscura", Tier::Curated, DepStatus::Present),   // present → skip
            line("weirdtool", Tier::Manual, DepStatus::Missing),  // manual → skip
        ];
        assert_eq!(installable(&lines, None), vec!["lightpanda"]);
        assert_eq!(installable(&lines, Some("obscura")), Vec::<String>::new()); // present
        assert_eq!(installable(&lines, Some("weirdtool")), Vec::<String>::new()); // manual
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p mur-core deps::install`
Expected: FAIL.

- [ ] **Step 3: Implement**

Create `mur-core/src/cmd/deps/install.rs`:

```rust
//! `install-deps`: install missing CURATED deps (consent-gated). Manual-only
//! deps are never installed — their hint is printed.

use crate::cmd::deps::doctor::{DepReportLine, Tier};
use anyhow::Result;
use mur_common::deps::registry::recipe;
use mur_common::deps::{current_platform, DepStatus};
use std::path::Path;

/// Names of deps that are missing, curated, and (if `only` set) match it.
pub fn installable(lines: &[DepReportLine], only: Option<&str>) -> Vec<String> {
    lines
        .iter()
        .filter(|l| l.status != DepStatus::Present && matches!(l.tier, Tier::Curated))
        .filter(|l| only.is_none_or(|o| o == l.name))
        .map(|l| l.name.clone())
        .collect()
}

/// Install each installable dep after per-item consent (unless `yes`).
pub async fn cmd_install_deps(
    mur_home: &Path,
    lines: &[DepReportLine],
    only: Option<&str>,
    yes: bool,
) -> Result<()> {
    let names = installable(lines, only);
    if names.is_empty() {
        println!("Nothing to install (no missing curated deps match).");
        for l in lines.iter().filter(|l| l.status != DepStatus::Present && matches!(l.tier, Tier::Manual)) {
            if let Some(h) = &l.hint {
                println!("  {} — install manually: {h}", l.name);
            }
        }
        return Ok(());
    }
    let platform = current_platform();
    for name in names {
        let key = lines.iter().find(|l| l.name == name).unwrap();
        let reg_key = key.name.as_str(); // registry key == name for curated seed
        let Some(rec) = recipe(reg_key, &platform) else {
            println!("  {name}: no recipe for platform {platform} — skipping (install manually).");
            continue;
        };
        if !yes {
            println!("Install {name} from {} ?", rec.url);
            if !crate::util::confirm::prompt_yes_no("  proceed? [y/N] ")? {
                println!("  skipped {name}.");
                continue;
            }
        }
        match crate::cmd::deps::installer::install(&rec, mur_home).await {
            Ok(paths) => println!("  installed {name} -> {:?}", paths),
            Err(e) => println!("  FAILED {name}: {e}"),
        }
    }
    Ok(())
}
```

**Interface note:** `crate::util::confirm::prompt_yes_no` — use the codebase's
existing y/N prompt helper (grep `prompt_yes_no`/`confirm`/`[y/N]`); if none
exists, read a line from stdin and match `y`/`yes`. `is_none_or` is stable in
edition 2024.

Wire CLI in `actions.rs`: add to the fleet action enum a `Doctor { name }` and
`InstallDeps { name, program: Option<String>, yes: bool }`; add `InstallDeps` to
the agent action enum (agent already has `Doctor`). In `dispatch.rs`, route them:
`doctor` → aggregate + build_report + print_report; `install-deps` → aggregate +
build_report + `cmd_install_deps`.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core deps::install` then `cargo build -p mur-core` (full, to catch CLI wiring).
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/deps/install.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(deps): install-deps command + agent/fleet doctor+install-deps CLI"
```

---

### Task 9: doctor integration + non-blocking import/run preflight

**Files:**
- Modify: `mur-core/src/cmd/agent/doctor.rs` (append the deps section)
- Modify: fleet import entrypoint (`mur-core/src/cmd/fleet/import.rs`) and run/start (`mur-core/src/cmd/fleet/loop_run.rs` or `deep_research/run.rs`, and agent start)

**Interfaces:**
- Consumes: `aggregate_agent`/`aggregate_fleet`, `build_report`, `print_report`, `missing_count`.

- [ ] **Step 1: Write the failing test** (a preflight helper that returns the report and NEVER errors on missing)

Add to `mur-core/src/cmd/deps/mod.rs`:

```rust
#[cfg(test)]
mod preflight_tests {
    use super::*;
    #[test]
    fn preflight_never_errors_on_missing() {
        // A report with missing deps must be Ok (non-blocking).
        let tmp = std::env::temp_dir().join(format!("murpf_{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();
        let deps = vec![]; // empty is trivially fine
        let r = crate::cmd::deps::doctor::build_report(&deps, &tmp);
        assert_eq!(crate::cmd::deps::doctor::missing_count(&r), 0);
        std::fs::remove_dir_all(&tmp).ok();
    }
}
```

- [ ] **Step 2: Run to verify it fails/passes**

Run: `cargo test -p mur-core deps::preflight_tests`
Expected: PASS (this codifies the non-blocking contract; it's a guard test).

- [ ] **Step 3: Wire the integrations**

- In `cmd/agent/doctor.rs::cmd_doctor`, after the existing checks, call
  `aggregate_agent` + `build_report` + `print_report` with
  `install_cmd = "mur agent install-deps <name>"`. Never return Err for missing deps.
- In fleet **import** (after a successful import), call `aggregate_fleet` +
  `build_report` + `print_report`; if `missing_count > 0` print a one-line
  "run `mur fleet install-deps <name>`" nudge. Do NOT fail the import.
- In fleet **run** / **deep-research run** startup and **agent start**, call the
  same and print a non-fatal warning line when `missing_count > 0`. Guard behind
  a best-effort `if let Ok(deps) = aggregate_*` so a load error never blocks the run.

Each of these is a few lines; wrap in `let _ = (|| -> anyhow::Result<()> { … })();`
style best-effort so a failure in the preflight never aborts the primary action.

- [ ] **Step 4: Run to verify it passes**

Run: `cargo test -p mur-core deps:: 2>&1 | tail -5` then `cargo build -p mur-core`
Expected: PASS + builds.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/doctor.rs mur-core/src/cmd/fleet/import.rs mur-core/src/cmd/fleet/loop_run.rs
git commit -m "feat(deps): non-blocking doctor preflight on import/run + agent doctor section"
```

---

### Task 10: Docs

**Files:**
- Modify: `docs/architecture/runtime-overview.md` (add a "Portable program dependencies" subsection)
- Modify: `docs/design/deep-research/README.md` (§6 install note → point at `mur fleet install-deps deep-research`)

- [ ] **Step 1:** Document the declaration schema (`requires_programs`), the four sites, `doctor`/`install-deps`, the curated-vs-manual tier, and that missing deps degrade rather than block. Update the deep-research §6 render-engine install step to mention `mur fleet install-deps deep-research` as the curated one-command path (alongside the manual download).

- [ ] **Step 2: Commit**

```bash
git add docs/architecture/runtime-overview.md docs/design/deep-research/README.md
git commit -m "docs(deps): portable program dependencies + deep-research install-deps note"
```

---

## Self-Review

**Spec coverage:** §5 schema → T1+T2; §6 detect → T3; §7.1 curated registry → T4; installer/§11 integrity → T5; §10 aggregate (+MCP-command synthesis) → T6; §8 doctor → T7; §8 install-deps → T8; §9 import/run preflight → T9; docs → T10. §7.2 (trusted-publisher) + §7.3 nuance beyond "manual" are explicitly Phase 2 / covered as the `Manual` tier. ✓ All Phase 1 spec sections have a task.

**Placeholder scan:** The only intentional `<FETCH>`/`<TAG>` placeholders are in the **registry manifest data** (T4 Step 3), where the plan explicitly instructs fetching real per-platform SHA-256 via `curl … | shasum -a 256` — a data-gathering action, not a code placeholder, and the T4 test (`sha256.len()==64`) fails until they're real. Loader fn names in T6 carry an explicit "confirm/grep the real name" instruction. No code-logic placeholders.

**Type consistency:** `ProgramDep`/`DetectMethod`/`DepStatus`/`VersionCheck` (T1) used identically in T3/T6/T7; `CuratedRecipe`/`ArchiveSpec`/`RecipeMember` (T4) match `verify_and_place` (T5); `AggregatedDep` (T6) consumed by `build_report` (T7); `DepReportLine`/`Tier` (T7) consumed by `install.rs` (T8) and preflight (T9). `install_to` is `Option<String>` on the recipe (bare) and `String` per-member (archive) — consistent between T4 and T5.

**Open interface confirmations for the implementer (flagged inline, not placeholders):** exact loader fn names (`load_profile`, `load_fleet`) in T6; the y/N prompt helper in T8. Both instruct grepping the real symbol.
