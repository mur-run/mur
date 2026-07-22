# MUR Pack S4 — Import Governance Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Bring the Claude-plugin importer under Pack governance — never-shadow at import, a content-hash pin, and an explicit re-import refresh.

**Architecture:** Three tasks. (1) `mur-common`: two back-compat `Option<String>` fields on `AddonRef` — `content_hash` (the pin) and `fetch_ref` (the re-fetchable source, distinct from the free-text provenance `source`). (2) `addon/import.rs`: a never-shadow filter (skip skills colliding with the global store + warn), plus recording `content_hash` and `fetch_ref` on the `AddonRef`. (3) `mur agent addon reimport` — re-fetch from `fetch_ref`, re-apply (re-running the never-shadow gate), preserve `enabled`. Reuses `list_installed`, `content_hash_for_origin`, `sha256_hex`, and the existing import/remove/enable commands.

**Tech Stack:** Rust (edition 2024), `mur-common` (`agent`, `skill::hash`, `skill::local`), `mur-core` (`cmd/agent/addon`, `cli`, `dispatch`), `serde`, `tempfile`.

## Global Constraints

- Reuse existing helpers — no new copies:
  - `mur_common::skill::local::list_installed(mur_home: &Path) -> Result<Vec<String>, StoreError>` — the global store (builtins after sync + registry). The never-shadow collision set.
  - `mur_common::skill::content_hash_for_origin(m: &SkillManifest) -> Result<String, ParseError>` and `mur_common::skill::sha256_hex(bytes: &[u8]) -> String` — for the pin.
  - `AddonRef` (`mur-common/src/agent.rs:405`): `{ id, source, enabled, skills, mcp, commands }`. `source` is free-text provenance (`"claude-local:<name>@<ver>"`), NOT a fetch target.
  - Import structure (`mur-core/src/cmd/agent/addon/import.rs`): Phase 1 collects `pending_skills: Vec<(PathBuf, SkillManifest, PathBuf)>` and `pending_cmds: Vec<(PathBuf, SkillManifest)>` with NO writes; Phase 2 writes them and pushes the `AddonRef` (~line 322). `cmd_addon_import(name, plugin_dir, plugin: Option<&str>, force: bool)`.
  - `cmd_addon_remove(name, addon_id)`, `cmd_addon_set_enabled(name, addon_id, enabled)` (`addon/mod.rs`).
  - Addon dispatch: `AgentAddonAction` enum (`cli/actions.rs`); the `AgentAction::Addon { action }` match arm (`dispatch.rs:1709`).
- Collision behavior: **skip the colliding skill + warn** (import the rest); the skill is excluded from `AddonRef.skills`. Never refuse the whole import or rename.
- Brand "MUR" uppercase in user-facing strings.
- Test: `ORT_STRATEGY=download MUR_WEB_DIST=$HOME/Projects/mur-web/dist RUSTFLAGS=-Cdebuginfo=0 CARGO_TARGET_DIR=/Volumes/Firecuda4tb/Projects/mur/target cargo test -p <crate> <name>` (`-p mur-common` Task 1, `-p mur-core` Tasks 2-3; add `~/.rustup/toolchains/stable-aarch64-apple-darwin/bin` to PATH if needed). Build ONLY the crate under test (disk is tight). If a debug bin CLI-parse test stack-overflows, set `RUST_MIN_STACK=33554432`.

---

### Task 1: `AddonRef` gains `content_hash` + `fetch_ref` (`mur-common`)

**Files:**
- Modify: `mur-common/src/agent.rs` (`AddonRef` struct + a test)
- Test: `mur-common/src/agent.rs` tests

**Interfaces:**
- Produces: `AddonRef.content_hash: Option<String>`, `AddonRef.fetch_ref: Option<String>` (both `#[serde(default, skip_serializing_if = "Option::is_none")]`). Consumed by Tasks 2-3.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod` in `mur-common/src/agent.rs`:

```rust
#[test]
fn addon_ref_content_hash_and_fetch_ref_default_none_and_round_trip() {
    // legacy AddonRef (no new fields) → None
    let legacy = "id: a\nsource: claude-local:a@1\nenabled: false\n";
    let r: AddonRef = serde_yaml_ng::from_str(legacy).unwrap();
    assert_eq!(r.content_hash, None);
    assert_eq!(r.fetch_ref, None);

    // with the new fields → round-trips
    let full = "id: a\nsource: claude-local:a@1\nenabled: true\ncontent_hash: abc123\nfetch_ref: owner/repo\n";
    let r2: AddonRef = serde_yaml_ng::from_str(full).unwrap();
    assert_eq!(r2.content_hash.as_deref(), Some("abc123"));
    assert_eq!(r2.fetch_ref.as_deref(), Some("owner/repo"));
    let back = serde_yaml_ng::to_string(&r2).unwrap();
    let r3: AddonRef = serde_yaml_ng::from_str(&back).unwrap();
    assert_eq!(r2, r3);
}
```

- [ ] **Step 2: Run to verify failure**

Run: `… cargo test -p mur-common addon_ref_content_hash_and_fetch_ref`
Expected: FAIL — no `content_hash`/`fetch_ref` fields (compile error).

- [ ] **Step 3: Add the fields**

In `mur-common/src/agent.rs`, add to `AddonRef` (after `commands`):

```rust
    /// Content-hash pin over the imported skill/command manifests, recorded
    /// at import. `None` on legacy refs. Enables drift detection + refresh.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// The re-fetchable source (the original `import` argument: a local path
    /// or `owner/repo`), distinct from the free-text provenance `source`.
    /// `None` on legacy refs. Used by `reimport`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fetch_ref: Option<String>,
```

If any `AddonRef { .. }` struct literal without `..Default::default()` breaks compilation, add `content_hash: None, fetch_ref: None,` there.

- [ ] **Step 4: Run to verify pass**

Run: `… cargo test -p mur-common addon_ref_content_hash_and_fetch_ref`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(common): AddonRef gains content_hash + fetch_ref (back-compat)"
```

---

### Task 2: Never-shadow filter + pin recording at import (`mur-core`)

**Files:**
- Modify: `mur-core/src/cmd/agent/addon/import.rs`
- Test: `mur-core/src/cmd/agent/addon/import.rs` tests

**Interfaces:**
- Consumes: Task 1's `AddonRef` fields; `list_installed`, `content_hash_for_origin`, `sha256_hex`.
- Produces: the importer now skips global-store-colliding skills, and records `content_hash` + `fetch_ref` on the `AddonRef`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod` in `import.rs` (mirror the existing import tests' setup — a plugin dir with `skills/<name>/SKILL.md`, an agent under a temp `MUR_HOME`):

```rust
#[test]
fn import_skips_skill_that_shadows_the_global_store() {
    // See sibling import tests for the exact temp-home + plugin scaffolding;
    // reuse that harness. Seed a global-store skill named "brainstorm"
    // (write ~/.mur/skills/brainstorm/skill.yaml), and a plugin bundling
    // skills/brainstorm + skills/unique.
    // After cmd_addon_import(agent, plugin_dir, None, true):
    //   - the agent has skills/unique but NOT skills/brainstorm
    //   - the AddonRef.skills contains "unique" but not "brainstorm"
    //   - AddonRef.content_hash is Some(non-empty)
    //   - AddonRef.fetch_ref == Some(<the plugin_dir arg>)
    // (Write the concrete assertions using the sibling tests' helper names.)
}
```
NOTE to implementer: build this test by copying the closest existing import test's scaffolding (temp `MUR_HOME`, agent seed, plugin dir with `skills/<n>/SKILL.md`), then add a global-store skill dir (`mur_home.join("skills/brainstorm/skill.yaml")` with a minimal valid manifest) and a second non-colliding plugin skill. Assert the four bullet points above.

- [ ] **Step 2: Run to verify failure**

Run: `… cargo test -p mur-core import_skips_skill_that_shadows`
Expected: FAIL — the colliding skill is currently imported (no filter), `content_hash`/`fetch_ref` are `None`.

- [ ] **Step 3: Add the never-shadow filter**

In `import.rs`, between Phase 1 (collect) and Phase 2 (commit), after `pending_skills` and `pending_cmds` are built, filter out global-store collisions:

```rust
    // ── Never-shadow gate: drop skills/commands whose name collides with a
    // global-store (builtin/registry) skill, so an import can never shadow a
    // MUR-shipped skill. The agent uses MUR's copy via store-wide injection.
    let global: std::collections::HashSet<String> =
        mur_common::skill::local::list_installed(&mur_home)
            .unwrap_or_default()
            .into_iter()
            .collect();
    let mut retain_non_shadow = |name: &str| -> bool {
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
```
(`mur_home` is the resolved MUR home already available in this function — reuse the existing binding; if it isn't in scope, resolve it via the same call the function already uses to locate the agent dir.)

- [ ] **Step 4: Record `content_hash` + `fetch_ref` on the `AddonRef`**

Before the `profile.addons.push(AddonRef { … })` (~line 322), compute the pin over the skills+commands actually being installed:

```rust
    // Content-hash pin over the installed skill + command manifests.
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
        Some(mur_common::skill::sha256_hex(member_hashes.join(",").as_bytes()))
    };
```
Then set the two new fields in the `AddonRef` literal:
```rust
        content_hash,
        fetch_ref: Some(requested.to_string()),
```
(`requested` is the original user input already captured at the top of the function: `let requested = plugin_dir;`.)

- [ ] **Step 5: Run to verify pass**

Run: `… cargo test -p mur-core import_skips_skill_that_shadows` then `… cargo test -p mur-core --lib cmd::agent::addon` (no regression in existing import tests).
Expected: PASS. Existing import tests still green (non-colliding bundles unchanged; note existing tests may need the two new `AddonRef` fields if they construct the literal directly — they don't, they read it).

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/addon/import.rs
git commit -m "feat(addon): never-shadow filter + content_hash/fetch_ref pin at import"
```

---

### Task 3: `mur agent addon reimport` (`mur-core`)

**Files:**
- Modify: `mur-core/src/cmd/agent/addon/mod.rs` (add `cmd_addon_reimport`)
- Modify: `mur-core/src/cli/actions.rs` (add `AgentAddonAction::Reimport`)
- Modify: `mur-core/src/dispatch.rs` (add the dispatch arm)
- Test: `mur-core/src/cmd/agent/addon/mod.rs` tests

**Interfaces:**
- Consumes: `cmd_addon_import`, `cmd_addon_remove`, `cmd_addon_set_enabled`, `AddonRef.fetch_ref`/`enabled` (Tasks 1-2).
- Produces: `pub fn cmd_addon_reimport(name: &str, addon_id: &str, source_override: Option<&str>) -> Result<()>`.

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod` in `addon/mod.rs`:

```rust
#[test]
fn reimport_replaces_and_preserves_enabled() {
    // Reuse the import-test scaffolding: temp MUR_HOME, an agent, and a
    // plugin dir. Import it, then enable it, then reimport.
    // Assert after reimport:
    //   - the AddonRef still exists (same id), enabled == true (preserved)
    //   - content_hash is Some (refreshed)
    //   - reimport with an unknown id returns Err
    // (Concrete assertions using the sibling helpers.)
}
```
NOTE to implementer: copy the closest existing addon test's scaffolding; after the first `cmd_addon_import`, call `cmd_addon_set_enabled(agent, id, true)`, then `cmd_addon_reimport(agent, id, None)`, then load the profile and assert the AddonRef's `enabled` and `content_hash`. Add a case asserting `cmd_addon_reimport(agent, "nope", None)` is `Err`.

- [ ] **Step 2: Run to verify failure**

Run: `… cargo test -p mur-core reimport_replaces_and_preserves_enabled`
Expected: FAIL — `cmd_addon_reimport` not defined.

- [ ] **Step 3: Implement `cmd_addon_reimport`**

Add to `mur-core/src/cmd/agent/addon/mod.rs`:

```rust
/// Re-fetch an add-on from its recorded `fetch_ref` (or `source_override`),
/// re-apply it (re-running the never-shadow gate + security scan), and
/// preserve the prior `enabled` state.
pub fn cmd_addon_reimport(name: &str, addon_id: &str, source_override: Option<&str>) -> Result<()> {
    let (_, profile) = crate::cmd::agent::load_profile_for_edit(name)?;
    let existing = profile
        .addons
        .iter()
        .find(|a| a.id == addon_id)
        .ok_or_else(|| anyhow::anyhow!("add-on '{addon_id}' not found on '{name}'"))?;
    let was_enabled = existing.enabled;
    let fetch = source_override
        .map(str::to_string)
        .or_else(|| existing.fetch_ref.clone())
        .ok_or_else(|| {
            anyhow::anyhow!(
                "add-on '{addon_id}' has no recorded fetch source — pass one explicitly"
            )
        })?;

    // Remove the old copy, then re-import fresh (records a new fail-closed ref
    // + content_hash), then restore the prior enabled state.
    cmd_addon_remove(name, addon_id)?;
    import::cmd_addon_import(name, &fetch, None, false)?;
    if was_enabled {
        cmd_addon_set_enabled(name, addon_id, true)?;
    }
    Ok(())
}
```

- [ ] **Step 4: Wire the CLI + dispatch**

In `mur-core/src/cli/actions.rs`, add to `AgentAddonAction` (mirror `Remove`):

```rust
    /// Re-fetch and re-apply an add-on from its recorded source
    Reimport {
        name: String,
        addon_id: String,
        /// Override the fetch source (a path or owner/repo)
        #[arg(long)]
        from: Option<String>,
    },
```

In `mur-core/src/dispatch.rs`, add to the `AgentAction::Addon { action }` match (after the `Remove` arm ~line 1723):

```rust
            AgentAddonAction::Reimport { name, addon_id, from } => {
                cmd::agent::addon::cmd_addon_reimport(&name, &addon_id, from.as_deref())?
            }
```

- [ ] **Step 5: Run tests + clippy**

Run:
```
… cargo test -p mur-core reimport_replaces_and_preserves_enabled
… cargo test -p mur-core --lib cmd::agent::addon
… cargo clippy -p mur-core -- -D warnings
```
Expected: tests PASS; clippy clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/addon/mod.rs mur-core/src/cli/actions.rs mur-core/src/dispatch.rs
git commit -m "feat(addon): mur agent addon reimport (refresh from source, preserve enabled)"
```

---

## Rollout / usage (post-merge)

`mur agent addon import <agent> owner/repo` now skips any bundled skill whose name collides with a MUR builtin/registry skill (warned), and records a `content_hash` pin + the `fetch_ref`. `mur agent addon list` shows the pin. `mur agent addon reimport <agent> <id>` refreshes from the recorded source (re-running the never-shadow gate), preserving the enable state; pass `--from <source>` for a legacy add-on with no recorded `fetch_ref`.

## Self-Review

**Spec coverage:** §4.1 never-shadow → Task 2 (filter over pending_skills/pending_cmds); §4.2 content_hash pin → Task 1 (field) + Task 2 (compute/record); §4.3 reimport → Task 3; §4.4 origin/TOFU → unchanged (`source` kept; `fetch_ref` added for re-fetch). ✅

**Placeholder scan:** the two test bodies (Task 2 Step 1, Task 3 Step 1) are described-not-coded because they must be built on the sibling import tests' existing scaffolding (temp `MUR_HOME` + plugin dir + agent seed), which differs across the file; the assertions to make are enumerated concretely. This is a deliberate instruction to reuse existing harness, not an unfilled placeholder — every assertion is specified. All non-test code is complete.

**Type consistency:** `AddonRef.content_hash`/`fetch_ref: Option<String>` (Task 1) consumed in Tasks 2-3. `cmd_addon_reimport(name, addon_id, source_override)` defined in Task 3, wired in dispatch with `from.as_deref()`. `content_hash_for_origin`/`sha256_hex`/`list_installed` signatures match the Global Constraints. `AgentAddonAction::Reimport` variant matches the dispatch arm.

**Scope:** three tasks, importer-only. The `ImportAdapter` trait, additional adapters, and store-level enforcement remain out of scope per the spec. `ponytail:` reimport composes existing remove+import+enable rather than a bespoke refresh path; `fetch_ref` is one small field rather than a general source-resolver abstraction.
