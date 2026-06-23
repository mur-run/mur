# Add-on Enable/Disable (Phase 1) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a non-destructive, per-agent enable/disable toggle for skills and MCP servers, enforced in the runtime and surfaced in the MUR Hub.

**Architecture:** Per-agent **denylists** (`disabled_skills`, `disabled_mcp`) on `AgentProfile`. Skills load globally and inject by scope with no per-agent gate, so enforcement happens at the single startup site where the profile and the loaded skills are both in scope (`supervisor_runner.rs::prepare_runtime`): the loaded-skill list is filtered before `RuntimeSkills::build` (which covers Layer-2 injection, Layer-3 trigger injection, and command-trigger firing in one shot), and `mcp_servers` is filtered before `McpPool::new` + `build_tools`. CLI + Hub edit the denylists; the change applies on next agent restart. Plugin-group imports are out of scope (Phase 2).

**Tech Stack:** Rust (edition 2024), `serde_yaml_ng`, Tauri 2, React/TypeScript, `cargo nextest`.

**Spec:** `docs/superpowers/specs/2026-06-23-mur-addon-system-and-claude-plugin-integration-design.md` (this plan implements **Phase 1 / §3.1, §3.3, §4, §9, §8-P1 only**; §3.2/§6 importer = Phase 2; hooks = Phase 3).

## Global Constraints

- **Branch:** all work on `feat/agent-addons` (already checked out).
- **No hardcoded values.** No magic strings/numbers; reuse existing constants.
- **Brand:** user-facing text uses uppercase **MUR**; internal `name`/dir slugs stay lowercase.
- **File size:** keep any source file ≤ 800 lines; if a file you touch is near the limit, note it (don't restructure in this plan).
- **Tests:** use `cargo nextest run` (CI uses nextest; plain `cargo test --workspace` fails ~7 mur-core tests spuriously). Prefix runtime/core build+test with `ORT_STRATEGY=download` (onnxruntime link otherwise fails).
- **Excluded Tauri crate:** `mur-hub-gui` is workspace-excluded — build/fmt it via its own manifest (`--manifest-path mur-hub-gui/src-tauri/Cargo.toml`); root `cargo fmt` does NOT cover it.
- **Non-destructive:** disable must never delete files, stats, or pins. Empty denylist = everything enabled (back-compat).
- **Commit footer:** end every commit message with
  `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

## File Structure

- `mur-common/src/agent.rs` — two `Vec<String>` fields on `AgentProfile`; free helpers `name_enabled` / `set_denylist`; methods `skill_enabled` / `mcp_enabled` / `set_skill_enabled` / `set_mcp_enabled` / `enabled_mcp_servers`. (Logic + unit tests live here.)
- `mur-common/src/skill/loader.rs` — `filter_enabled(loaded, &disabled_skills)` + test.
- `mur-agent-runtime/src/supervisor_runner.rs` — wire both filters at the two known sites.
- `mur-core/src/cli/agent.rs` — `Enable`/`Disable` variants on `AgentSkillAction` + `AgentMcpAction`.
- `mur-core/src/cmd/agent/skill.rs` — `cmd_skill_set_enabled`.
- `mur-core/src/cmd/agent/mcp.rs` — `cmd_mcp_set_enabled`.
- `mur-core/src/cmd/agent/mod.rs` — re-export the two new handlers.
- dispatch site (located via `rg`) — two new match arms each for skill + mcp.
- `mur-hub-gui/src-tauri/src/mcp_skills.rs` — `agent_skill_toggle` / `agent_mcp_toggle`.
- `mur-hub-gui/src-tauri/src/detail.rs` — `enabled` on `InstalledSkillView` + `McpServerView`.
- `mur-hub-gui/src-tauri/src/lib.rs` — register the two commands.
- `mur-hub-gui/ui/src/types.ts` — `enabled` on the two interfaces.
- `mur-hub-gui/ui/src/components/DetailPanel.tsx` — toggle control + handlers on the Skills + MCP rows.

---

## Task 1: Denylist data model + logic (mur-common)

**Files:**
- Modify: `mur-common/src/agent.rs` (add fields ~after line 74; helpers in `impl AgentProfile`; free fns + tests)

**Interfaces:**
- Produces:
  - `pub fn name_enabled(denylist: &[String], name: &str) -> bool`
  - `pub fn set_denylist(list: &mut Vec<String>, name: &str, enabled: bool)`
  - `AgentProfile.disabled_skills: Vec<String>`, `AgentProfile.disabled_mcp: Vec<String>`
  - `AgentProfile::skill_enabled(&self, &str) -> bool`, `mcp_enabled(&self, &str) -> bool`
  - `AgentProfile::set_skill_enabled(&mut self, &str, bool)`, `set_mcp_enabled(&mut self, &str, bool)`
  - `AgentProfile::enabled_mcp_servers(&self) -> Vec<McpServerEntry>`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests { use super::*; ... }` block in `mur-common/src/agent.rs` (create the block at end of file if none exists):

```rust
#[test]
fn denylist_membership_and_mutation() {
    let mut list: Vec<String> = vec![];
    assert!(name_enabled(&list, "a"), "empty denylist => enabled");

    set_denylist(&mut list, "a", false); // disable
    assert!(!name_enabled(&list, "a"));
    assert_eq!(list, ["a"]);

    set_denylist(&mut list, "a", false); // idempotent disable
    assert_eq!(list, ["a"], "no duplicate entries");

    set_denylist(&mut list, "a", true); // enable removes
    assert!(name_enabled(&list, "a"));
    assert!(list.is_empty());

    set_denylist(&mut list, "b", true); // enabling an absent name is a no-op
    assert!(list.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common denylist_membership_and_mutation`
Expected: FAIL — `cannot find function name_enabled` / `set_denylist`.

- [ ] **Step 3: Add the fields, free helpers, and methods**

In `mur-common/src/agent.rs`, add the two fields to `AgentProfile` immediately after the `installed_skills` field (~line 74):

```rust
    /// Per-agent skill denylist (add-on Phase 1). Skill names that are
    /// installed/visible to this agent but suppressed from injection.
    /// Non-destructive: the skill's files/stats are untouched. Empty = all
    /// visible skills enabled (back-compat: absent in old profiles).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_skills: Vec<String>,

    /// Per-agent MCP denylist (add-on Phase 1). `McpServerEntry` names not
    /// spawned for this agent. Non-destructive: the entry + its pin stay in
    /// the profile. Empty = all configured servers enabled.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub disabled_mcp: Vec<String>,
```

Add the free helpers near the bottom of the file (next to `default_true`, ~line 726):

```rust
/// True if `name` is not present in a denylist (i.e. enabled).
pub fn name_enabled(denylist: &[String], name: &str) -> bool {
    !denylist.iter().any(|n| n == name)
}

/// Add/remove `name` in a denylist. `enabled=true` removes it (idempotent),
/// `enabled=false` adds it once (idempotent).
pub fn set_denylist(list: &mut Vec<String>, name: &str, enabled: bool) {
    if enabled {
        list.retain(|n| n != name);
    } else if !list.iter().any(|n| n == name) {
        list.push(name.to_string());
    }
}
```

Add the methods in the existing `impl AgentProfile { ... }` block:

```rust
    /// Whether `skill_name` is enabled for this agent (Phase 1 denylist).
    pub fn skill_enabled(&self, skill_name: &str) -> bool {
        name_enabled(&self.disabled_skills, skill_name)
    }

    /// Whether MCP server `server_id` is enabled for this agent.
    pub fn mcp_enabled(&self, server_id: &str) -> bool {
        name_enabled(&self.disabled_mcp, server_id)
    }

    /// Toggle a skill for this agent without uninstalling it.
    pub fn set_skill_enabled(&mut self, skill_name: &str, enabled: bool) {
        set_denylist(&mut self.disabled_skills, skill_name, enabled);
    }

    /// Toggle an MCP server for this agent without removing it.
    pub fn set_mcp_enabled(&mut self, server_id: &str, enabled: bool) {
        set_denylist(&mut self.disabled_mcp, server_id, enabled);
    }

    /// This agent's MCP servers minus any disabled for it.
    pub fn enabled_mcp_servers(&self) -> Vec<McpServerEntry> {
        self.mcp_servers
            .iter()
            .filter(|m| self.mcp_enabled(&m.name))
            .cloned()
            .collect()
    }
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common denylist_membership_and_mutation`
Expected: PASS.

- [ ] **Step 5: Fix any exhaustive `AgentProfile { ... }` constructors**

Adding fields breaks struct literals that don't use `..`. Find them:

Run: `rg -n "AgentProfile \{" --type rust`
For each literal construction (NOT pattern matches, NOT `..Default::default()`), add:
```rust
            disabled_skills: Vec::new(),
            disabled_mcp: Vec::new(),
```
Then confirm the workspace compiles:
Run: `ORT_STRATEGY=download cargo check -p mur-common -p mur-core`
Expected: builds clean.

- [ ] **Step 6: Commit**

```bash
git add mur-common/src/agent.rs
git commit -m "feat(addons): per-agent skill/mcp denylist on AgentProfile

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Skill-load filter (mur-common loader)

**Files:**
- Modify: `mur-common/src/skill/loader.rs` (add `filter_enabled` + test)

**Interfaces:**
- Consumes: `name_enabled` (Task 1), `LoadedSkill { name, manifest, trust, scope, content_hash }`
- Produces: `pub fn filter_enabled(loaded: Vec<LoadedSkill>, disabled_skills: &[String]) -> Vec<LoadedSkill>`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `mur-common/src/skill/loader.rs` (it already has a `make(name) -> SkillManifest` helper and imports `TrustLevel` / `SkillScope` via `use super::*`):

```rust
#[test]
fn filter_enabled_drops_denied_names() {
    let mk = |n: &str| LoadedSkill {
        name: n.to_string(),
        manifest: make(n),
        trust: TrustLevel::Sandboxed,
        scope: SkillScope::User,
        content_hash: String::new(),
    };
    let loaded = vec![mk("alpha"), mk("beta")];
    let kept = filter_enabled(loaded, &["beta".to_string()]);
    assert_eq!(
        kept.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
        ["alpha"]
    );
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common filter_enabled_drops_denied_names`
Expected: FAIL — `cannot find function filter_enabled`.

- [ ] **Step 3: Implement `filter_enabled`**

Add to `mur-common/src/skill/loader.rs` (top-level, near `load_all`):

```rust
/// Drop skills disabled for an agent (Phase 1 denylist). Applied once at
/// agent startup, after `load_all`, so the filtered list feeds both injection
/// and the trigger registry.
pub fn filter_enabled(loaded: Vec<LoadedSkill>, disabled_skills: &[String]) -> Vec<LoadedSkill> {
    loaded
        .into_iter()
        .filter(|s| crate::agent::name_enabled(disabled_skills, &s.name))
        .collect()
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-common filter_enabled_drops_denied_names`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-common/src/skill/loader.rs
git commit -m "feat(addons): filter_enabled() to drop denylisted skills at load

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Runtime enforcement (supervisor_runner)

**Files:**
- Modify: `mur-agent-runtime/src/supervisor_runner.rs` (~line 233 MCP site; ~line 494-496 skills site)

**Interfaces:**
- Consumes: `loader::filter_enabled` (Task 2), `AgentProfile::enabled_mcp_servers` (Task 1)

This task is wiring; its correctness is covered by the Task 1/2 unit tests. The deliverable check is a clean build of the runtime.

- [ ] **Step 1: Filter skills before building RuntimeSkills**

In `mur-agent-runtime/src/supervisor_runner.rs`, find (~line 494-496):

```rust
    let loaded = mur_common::skill::loader::load_all(&mur_home, &profile.inner.name);
    let runtime_skills = Arc::new(RuntimeSkills::build(loaded));
```

Replace with:

```rust
    let loaded = mur_common::skill::loader::load_all(&mur_home, &profile.inner.name);
    let loaded = mur_common::skill::loader::filter_enabled(loaded, &profile.inner.disabled_skills);
    let runtime_skills = Arc::new(RuntimeSkills::build(loaded));
```

- [ ] **Step 2: Filter MCP servers before building the pool + tools**

In the same file, find (~line 233):

```rust
    let sandbox_policy = SandboxPolicy::from_entitlements(&profile.inner.entitlements, agent_home);
    let pool = McpPool::new(profile.inner.mcp_servers.clone(), sandbox_policy);
```

Replace with:

```rust
    let sandbox_policy = SandboxPolicy::from_entitlements(&profile.inner.entitlements, agent_home);
    // Phase-1 enable/disable: drop servers disabled for this agent so they
    // are never spawned and never advertised in tools/list.
    let enabled_mcp = profile.inner.enabled_mcp_servers();
    let pool = McpPool::new(enabled_mcp.clone(), sandbox_policy);
```

Then, a few lines below, find the `build_tools(...)` call that passes `&profile.inner.mcp_servers`:

```rust
    let (_defs, tool_map) = build_tools(
        Some((bash_def, bash_exec)),
        &profile.inner.mcp_servers,
        &tools_policy,
        pool.clone(),
    )
    .await;
```

Replace the second argument with the filtered list:

```rust
    let (_defs, tool_map) = build_tools(
        Some((bash_def, bash_exec)),
        &enabled_mcp,
        &tools_policy,
        pool.clone(),
    )
    .await;
```

- [ ] **Step 3: Build + lint the runtime**

Run: `ORT_STRATEGY=download cargo check -p mur-agent-runtime && ORT_STRATEGY=download cargo clippy -p mur-agent-runtime -- -D warnings`
Expected: builds clean, no clippy warnings.

- [ ] **Step 4: Run the existing runtime tests (no regressions)**

Run: `ORT_STRATEGY=download cargo nextest run -p mur-agent-runtime`
Expected: PASS (same set as before this task).

- [ ] **Step 5: Commit**

```bash
git add mur-agent-runtime/src/supervisor_runner.rs
git commit -m "feat(addons): enforce per-agent skill/mcp denylist at runtime startup

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: CLI — `mur agent skill enable|disable`

**Files:**
- Modify: `mur-core/src/cli/agent.rs` (`AgentSkillAction` enum)
- Modify: `mur-core/src/cmd/agent/skill.rs` (`cmd_skill_set_enabled`)
- Modify: `mur-core/src/cmd/agent/mod.rs` (re-export)
- Modify: dispatch site (located via `rg`)

**Interfaces:**
- Consumes: `load_profile_for_edit`, `save_profile`, `AgentProfile::set_skill_enabled` (Task 1)
- Produces: `pub fn cmd_skill_set_enabled(name: &str, skill_id: &str, enabled: bool) -> Result<()>`

- [ ] **Step 1: Add enum variants**

In `mur-core/src/cli/agent.rs`, add to `AgentSkillAction` (after `Show`):

```rust
    /// Enable a previously disabled skill for this agent (clears the denylist
    /// entry). Non-destructive — the skill is never re-installed or removed.
    Enable { name: String, skill_id: String },
    /// Disable a skill for this agent WITHOUT uninstalling it. The skill's
    /// files + stats are kept; it simply stops injecting. Applies on the
    /// agent's next restart.
    Disable { name: String, skill_id: String },
```

- [ ] **Step 2: Write the handler**

In `mur-core/src/cmd/agent/skill.rs`, add:

```rust
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
```

(`load_profile_for_edit` and `save_profile` are already imported in this module via the `cmd_skill_add` handler — no new imports needed.)

- [ ] **Step 3: Re-export the handler**

In `mur-core/src/cmd/agent/mod.rs`, update the skill re-export line:

```rust
pub use skill::{cmd_skill_add, cmd_skill_list, cmd_skill_remove, cmd_skill_set_enabled, cmd_skill_show};
```

- [ ] **Step 4: Wire the dispatch**

Locate the match over `AgentSkillAction`:
Run: `rg -n "AgentSkillAction::(Add|Remove)\b" mur-core/src`

In that match expression, add two arms next to the existing ones (mirror their `=>` style; if arms return via a helper like `cmd_skill_add(&name, &source)`, match that form):

```rust
        AgentSkillAction::Enable { name, skill_id } => cmd_skill_set_enabled(&name, &skill_id, true),
        AgentSkillAction::Disable { name, skill_id } => cmd_skill_set_enabled(&name, &skill_id, false),
```

- [ ] **Step 5: Build + verify end-to-end manually**

```bash
ORT_STRATEGY=download cargo build -p mur-core
export MUR_HOME=$(mktemp -d)
./target/debug/mur agent create probe --yes 2>/dev/null || ./target/debug/mur agent create probe
./target/debug/mur agent skill disable probe demo
rg "disabled_skills" "$MUR_HOME/agents/probe/profile.yaml"
```
Expected: `profile.yaml` now contains `disabled_skills:` with `- demo`. Then:
```bash
./target/debug/mur agent skill enable probe demo
rg "disabled_skills" "$MUR_HOME/agents/probe/profile.yaml" || echo "cleared (expected)"
```
Expected: the key is gone (empty denylist is not serialized). Clean up: `rm -rf "$MUR_HOME"; unset MUR_HOME`.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cli/agent.rs mur-core/src/cmd/agent/skill.rs mur-core/src/cmd/agent/mod.rs
# plus the dispatch file rg found:
git add -A
git commit -m "feat(addons): mur agent skill enable|disable

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: CLI — `mur agent mcp enable|disable`

**Files:**
- Modify: `mur-core/src/cli/agent.rs` (`AgentMcpAction` enum)
- Modify: `mur-core/src/cmd/agent/mcp.rs` (`cmd_mcp_set_enabled`)
- Modify: `mur-core/src/cmd/agent/mod.rs` (re-export)
- Modify: dispatch site

**Interfaces:**
- Consumes: `load_profile_for_edit`, `save_profile`, `AgentProfile::set_mcp_enabled` (Task 1)
- Produces: `pub fn cmd_mcp_set_enabled(name: &str, server_id: &str, enabled: bool) -> Result<()>`

- [ ] **Step 1: Add enum variants**

In `mur-core/src/cli/agent.rs`, add to `AgentMcpAction` (after `Inspect`):

```rust
    /// Enable a previously disabled MCP server for this agent.
    Enable { name: String, server_id: String },
    /// Disable an MCP server for this agent WITHOUT removing it. The entry +
    /// its pin stay in the profile; it simply stops spawning. Applies on the
    /// agent's next restart.
    Disable { name: String, server_id: String },
```

- [ ] **Step 2: Write the handler**

In `mur-core/src/cmd/agent/mcp.rs`, add:

```rust
/// Enable/disable an MCP server for an agent by editing the per-agent
/// denylist. Non-destructive: the entry (and its pin) stays in the profile.
pub fn cmd_mcp_set_enabled(name: &str, server_id: &str, enabled: bool) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile.mcp_servers.iter().any(|s| s.name == server_id) {
        bail!("MCP server '{server_id}' not found on '{name}'");
    }
    profile.set_mcp_enabled(server_id, enabled);
    save_profile(&path, &mut profile)?;
    println!(
        "{} MCP server '{server_id}' for '{name}' (restart the agent to apply)",
        if enabled { "Enabled" } else { "Disabled" }
    );
    Ok(())
}
```

- [ ] **Step 3: Re-export the handler**

In `mur-core/src/cmd/agent/mod.rs`, update the mcp re-export line:

```rust
pub use mcp::{McpAddPin, cmd_mcp_add, cmd_mcp_list, cmd_mcp_remove, cmd_mcp_rename, cmd_mcp_set_enabled};
```

- [ ] **Step 4: Wire the dispatch**

Run: `rg -n "AgentMcpAction::(Add|Remove)\b" mur-core/src`
Add two arms next to the existing ones:

```rust
        AgentMcpAction::Enable { name, server_id } => cmd_mcp_set_enabled(&name, &server_id, true),
        AgentMcpAction::Disable { name, server_id } => cmd_mcp_set_enabled(&name, &server_id, false),
```

- [ ] **Step 5: Build + manual verify**

```bash
ORT_STRATEGY=download cargo build -p mur-core
export MUR_HOME=$(mktemp -d)
./target/debug/mur agent create probe
./target/debug/mur agent mcp add probe demo --command /bin/echo --arg hi --force
./target/debug/mur agent mcp disable probe demo
rg "disabled_mcp" "$MUR_HOME/agents/probe/profile.yaml"
./target/debug/mur agent mcp disable probe nope 2>&1 | rg "not found"
rm -rf "$MUR_HOME"; unset MUR_HOME
```
Expected: `disabled_mcp: [demo]` written; toggling a missing server errors with "not found".

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "feat(addons): mur agent mcp enable|disable

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Hub backend — toggle commands + detail flags

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mcp_skills.rs` (two commands)
- Modify: `mur-hub-gui/src-tauri/src/detail.rs` (`enabled` on two views + populate)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (register)

**Interfaces:**
- Consumes: `cmd_skill_set_enabled`, `cmd_mcp_set_enabled` (Tasks 4-5); `get_agent_detail`
- Produces: Tauri commands `agent_skill_toggle(name, skillId, enabled)`, `agent_mcp_toggle(name, serverId, enabled)`; `InstalledSkillView.enabled`, `McpServerView.enabled`

- [ ] **Step 1: Add `enabled` to the two view structs**

In `mur-hub-gui/src-tauri/src/detail.rs`, add a field to `InstalledSkillView`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InstalledSkillView {
    pub name: String,
    pub version: String,
    pub description: String,
    pub category: String,
    pub enabled: bool,
}
```

and to `McpServerView`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct McpServerView {
    pub name: String,
    pub command: String,
    pub args: Vec<String>,
    pub enabled: bool,
}
```

- [ ] **Step 2: Populate `enabled` in `get_agent_detail`**

In `get_agent_detail`, capture the denylists before `profile` is consumed by the `.into_iter()` maps (add these lines just above where `installed_skills` / `mcp_servers` are built, ~line 209):

```rust
    let disabled_skills = profile.disabled_skills.clone();
    let disabled_mcp = profile.disabled_mcp.clone();
```

Update the `installed_skills` map:

```rust
        installed_skills: profile
            .installed_skills
            .into_iter()
            .map(|s| InstalledSkillView {
                enabled: !disabled_skills.iter().any(|n| n == &s.name),
                name: s.name,
                version: s.version,
                description: s.description,
                category: s.category,
            })
            .collect(),
```

Update the `mcp_servers` map:

```rust
        mcp_servers: profile
            .mcp_servers
            .into_iter()
            .map(|m| McpServerView {
                enabled: !disabled_mcp.iter().any(|n| n == &m.name),
                name: m.name,
                command: m.command,
                args: m.args,
            })
            .collect(),
```

- [ ] **Step 3: Add the two Tauri commands**

In `mur-hub-gui/src-tauri/src/mcp_skills.rs`, add `cmd_skill_set_enabled` and `cmd_mcp_set_enabled` to the existing `use mur_core::cmd::agent::{...}` import line (which already brings in `cmd_mcp_add`, `cmd_skill_remove`, etc.), then add:

```rust
#[tauri::command]
pub fn agent_skill_toggle(
    name: String,
    skill_id: String,
    enabled: bool,
) -> Result<AgentDetail, String> {
    cmd_skill_set_enabled(&name, &skill_id, enabled).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_mcp_toggle(
    name: String,
    server_id: String,
    enabled: bool,
) -> Result<AgentDetail, String> {
    cmd_mcp_set_enabled(&name, &server_id, enabled).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}
```

- [ ] **Step 4: Register the commands**

In `mur-hub-gui/src-tauri/src/lib.rs`, inside `tauri::generate_handler![ ... ]`, add after the existing `mcp_skills::agent_mcp_remove,` line:

```rust
            mcp_skills::agent_skill_toggle,
            mcp_skills::agent_mcp_toggle,
```

- [ ] **Step 5: Build + format the excluded crate**

Run:
```bash
cargo check --manifest-path mur-hub-gui/src-tauri/Cargo.toml
cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml
```
Expected: builds clean; fmt leaves no diff.

- [ ] **Step 6: Commit**

```bash
git add mur-hub-gui/src-tauri/src/mcp_skills.rs mur-hub-gui/src-tauri/src/detail.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(addons): Hub Tauri commands agent_skill_toggle / agent_mcp_toggle

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Hub UI — toggles on Skills + MCP tabs

**Files:**
- Modify: `mur-hub-gui/ui/src/types.ts` (`enabled` on two interfaces)
- Modify: `mur-hub-gui/ui/src/components/DetailPanel.tsx` (handlers + row controls)

**Interfaces:**
- Consumes: Tauri `agent_skill_toggle` / `agent_mcp_toggle` (Task 6); `InstalledSkillView.enabled`, `McpServerView.enabled`

- [ ] **Step 1: Add `enabled` to the TS interfaces**

In `mur-hub-gui/ui/src/types.ts`:

```typescript
export interface InstalledSkillView {
  name: string;
  version: string;
  description: string;
  category: string;
  enabled: boolean;
}
```

```typescript
export interface McpServerView {
  name: string;
  command: string;
  args: string[];
  enabled: boolean;
}
```

- [ ] **Step 2: Add the toggle handlers**

In `mur-hub-gui/ui/src/components/DetailPanel.tsx`, in the `SkillsTab` component near `removeSkill` (~line 823), add:

```typescript
  async function toggleSkill(id: string, enabled: boolean) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_skill_toggle", {
        name: detail.agent_name,
        skillId: id,
        enabled,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
```

In the `McpTab` component near `removeServer` (~line 985), add:

```typescript
  async function toggleServer(id: string, enabled: boolean) {
    setError(null);
    setBusy(true);
    try {
      const updated = await invoke<AgentDetail>("agent_mcp_toggle", {
        name: detail.agent_name,
        serverId: id,
        enabled,
      });
      onSaved(updated);
    } catch (e) {
      setError(String(e));
    } finally {
      setBusy(false);
    }
  }
```

- [ ] **Step 3: Add the toggle control to each skill row**

In `SkillsTab`'s `detail.installed_skills.map(...)` block (~line 874), add a checkbox before the name `<div>` and dim disabled rows:

```tsx
            {detail.installed_skills.map((s) => (
              <li key={s.name} className={s.enabled ? "item-card" : "item-card item-card-off"}>
                <button
                  className="item-card-remove"
                  title={t("detail.remove")}
                  aria-label={t("detail.remove")}
                  disabled={busy}
                  onClick={() => removeSkill(s.name)}
                >
                  ×
                </button>
                <label className="item-card-toggle" title={s.enabled ? "Disable" : "Enable"}>
                  <input
                    type="checkbox"
                    checked={s.enabled}
                    disabled={busy}
                    onChange={(e) => toggleSkill(s.name, e.target.checked)}
                  />
                </label>
                <div className="item-card-name">{s.name}</div>
                {s.version && <span className="badge-sm">{s.version}</span>}
                {s.description && <div className="item-card-desc">{s.description}</div>}
                {s.category && (
                  <span className="field-muted" style={{ fontSize: 11 }}>{s.category}</span>
                )}
              </li>
            ))}
```

- [ ] **Step 4: Add the toggle control to each MCP row**

In `McpTab`'s `detail.mcp_servers.map(...)` block (~line 1082):

```tsx
            {detail.mcp_servers.map((m) => (
              <li key={m.name} className={m.enabled ? "item-card" : "item-card item-card-off"}>
                <button
                  className="item-card-remove"
                  title={t("detail.remove")}
                  aria-label={t("detail.remove")}
                  disabled={busy}
                  onClick={() => removeServer(m.name)}
                >
                  ×
                </button>
                <label className="item-card-toggle" title={m.enabled ? "Disable" : "Enable"}>
                  <input
                    type="checkbox"
                    checked={m.enabled}
                    disabled={busy}
                    onChange={(e) => toggleServer(m.name, e.target.checked)}
                  />
                </label>
                <div className="item-card-name">{m.name}</div>
                <code className="item-card-code">{m.command}</code>
                {m.args.length > 0 && (
                  <div className="item-card-args">
                    {m.args.map((a, i) => (
                      <span key={i} className="badge-sm">{a}</span>
                    ))}
                  </div>
                )}
              </li>
            ))}
```

- [ ] **Step 5: Add minimal CSS for the off-state + toggle**

Append to the stylesheet that defines `.item-card` (find it: `rg -l "item-card-name" mur-hub-gui/ui/src`):

```css
.item-card-off { opacity: 0.55; }
.item-card-toggle { margin-right: 6px; cursor: pointer; }
```

- [ ] **Step 6: Typecheck + build the UI**

Run: `cd mur-hub-gui/ui && npm run build`
Expected: TypeScript compiles, build succeeds. (If `npm` isn't wired, run `npx tsc --noEmit`.)

- [ ] **Step 7: Manual verification**

Build + run the Hub (`cargo tauri build --bundles app` per project docs, or `cargo tauri dev`), open an agent's **Skills** tab, untick a skill → the row dims and `profile.yaml` gains `disabled_skills: [<name>]`; re-tick clears it. Repeat on **MCP**. (Toggle applies on the agent's next restart — see Notes.)

- [ ] **Step 8: Commit**

```bash
git add mur-hub-gui/ui/src/types.ts mur-hub-gui/ui/src/components/DetailPanel.tsx
git add -A   # picks up the CSS file rg located
git commit -m "feat(addons): Hub enable/disable toggles on Skills + MCP tabs

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>"
```

---

## Notes & deliberate Phase-1 simplifications

- **Apply-on-restart.** Toggles persist to `profile.yaml`; the runtime reads denylists once at `prepare_runtime`, so a change takes effect on the agent's next start. CLI + Hub both print/badge "restart to apply." Auto-restart-on-toggle in the Hub (the `model_ref` pattern) is a trivial follow-up, deferred to keep Task 7 small.
- **Legacy skills** (`AgentProfile.skills` path strings) are not toggleable in P1 — they remain always-on and the Skills tab's legacy `SkillView` rows show no switch (only `installed_skills` get one). Consistent with the spec.
- **No audit action / kill-switch in P1.** Those (`AuditAction::AddonToggle`, `mur agent addon disable-all`) land in Phase 2 with external imports, where the safety surface actually exists; P1 only toggles the user's own native items, for which `profile.yaml` history is the record.
- **Out of scope (Phase 2):** plugin-group imports (`AddonRef`, `mur agent addon import`, the Claude `SKILL.md`/`.mcp.json`/`commands.toml` converters, import-time validation) — separate plan.

---

## Self-Review

- **Spec coverage (P1 slice):** §3.1 denylist fields → Task 1. §3.3 rule (P1 = denylist only) → Task 1 (`skill_enabled`/`mcp_enabled`) + Task 2. §4 filter sites (skills+triggers, mcp) → Tasks 2-3. §9 CLI enable/disable → Tasks 4-5. §8 P1 toggles on existing tabs → Tasks 6-7. §12 back-compat (`serde(default)`, no schema bump) → Task 1 fields. §13 P1 tests (predicate truth table; denied skill absent from filtered load → trigger registry) → Tasks 1-2. Audit/kill-switch (§7) explicitly deferred to Phase 2 (documented in Notes). No P1 spec requirement is unimplemented.
- **Placeholder scan:** none — every code step shows full code; manual-verify steps give exact commands + expected output.
- **Type consistency:** `cmd_skill_set_enabled(name, skill_id, enabled)` / `cmd_mcp_set_enabled(name, server_id, enabled)` used identically in Tasks 4/5 (handlers), mod.rs re-exports, and Task 6 (Tauri). `enabled: bool` field name matches across `InstalledSkillView`/`McpServerView` (Rust) and the TS interfaces. `name_enabled`/`set_denylist`/`filter_enabled` signatures match between definition (Tasks 1-2) and use (Task 3).
