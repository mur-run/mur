# Skill Origin Stamps + Upgrade Pipeline Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Registry-installed and built-in skills carry an origin stamp and auto-upgrade through one pipeline; user-modified skills are never overwritten.

**Architecture:** Three origin fields on `SkillManifest` + an origin-stable content hash in `mur-common`; an upgrade engine in `mur-core` that compares installed origin-stamped skills against the git registry cache and applies upgrades only when the local content is unmodified; a `mur skill upgrade` CLI; a daily daemon tick. Spec: `docs/superpowers/specs/2026-07-04-builtin-skills-registry-install-design.md` §C.

**Tech Stack:** Rust (edition 2024), serde/serde_yaml_ng, existing `mur_common::skill` module, existing git-based registry client (`mur-core/src/cmd/skill_registry.rs`).

## Global Constraints

- No hardcoded values — registry URL comes from `skill_registry::DEFAULT_REGISTRY` / config.
- Single source file ≤ 800 lines.
- Fail-closed: any error during upgrade of one skill skips that skill and continues; never write a partially-upgraded manifest (use existing atomic `write_to_dir`).
- Never overwrite a user-modified skill (drift hash mismatch ⇒ skip + report).
- Test with `cargo nextest run -p <crate>` (plain `cargo test --workspace` is flaky); build env needs `ORT_STRATEGY=download` and, for `mur-core` lib, `MUR_WEB_DIST=$HOME/Projects/mur-web/dist`.
- Run `cargo fmt --all` and `cargo clippy --workspace -- -D warnings` before each commit.

---

### Task 1: Origin fields on `SkillManifest` + origin-stable hash

**Files:**
- Modify: `mur-common/src/skill/manifest.rs` (struct `SkillManifest`, ~line 124)
- Modify: `mur-common/src/skill/hash.rs`
- Test: inline `#[cfg(test)]` in `mur-common/src/skill/hash.rs`

**Interfaces:**
- Produces: `SkillManifest.origin: Option<String>`, `origin_version: Option<String>`, `origin_hash: Option<String>` (all serde-default, skipped when `None`), and `pub fn content_hash_for_origin(m: &SkillManifest) -> Result<String, ParseError>` in `hash.rs`.
- Key invariant: `content_hash_for_origin` excludes `origin`/`origin_version`/`origin_hash` (plus `transfer_chain`/`evolution_log`, like `content_hash_for_trust`) so restamping after an upgrade does not change the hash it is compared against.

- [ ] **Step 1: Write the failing test** (append to `hash.rs` tests)

```rust
#[test]
fn origin_hash_stable_across_restamp() {
    let yaml = "name: t\nversion: 1.0.0\npublisher: mur-official\ndescription: d\ncategory: workflow\n";
    let mut m: SkillManifest = crate::skill::parse_str(yaml).unwrap();
    let before = content_hash_for_origin(&m).unwrap();
    m.origin = Some("registry:mur-official/t".into());
    m.origin_version = Some("1.1.0".into());
    m.origin_hash = Some(before.clone());
    assert_eq!(content_hash_for_origin(&m).unwrap(), before);
    // but real content changes DO change it
    m.description = "changed".into();
    assert_ne!(content_hash_for_origin(&m).unwrap(), before);
}
```

(Use whatever the crate's existing yaml-parse entry point is — the same
one `read_from_dir` uses in `store.rs:48` — if `parse_str` isn't its name.)

- [ ] **Step 2: Run to verify it fails**

Run: `cargo nextest run -p mur-common origin_hash_stable`
Expected: compile error — fields/function don't exist.

- [ ] **Step 3: Implement**

In `manifest.rs`, after the `visibility` field (follow the `scope` field's serde style):

```rust
/// Registry origin stamp: `registry:<publisher>/<name>`. Present on
/// built-in and registry-installed skills; drives the upgrade pipeline.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub origin: Option<String>,
#[serde(default, skip_serializing_if = "Option::is_none")]
pub origin_version: Option<String>,
/// `content_hash_for_origin` of the content as shipped; mismatch with
/// the current content means the user modified the skill locally.
#[serde(default, skip_serializing_if = "Option::is_none")]
pub origin_hash: Option<String>,
```

In `hash.rs`:

```rust
/// Origin-stable content hash: excludes the origin stamp itself (and the
/// trust-excluded fields) so restamping never changes the hash.
pub fn content_hash_for_origin(m: &SkillManifest) -> Result<String, crate::skill::ParseError> {
    let mut clone = m.clone();
    clone.transfer_chain = vec![];
    clone.evolution_log = vec![];
    clone.origin = None;
    clone.origin_version = None;
    clone.origin_hash = None;
    content_sha256(&clone)
}
```

If `SkillManifest` construction sites break (struct literals), add `..Default::default()` or the three `None` fields as needed.

- [ ] **Step 4: Run tests**

Run: `cargo nextest run -p mur-common`
Expected: all PASS (including existing hash/sign tests — origin fields are `None` by default so canonical serialization of old manifests is unchanged; if a sign/trust-hash test fails, that's a regression to fix, not to accept).

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/manifest.rs mur-common/src/skill/hash.rs
git commit -m "feat(skill): origin stamp fields + origin-stable content hash"
```

---

### Task 2: Stamp origin at registry install time

**Files:**
- Modify: `mur-core/src/cmd/agent/skill.rs` (registry-resolved install path that calls `write_to_dir` at ~line 94)
- Test: existing test module in the same file (or sibling test file if none)

**Interfaces:**
- Consumes: Task 1 fields + `content_hash_for_origin`.
- Produces: every skill installed FROM THE REGISTRY lands on disk with `origin: registry:<publisher>/<name>`, `origin_version: <installed version>`, `origin_hash: <hash>`. Non-registry installs (local path, remote URL via quill) are NOT stamped in this task.

- [ ] **Step 1: Write the failing test**

```rust
#[test]
fn registry_install_stamps_origin() {
    // arrange a manifest as the install path has it just before write_to_dir
    let yaml = "name: t\nversion: 1.2.0\npublisher: mur-official\ndescription: d\ncategory: workflow\n";
    let mut m: mur_common::skill::SkillManifest = /* crate's parse entry */;
    stamp_registry_origin(&mut m);
    assert_eq!(m.origin.as_deref(), Some("registry:mur-official/t"));
    assert_eq!(m.origin_version.as_deref(), Some("1.2.0"));
    assert_eq!(
        m.origin_hash.as_deref().unwrap(),
        mur_common::skill::hash::content_hash_for_origin(&m).unwrap()
    );
}
```

- [ ] **Step 2: Run to verify it fails** — `cargo nextest run -p mur-core registry_install_stamps_origin` → compile error.

- [ ] **Step 3: Implement**

```rust
/// Stamp a registry-sourced manifest with its origin identity so the
/// upgrade pipeline can track it. Idempotent.
pub fn stamp_registry_origin(m: &mut SkillManifest) {
    m.origin = Some(format!("registry:{}/{}", m.publisher, m.name));
    m.origin_version = Some(m.version.clone());
    m.origin_hash = None; // exclude stamp fields, then hash
    m.origin_hash = mur_common::skill::hash::content_hash_for_origin(m).ok();
}
```

Call it in the registry-install path in `agent/skill.rs` immediately before `write_to_dir(&dest_dir, &manifest)` — only on the branch where the manifest was resolved via `skill_resolver`/`skill_registry` (trace the caller; do not stamp local-file or quill-URL installs).

- [ ] **Step 4: Run tests** — `cargo nextest run -p mur-core` → PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/agent/skill.rs
git commit -m "feat(skill): stamp registry origin on install"
```

---

### Task 3: Upgrade engine (`skill_upgrade.rs`)

**Files:**
- Create: `mur-core/src/cmd/skill_upgrade.rs`
- Modify: `mur-core/src/cmd/mod.rs` (add `pub mod skill_upgrade;`) — mind the known gotcha: mur-core modules must be declared in BOTH `lib.rs` and `main.rs` module trees if both exist.
- Test: `#[cfg(test)]` in `skill_upgrade.rs` using `tempfile::TempDir`

**Interfaces:**
- Consumes: `skill_registry::{load_index, skill_yaml_path, available_versions}`, `store::{read_from_dir, write_to_dir, global_skill_dir, agent_skill_dir}`, `hash::content_hash_for_origin`, Task 2's stamp.
- Produces:

```rust
pub enum UpgradeStatus { UpToDate, Upgraded { from: String, to: String },
                         BlockedModified { local: String, latest: String },
                         NotInRegistry, Error(String) }
pub struct UpgradeItem { pub name: String, pub dir: PathBuf, pub status: UpgradeStatus }
pub struct UpgradeReport { pub items: Vec<UpgradeItem> }
/// Scan all origin-stamped skills (global `~/.mur/skills/*` and every
/// `~/.mur/agents/*/skills/*`) against the registry cache.
/// `apply=false` = check only.
pub fn upgrade_all(mur_home: &Path, registry_dir: &Path, apply: bool) -> UpgradeReport
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    // helper: write a skill dir + a fake registry dir (index.yaml +
    // skills/<name>/<version>/skill.yaml) in a TempDir, mirroring the
    // layout skill_yaml_path() expects.

    #[test]
    fn unmodified_skill_upgrades_and_restamps() {
        // local: v1.0.0 stamped, content untouched; registry latest: 1.1.0
        // expect: status Upgraded{1.0.0→1.1.0}; on-disk manifest is the
        // registry 1.1.0 content, origin_version=1.1.0,
        // origin_hash == content_hash_for_origin(new)
    }

    #[test]
    fn modified_skill_is_never_overwritten() {
        // local: v1.0.0 stamped, then description edited (hash drifts)
        // registry latest: 1.1.0
        // expect: BlockedModified; on-disk file byte-identical to before
    }

    #[test]
    fn check_mode_writes_nothing() { /* apply=false, upgradable skill, file unchanged */ }

    #[test]
    fn unstamped_and_unknown_skills_are_skipped() {
        // one skill without origin (ignored entirely),
        // one stamped but absent from index (NotInRegistry)
    }
}
```

Write the four tests fully (arrange/act/assert with real yaml strings) before any implementation.

- [ ] **Step 2: Run to verify they fail** — `cargo nextest run -p mur-core skill_upgrade` → compile error.

- [ ] **Step 3: Implement**

Core loop, per skill dir found:

```rust
let Ok(mut local) = read_from_dir(&dir) else { continue };
let Some(origin) = local.origin.clone() else { continue };
let Some(key) = origin.strip_prefix("registry:") else { continue };
let (_, name) = key.rsplit_once('/').unwrap_or(("", key));
let Some(entry) = index.skills.get(name) else { push(NotInRegistry); continue };
let local_ver = local.origin_version.clone().unwrap_or_default();
if entry.latest == local_ver { push(UpToDate); continue }
let modified = local.origin_hash.as_deref()
    != content_hash_for_origin(&local).ok().as_deref();
if modified { push(BlockedModified{..}); continue }
if !apply { push(Upgraded{..}); continue } // check mode reports what WOULD happen
let new_yaml_path = skill_yaml_path(registry_dir, name, &entry.latest);
// parse, validate, stamp_registry_origin, write_to_dir(&dir, &new); fail → Error(..), continue
```

Skill-dir discovery: `global_skill_dir`'s parent (`~/.mur/skills/`) plus each `~/.mur/agents/<a>/skills/` — plain `read_dir`, skip non-dirs and dirs without `skill.yaml`.

- [ ] **Step 4: Run tests** — `cargo nextest run -p mur-core skill_upgrade` → 4 PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/skill_upgrade.rs mur-core/src/cmd/mod.rs mur-core/src/lib.rs mur-core/src/main.rs
git commit -m "feat(skill): origin-aware upgrade engine (drift-safe)"
```

---

### Task 4: `mur skill upgrade` CLI

**Files:**
- Modify: `mur-core/src/cmd/skill_cmd.rs` (add subcommand where the existing `skill` subcommands are declared — follow the pattern of `skill scope`)
- Test: `#[cfg(test)]` covering the report formatter

**Interfaces:**
- Consumes: `skill_upgrade::upgrade_all`, `skill_registry::{fetch_registry, DEFAULT_REGISTRY, registry_cache_dir}`.
- Produces: `mur skill upgrade [--check] [--json]`. Default applies; `--check` reports only; `--json` emits the report as JSON (derive `Serialize` on the report types) for the Hub to consume later.

- [ ] **Step 1: Write the failing test** — formatter test: an `UpgradeReport` with one item of each status renders one line per item, e.g. `upgraded brainstorming 1.0.0 → 1.1.0`, `blocked (locally modified) foo: local 1.0.0, latest 1.2.0`, and a summary line `2 upgraded, 1 blocked, 3 up-to-date`.

- [ ] **Step 2: Run to verify it fails** — `cargo nextest run -p mur-core upgrade_report_format` → compile error.

- [ ] **Step 3: Implement** — subcommand handler: `fetch_registry(mur_home, DEFAULT_REGISTRY)` (respect the existing registry-URL config override if `skill_cmd.rs` has one — grep before hardcoding), then `upgrade_all(...)`, then print via the formatter or `serde_json::to_string_pretty`. Registry fetch failure = clean error exit, not a panic.

- [ ] **Step 4: Run** — `cargo nextest run -p mur-core` PASS, then a manual smoke: `cargo run -- skill upgrade --check` on the dev machine prints a report (likely all skills unstamped → empty report; that's correct).

- [ ] **Step 5: Commit**

```bash
git add mur-core/src/cmd/skill_cmd.rs mur-core/src/cmd/skill_upgrade.rs
git commit -m "feat(cli): mur skill upgrade (--check/--json)"
```

---

### Task 5: Daily auto-upgrade tick in the daemon

**Files:**
- Create: `mur-daemon/src/skill_upgrade_tick.rs` (mirror the structure of `mur-daemon/src/fleet_tick.rs`)
- Modify: `mur-daemon/src/main.rs` (spawn alongside `fleet_tick`)
- Test: `#[cfg(test)]` in `skill_upgrade_tick.rs` for the due-check

**Interfaces:**
- Consumes: `mur_core::cmd::skill_upgrade::upgrade_all`, `skill_registry::fetch_registry`.
- Produces: once per 24h (stamp file `~/.mur/cache/.skill-upgrade-last-run`, same pattern as fleets' `.last_run`), the daemon fetches the registry and runs `upgrade_all(apply=true)`. Config gate `skills.auto_upgrade: bool` in `config.yaml`, **default true** (matches plugin auto-update expectations; upgrades touch only unmodified official-origin skills, and drift-blocking makes it non-destructive). Log one `tracing::info!` summary; blocked items logged at `warn` so the Hub/user can see them.

- [ ] **Step 1: Write the failing test** — due-check: stamp file absent → due; stamped now → not due; stamped 25h ago (write a fake old timestamp) → due.

- [ ] **Step 2: Run to verify it fails** — `cargo nextest run -p mur-daemon skill_upgrade` → compile error.

- [ ] **Step 3: Implement** — `is_due(mur_home) -> bool` reading the stamp file (unix-seconds text, like fleet `.last_run`); tick task on the existing 30s cycle: if config flag on and due → `std::thread::spawn` the fetch+upgrade (registry fetch shells out to git — keep it off the async runtime, same reasoning as fleet loops), write stamp after completion.

- [ ] **Step 4: Run tests** — `cargo nextest run -p mur-daemon` → PASS; `cargo clippy --workspace -- -D warnings`.

- [ ] **Step 5: Commit**

```bash
git add mur-daemon/src/skill_upgrade_tick.rs mur-daemon/src/main.rs
git commit -m "feat(daemon): daily auto-upgrade for origin-stamped skills"
```

---

## Deferred to later plans (per spec order)

- Plan 2 (A): port brainstorming skill, publish to registry, seed in Hub template with origin stamp, seed-missing-only on Hub upgrade.
- Plan 3 (D): role skill packs + `recommended_roles` + wizard integration.
- Plan 4 (B): relay install_request frame, daemon handler, Hub consent UI (which will also surface Task 5's blocked-modified items), Dashboard buttons.
