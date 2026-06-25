# Add-on System Phase 2 — Claude Plugin Importer — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let a MUR agent import a local Claude Code plugin directory (its skills, slash-commands, and MCP servers) as a per-agent, non-destructive, fail-closed-disabled "plugin-group" add-on, toggled as a unit from CLI and the Hub.

**Architecture:** A Claude plugin is an *import source*, not a new runtime object. The importer expands a plugin dir into existing primitives — `SkillManifest` (per-agent skill dirs) and `McpServerEntry` (this profile's `mcp_servers`) — and records one `AddonRef` on `AgentProfile` for provenance + cascade-toggle + uninstall-as-a-unit. Enforcement reuses the single Phase-1 filter site: the effective-enabled rule (`skill_enabled`/`mcp_enabled`) is extended to AND-in the group's `enabled` flag, so nothing new threads through the runtime. Imports install disabled; the only choke point that can set `enabled=true` is an explicit user toggle.

**Tech Stack:** Rust (edition 2024, workspace), `serde`/`serde_yaml_ng`/`serde_json`/`toml`, `sha2`; Tauri 2 + React/TS for the Hub.

**Source spec:** `docs/superpowers/specs/2026-06-23-mur-addon-system-and-claude-plugin-integration-design.md` — §3.1, §3.2, §3.3, §4, §6, §7, §8 (P2), §9 (P2), §10, §13 (P2).

**Builds on Phase 1** (PR #487, commit `d048f49e`): `AgentProfile.disabled_skills`/`disabled_mcp` + `skill_enabled`/`mcp_enabled`/`set_*_enabled`/`enabled_mcp_servers` (`mur-common/src/agent.rs`), `loader::filter_enabled` (`mur-common/src/skill/loader.rs`), the two enforcement sites (`mur-agent-runtime/src/supervisor_runner.rs:260`, `:524`), the `Enable`/`Disable` CLI verbs, and the Hub per-row toggles.

## Global Constraints

- **Rust edition 2024** — `let`-chains stable.
- **No hardcoded magic values.** Provenance prefixes and the local-source tag are module `const`s, not inline literals (Mandatory Rule 1).
- **`mur-core` build/test requires `ORT_STRATEGY=download`** prefixed (onnxruntime link failure otherwise). Same for `mur-agent-runtime`. Use targeted `cargo test -p <crate> <filter>` — never `cargo test --workspace` (it flakes 7 mur-core tests; CI uses nextest).
- **Single source file ≤ 800 lines** (Mandatory Rule 4). The importer is a submodule dir `cmd/agent/addon/{mod,parse,import}.rs`, mirroring sibling `cmd/agent/{…}.rs`.
- **Fail-closed.** Every imported `AddonRef` lands `enabled=false` at construction. There is no serde default-true and no code path in the importer or Hub that can construct an enabled imported group.
- **Imported skills are `TrustLevel::Sandboxed`** — achieved by *not* pinning them in the trust store; the loader resolves absent skills to `Sandboxed` (`loader.rs:116-141`). No trust-store writes in the importer.
- **env is never imported.** `.mcp.json` `env` keys are surfaced as a stdout notice only; nothing secret enters `profile.yaml`.
- **Brand:** user-facing copy uses lowercase "add-on"/"plugin" (these are not the MUR brand word).
- **Hub fmt gotcha:** the Tauri crate is workspace-excluded — run `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml` separately (root `cargo fmt` misses it).
- **Tauri undefined-drop gotcha:** optional Tauri command args use `Option<T>`.

---

### Task 1: Data model — `AddonRef`, `addons` field, group-aware effective-enabled rule

**Files:**
- Modify: `mur-common/src/agent.rs` (add `AddonRef` struct near `McpServerEntry` ~line 276; add `addons` field to `AgentProfile` after `disabled_mcp` ~line 86; extend `skill_enabled`/`mcp_enabled` ~line 1271-1278; add `group_of`/`set_addon_enabled`/`disable_all_addons` methods in the same `impl AgentProfile`; add unit test in the `tests` mod ~line 1300)
- Modify: `mur-core/src/cmd/agent/lifecycle.rs` (one exhaustive `AgentProfile { … }` literal — add `addons: Vec::new(),`)
- Modify: `mur-core/src/cmd/agent_companion/connector.rs` (one exhaustive `AgentProfile { … }` literal — add `addons: Vec::new(),`)
- Test: inline in `mur-common/src/agent.rs` `#[cfg(test)] mod tests`

**Interfaces:**
- Produces:
  - `mur_common::agent::AddonRef { id: String, source: String, enabled: bool, skills: Vec<String>, mcp: Vec<String>, commands: Vec<String> }` (derives `Debug, Clone, Serialize, Deserialize, PartialEq, Default`)
  - `AgentProfile.addons: Vec<AddonRef>`
  - `AgentProfile::group_of(&self, name: &str) -> Option<&AddonRef>`
  - `AgentProfile::skill_enabled(&self, &str) -> bool` and `::mcp_enabled(&self, &str) -> bool` — now denylist AND group rule (§3.3)
  - `AgentProfile::set_addon_enabled(&mut self, addon_id: &str, enabled: bool) -> bool` (false if no such addon)
  - `AgentProfile::disable_all_addons(&mut self)` — kill-switch
- Consumes: Phase-1 `name_enabled(&[String], &str) -> bool` and `set_denylist(&mut Vec<String>, &str, bool)` (both `pub fn` in `agent.rs`).

- [ ] **Step 1: Write the failing test** — append to `mur-common/src/agent.rs` `mod tests`:

```rust
#[test]
fn addon_group_rule_truth_table() {
    let mut p = AgentProfile::default_for_tests();
    p.addons.push(AddonRef {
        id: "grp".into(),
        source: "claude-local:grp@1.0.0".into(),
        enabled: false,
        skills: vec!["g_skill".into()],
        mcp: vec!["g_mcp".into()],
        commands: vec!["g_cmd".into()],
    });

    // 1. standalone item, no entry anywhere => enabled (back-compat)
    assert!(p.skill_enabled("standalone"));
    assert!(p.mcp_enabled("standalone_mcp"));

    // 2. grouped item, group disabled => off (cannot enable one member of a disabled group)
    assert!(!p.skill_enabled("g_skill"));
    assert!(!p.mcp_enabled("g_mcp"));

    // 3. grouped item, group enabled, name not denied => on
    assert!(p.set_addon_enabled("grp", true));
    assert!(p.skill_enabled("g_skill"));
    assert!(p.mcp_enabled("g_mcp"));

    // 4. name in denylist overrides an enabled group => off (silence one member)
    p.set_skill_enabled("g_skill", false);
    assert!(!p.skill_enabled("g_skill"));

    // set_addon_enabled on a missing id reports false
    assert!(!p.set_addon_enabled("nope", true));

    // kill-switch: clears every enabled flag and denies all members (skills+commands+mcp)
    p.set_skill_enabled("g_skill", true); // un-deny so kill-switch is what disables it
    p.disable_all_addons();
    assert!(p.addons.iter().all(|g| !g.enabled));
    assert!(!p.skill_enabled("g_skill"));
    assert!(!p.skill_enabled("g_cmd"));
    assert!(!p.mcp_enabled("g_mcp"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p mur-common addon_group_rule_truth_table`
Expected: FAIL — `AddonRef` and `addons`/methods do not exist (compile error).

- [ ] **Step 3: Add the `AddonRef` struct** — in `mur-common/src/agent.rs`, immediately after the `McpServerEntry` struct's closing `}` (~line 276):

```rust
/// A plugin-group imported by one agent (add-on Phase 2). Self-contained:
/// members are installed PER-AGENT (skills under
/// `~/.mur/agents/<a>/skills/`, mcp appended to this profile's
/// `mcp_servers`). No global library, no refcounting.
///
/// Fail-closed: `enabled` defaults to `false`. Only an explicit user
/// toggle (CLI/Hub) or a trusted native installer flips it true — the
/// importer always constructs it `false`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct AddonRef {
    /// e.g. "superpowers" (local) or "superpowers@claude-plugins-official".
    pub id: String,
    /// Provenance, free-text. e.g. "claude-local:superpowers@6.0.3".
    pub source: String,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mcp: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}
```

- [ ] **Step 4: Add the `addons` field to `AgentProfile`** — after the `disabled_mcp` field (~line 86):

```rust
    /// Plugin-groups imported by this agent (add-on Phase 2). Each is
    /// self-contained (members installed per-agent). Absent/empty in
    /// legacy profiles (back-compat).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub addons: Vec<AddonRef>,
```

- [ ] **Step 5: Extend the effective-enabled rule + add helpers** — replace the Phase-1 `skill_enabled`/`mcp_enabled` bodies (~line 1271-1278) and add the new methods in the same `impl AgentProfile`:

```rust
    /// The imported add-on group a skill/mcp/command name belongs to.
    pub fn group_of(&self, name: &str) -> Option<&AddonRef> {
        self.addons.iter().find(|g| {
            g.skills.iter().any(|n| n == name)
                || g.mcp.iter().any(|n| n == name)
                || g.commands.iter().any(|n| n == name)
        })
    }

    /// Whether `skill_name` is enabled (§3.3): not denied AND, if it
    /// belongs to an imported group, that group is enabled.
    pub fn skill_enabled(&self, skill_name: &str) -> bool {
        name_enabled(&self.disabled_skills, skill_name)
            && self.group_of(skill_name).is_none_or(|g| g.enabled)
    }

    /// Whether MCP server `server_id` is enabled (§3.3).
    pub fn mcp_enabled(&self, server_id: &str) -> bool {
        name_enabled(&self.disabled_mcp, server_id)
            && self.group_of(server_id).is_none_or(|g| g.enabled)
    }

    /// Toggle an imported plugin-group as a unit. Returns false if no
    /// add-on has that id.
    pub fn set_addon_enabled(&mut self, addon_id: &str, enabled: bool) -> bool {
        match self.addons.iter_mut().find(|g| g.id == addon_id) {
            Some(g) => {
                g.enabled = enabled;
                true
            }
            None => false,
        }
    }

    /// Emergency kill-switch (§7): clear every add-on's `enabled` and add
    /// all members (skills + commands + mcp) to the denylists.
    pub fn disable_all_addons(&mut self) {
        let skills: Vec<String> = self
            .addons
            .iter()
            .flat_map(|g| g.skills.iter().chain(g.commands.iter()).cloned())
            .collect();
        let mcp: Vec<String> = self.addons.iter().flat_map(|g| g.mcp.iter().cloned()).collect();
        for g in &mut self.addons {
            g.enabled = false;
        }
        for s in skills {
            set_denylist(&mut self.disabled_skills, &s, false);
        }
        for m in mcp {
            set_denylist(&mut self.disabled_mcp, &m, false);
        }
    }
```

> Keep the existing `set_skill_enabled`/`set_mcp_enabled`/`enabled_mcp_servers` methods unchanged — `enabled_mcp_servers` already routes through `mcp_enabled`, so the MCP enforcement site needs no edit.

- [ ] **Step 6: Fix the two exhaustive constructors.** In `mur-core/src/cmd/agent/lifecycle.rs` and `mur-core/src/cmd/agent_companion/connector.rs`, find the `AgentProfile { … }` struct literals (each already sets `disabled_skills`/`disabled_mcp` from Phase 1) and add next to them:

```rust
        addons: Vec::new(),
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test -p mur-common addon_group_rule_truth_table`
Expected: PASS.

- [ ] **Step 8: Confirm dependent crates still compile**

Run: `ORT_STRATEGY=download cargo check -p mur-core -p mur-agent-runtime`
Expected: clean (the two constructors compile with the new field).

- [ ] **Step 9: Commit**

```bash
git add mur-common/src/agent.rs mur-core/src/cmd/agent/lifecycle.rs mur-core/src/cmd/agent_companion/connector.rs
git commit -m "feat(addons): AddonRef + group-aware effective-enabled rule (Phase 2)"
```

---

### Task 2: Enforcement — make the supervisor skill filter group-aware

**Files:**
- Modify: `mur-agent-runtime/src/supervisor_runner.rs:524`
- Modify: `mur-common/src/skill/loader.rs` (remove the now-unused Phase-1 `filter_enabled` + its test, *only if* no other reference remains)

**Interfaces:**
- Consumes: `AgentProfile::skill_enabled` (Task 1, now group-aware). MCP enforcement at `supervisor_runner.rs:260` (`enabled_mcp_servers()`) already inherits the group rule via `mcp_enabled` — no edit.

**Note:** this is mechanical wiring. The behavior (a grouped-disabled skill drops out) is locked by Task 1's truth-table test; the predicate `skill_enabled` is the single source of truth. The gate here is "still compiles, runtime suite green, denylist filter still works through the new predicate."

- [ ] **Step 1: Replace the skill filter at `supervisor_runner.rs:524`**

Current (Phase 1):
```rust
    let loaded = mur_common::skill::loader::load_all(&mur_home, &profile.inner.name);
    let loaded = mur_common::skill::loader::filter_enabled(loaded, &profile.inner.disabled_skills);
    let runtime_skills = Arc::new(RuntimeSkills::build(loaded));
```

New (group-aware single predicate):
```rust
    let loaded = mur_common::skill::loader::load_all(&mur_home, &profile.inner.name);
    let loaded: Vec<_> = loaded
        .into_iter()
        .filter(|s| profile.inner.skill_enabled(&s.name))
        .collect();
    let runtime_skills = Arc::new(RuntimeSkills::build(loaded));
```

- [ ] **Step 2: Check whether `filter_enabled` is now dead**

Run: `grep -rn "filter_enabled" mur-common/src mur-core/src mur-agent-runtime/src mur-hub-gui/src-tauri/src`
Expected: only its definition + its test in `mur-common/src/skill/loader.rs` remain. (If any *other* caller exists, leave `filter_enabled` as-is and skip Step 3.)

- [ ] **Step 3: Delete the unused `filter_enabled` fn and its test** in `mur-common/src/skill/loader.rs` (the `pub fn filter_enabled(...)` ~line 148 and any `#[test]` exercising it). The denylist semantics it encoded now live inside `AgentProfile::skill_enabled`.

- [ ] **Step 4: Build the runtime + run its tests**

Run: `ORT_STRATEGY=download cargo test -p mur-agent-runtime`
Expected: PASS (no references to the removed fn; supervisor compiles).

- [ ] **Step 5: Run mur-common tests**

Run: `cargo test -p mur-common skill::loader`
Expected: PASS (no orphaned `filter_enabled` test).

- [ ] **Step 6: Commit**

```bash
git add mur-agent-runtime/src/supervisor_runner.rs mur-common/src/skill/loader.rs
git commit -m "feat(addons): group-aware skill enforcement at prepare_runtime (Phase 2)"
```

---

### Task 3: Plugin parsers + converters (pure functions)

**Files:**
- Create: `mur-core/src/cmd/agent/addon/mod.rs` (declares submodules; lifecycle handlers added in Task 5)
- Create: `mur-core/src/cmd/agent/addon/parse.rs`
- Modify: `mur-core/src/cmd/agent/mod.rs` (add `pub mod addon;` near the other `pub mod` lines ~line 27-45)
- Modify: `mur-core/Cargo.toml` (add `toml = "0.8"` — present in `mur-common` but not yet in `mur-core`)
- Test: inline `#[cfg(test)]` in `parse.rs`

**Interfaces:**
- Produces (all in `crate::cmd::agent::addon::parse`):
  - `PluginJson { name: String, version: String, description: String, author: Option<Author> }` (serde, from `plugin.json`)
  - `pub fn publisher_of(p: &PluginJson) -> String`
  - `pub fn manifest_version(p: &PluginJson) -> String` (plugin version or `"0.0.0"`)
  - `SkillMd { name: String, description: String, body: String }`
  - `pub fn parse_skill_md(raw: &str) -> SkillMd`
  - `pub fn skill_md_to_manifest(dir_name: &str, raw: &str, p: &PluginJson) -> SkillManifest`
  - `CommandToml { prompt: String, description: String }`
  - `pub fn command_to_manifest(cmd_name: &str, toml_src: &str, p: &PluginJson) -> anyhow::Result<SkillManifest>`
  - `McpJson { mcp_servers: BTreeMap<String, McpServerJson> }`, `McpServerJson { command: String, args: Vec<String>, env: BTreeMap<String,String> }`
  - `pub fn parse_mcp_json(src: &str) -> anyhow::Result<McpJson>`
- Consumes: `mur_common::skill::{SkillManifest, Content, Trigger, Procedure, TriggerKind, Category, Provenance, SkillScope, Priority}` (re-exported at `mur_common::skill`; `SkillScope` is the manifest visibility scope — `SkillScope::default() == User`).

- [ ] **Step 1: Add the `toml` dependency** to `mur-core/Cargo.toml` under `[dependencies]`:

```toml
toml = "0.8"
```

- [ ] **Step 2: Create `mur-core/src/cmd/agent/addon/mod.rs`** (handlers come in Task 5; for now just wire the submodules):

```rust
//! Per-agent add-on (Claude plugin) import + lifecycle (Phase 2).

pub mod import;
pub mod parse;
```

> Also create an empty placeholder `mur-core/src/cmd/agent/addon/import.rs` with `//! Claude plugin importer (Phase 2). Implemented in Task 4.` so the `pub mod import;` line compiles. Task 4 fills it.

- [ ] **Step 3: Register the module** — in `mur-core/src/cmd/agent/mod.rs`, add alongside the other `pub mod` lines (~line 27):

```rust
pub mod addon;
```

- [ ] **Step 4: Write the failing converter tests** — create `mur-core/src/cmd/agent/addon/parse.rs` with *only* the test module first:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use mur_common::skill::{Category, TriggerKind};

    fn plugin() -> PluginJson {
        PluginJson {
            name: "superpowers".into(),
            version: "6.0.3".into(),
            description: "test plugin".into(),
            author: Some(Author::Name("Acme".into())),
        }
    }

    #[test]
    fn parse_skill_md_splits_frontmatter_and_body() {
        let md = "---\nname: brainstorm\ndescription: helps you think\n---\nDo the thing.\n";
        let r = parse_skill_md(md);
        assert_eq!(r.name, "brainstorm");
        assert_eq!(r.description, "helps you think");
        assert_eq!(r.body, "Do the thing.\n");
    }

    #[test]
    fn skill_md_to_manifest_is_sandboxable_context() {
        let md = "---\nname: brainstorm\ndescription: helps you think\n---\nBody.\n";
        let m = skill_md_to_manifest("brainstorm-dir", md, &plugin());
        assert_eq!(m.name, "brainstorm");
        assert_eq!(m.publisher, "Acme");
        assert!(matches!(m.category, Category::Context));
        assert_eq!(m.content.r#abstract, "helps you think");
        assert_eq!(m.content.context.as_deref(), Some("Body.\n"));
        assert!(m.content.procedure.is_none() && m.content.command.is_none());
        // Keyword + Manual triggers, NO SessionStart (no auto-inject).
        assert!(m.triggers.iter().any(|t| t.kind == TriggerKind::Keyword));
        assert!(m.triggers.iter().any(|t| t.kind == TriggerKind::Manual));
        assert!(!m.triggers.iter().any(|t| t.kind == TriggerKind::SessionStart));
    }

    #[test]
    fn skill_md_falls_back_to_dir_name_when_frontmatter_missing() {
        let m = skill_md_to_manifest("my-dir", "no frontmatter here", &plugin());
        assert_eq!(m.name, "my-dir");
    }

    #[test]
    fn command_to_manifest_is_command_category() {
        let toml_src = "prompt = \"Review the diff: {{args}}\"\ndescription = \"code review\"\n";
        let m = command_to_manifest("review", toml_src, &plugin()).unwrap();
        assert_eq!(m.name, "review");
        assert!(matches!(m.category, Category::Command));
        assert_eq!(m.content.command.as_deref(), Some("Review the diff: {{args}}"));
        assert!(m.triggers.iter().any(|t| t.kind == TriggerKind::Command));
    }

    #[test]
    fn parse_mcp_json_reads_servers_and_env() {
        let src = r#"{"mcpServers":{"weather":{"command":"weather-mcp","args":["--port","9"],"env":{"API_KEY":"x"}}}}"#;
        let j = parse_mcp_json(src).unwrap();
        let s = j.mcp_servers.get("weather").unwrap();
        assert_eq!(s.command, "weather-mcp");
        assert_eq!(s.args, vec!["--port", "9"]);
        assert!(s.env.contains_key("API_KEY"));
    }
}
```

- [ ] **Step 5: Run the tests to verify they fail**

Run: `ORT_STRATEGY=download cargo test -p mur-core addon::parse`
Expected: FAIL — the types/functions are undefined (compile error).

- [ ] **Step 6: Implement the parsers + converters** — prepend to `mur-core/src/cmd/agent/addon/parse.rs` (above the test module):

```rust
//! Claude plugin → MUR primitive converters (pure, no I/O).

use std::collections::BTreeMap;

use anyhow::Result;
use serde::Deserialize;

use mur_common::skill::{
    Category, Content, Priority, Procedure, Provenance, SkillManifest, SkillScope, Trigger,
    TriggerKind,
};

/// `plugin.json` (only the fields we consume).
#[derive(Debug, Clone, Deserialize)]
pub struct PluginJson {
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub author: Option<Author>,
}

/// Claude `author` is either a bare string or `{ "name": ... }`.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum Author {
    Name(String),
    Obj { name: String },
}

pub fn publisher_of(p: &PluginJson) -> String {
    match &p.author {
        Some(Author::Name(s)) => s.clone(),
        Some(Author::Obj { name }) => name.clone(),
        None => p.name.clone(),
    }
}

pub fn manifest_version(p: &PluginJson) -> String {
    if p.version.is_empty() {
        "0.0.0".to_string()
    } else {
        p.version.clone()
    }
}

/// Build a `SkillManifest` with MUR-import defaults; callers vary the shape.
#[allow(clippy::too_many_arguments)]
fn base_manifest(
    name: String,
    publisher: String,
    description: String,
    category: Category,
    content: Content,
    triggers: Vec<Trigger>,
    tags: Vec<String>,
    version: String,
) -> SkillManifest {
    SkillManifest {
        name,
        version,
        publisher,
        description,
        category,
        scope: SkillScope::default(), // User
        fleet: None,
        team: None,
        governance: None,
        project: None,
        provenance: Provenance::Hybrid, // LLM-authored, human-reviewed source
        hosts: Vec::new(),
        content,
        requires: Vec::new(),
        tags,
        triggers,
        priority: Priority::default(),
        evolution_log: Vec::new(),
        transfer_chain: Vec::new(),
        mcp_requirements: Vec::new(),
        updated_at: chrono::Utc::now(),
    }
}

/// Parsed SKILL.md: YAML frontmatter `name`/`description` + markdown body.
#[derive(Debug, Clone, Default)]
pub struct SkillMd {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// Split a SKILL.md into frontmatter fields + body. A leading `---` ... `---`
/// fence is parsed as YAML; absent fence => whole file is the body.
pub fn parse_skill_md(raw: &str) -> SkillMd {
    if let Some(rest) = raw.strip_prefix("---")
        && let Some(end) = rest.find("\n---")
    {
        let fm = &rest[..end];
        // Body begins after the closing fence's line.
        let after = &rest[end + "\n---".len()..];
        let body = after
            .split_once('\n')
            .map(|(_, b)| b)
            .unwrap_or("")
            .to_string();
        let (mut name, mut description) = (String::new(), String::new());
        if let Ok(v) = serde_yaml_ng::from_str::<serde_yaml_ng::Value>(fm) {
            name = v.get("name").and_then(|x| x.as_str()).unwrap_or("").to_string();
            description = v
                .get("description")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string();
        }
        return SkillMd { name, description, body };
    }
    SkillMd { body: raw.to_string(), ..Default::default() }
}

/// Convert a Claude `skills/<dir>/SKILL.md` to a MUR `SkillManifest`.
/// Freeform instructions => `Category::Context` with `content.context = body`
/// (matches category↔mode validation; the spec's "procedure" wording is loose —
/// a Claude skill is a context blob, not a structured procedure). Abstract from
/// `description`. Triggers: Keyword(name) + Manual; NO SessionStart auto-inject.
pub fn skill_md_to_manifest(dir_name: &str, raw: &str, p: &PluginJson) -> SkillManifest {
    let parsed = parse_skill_md(raw);
    let name = if parsed.name.is_empty() {
        dir_name.to_string()
    } else {
        parsed.name
    };
    let content = Content {
        r#abstract: parsed.description.clone(),
        context: Some(parsed.body),
        procedure: None,
        command: None,
        note: None,
    };
    let triggers = vec![
        Trigger { kind: TriggerKind::Keyword, pattern: Some(name.clone()) },
        Trigger { kind: TriggerKind::Manual, pattern: None },
    ];
    base_manifest(
        name,
        publisher_of(p),
        parsed.description,
        Category::Context,
        content,
        triggers,
        p_tags(p),
        manifest_version(p),
    )
}

/// `commands/<name>.toml` (only `prompt`/`description`).
#[derive(Debug, Clone, Deserialize)]
pub struct CommandToml {
    pub prompt: String,
    #[serde(default)]
    pub description: String,
}

/// Convert a Claude slash-command TOML to a `Category::Command` skill.
/// `content.command = prompt` (`{{args}}` preserved); trigger `Command(/name)`.
/// Runtime semantics: instruction injection, no dispatcher (spec §6).
pub fn command_to_manifest(cmd_name: &str, toml_src: &str, p: &PluginJson) -> Result<SkillManifest> {
    let parsed: CommandToml = toml::from_str(toml_src)?;
    let description = if parsed.description.is_empty() {
        format!("Command: /{cmd_name}")
    } else {
        parsed.description
    };
    let content = Content {
        r#abstract: description.clone(),
        context: None,
        procedure: None,
        command: Some(parsed.prompt),
        note: None,
    };
    let triggers = vec![Trigger {
        kind: TriggerKind::Command,
        pattern: Some(format!("/{cmd_name}")),
    }];
    Ok(base_manifest(
        cmd_name.to_string(),
        publisher_of(p),
        description,
        Category::Command,
        content,
        triggers,
        p_tags(p),
        manifest_version(p),
    ))
}

fn p_tags(_p: &PluginJson) -> Vec<String> {
    // plugin.json carries no tags in the local-dir first cut; empty for now.
    Vec::new()
}

/// `.mcp.json` (the `mcpServers` map).
#[derive(Debug, Clone, Deserialize)]
pub struct McpJson {
    #[serde(rename = "mcpServers", default)]
    pub mcp_servers: BTreeMap<String, McpServerJson>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct McpServerJson {
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
}

pub fn parse_mcp_json(src: &str) -> Result<McpJson> {
    Ok(serde_json::from_str(src)?)
}
```

> Note the `Procedure` import is unused in this file by design (Content uses `context`/`command`, not `procedure`). Remove `Procedure` from the `use` list if clippy flags it — that's expected and correct.

- [ ] **Step 7: Run the tests to verify they pass**

Run: `ORT_STRATEGY=download cargo test -p mur-core addon::parse`
Expected: PASS (5 tests).

- [ ] **Step 8: Lint**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: clean (drop the unused `Procedure` import if flagged).

- [ ] **Step 9: Commit**

```bash
git add mur-core/Cargo.toml mur-core/src/cmd/agent/mod.rs mur-core/src/cmd/agent/addon/mod.rs mur-core/src/cmd/agent/addon/parse.rs
git commit -m "feat(addons): Claude plugin parsers + converters (Phase 2)"
```

---

### Task 4: The importer — `cmd_addon_import`

**Files:**
- Modify: `mur-core/src/cmd/agent/addon/import.rs` (replace the placeholder with the real importer)
- Test: inline `#[cfg(test)]` in `import.rs`

**Interfaces:**
- Produces: `pub fn cmd_addon_import(name: &str, plugin_dir: &str, force: bool) -> anyhow::Result<()>`
- Consumes:
  - `super::parse::{PluginJson, skill_md_to_manifest, command_to_manifest, parse_mcp_json, publisher_of, manifest_version}`
  - `crate::cmd::agent::{load_profile_for_edit, save_profile}` and `crate::cmd::resolve_mur_home` (profile load/save; the same helpers Phase-1 toggles use — `load_profile_for_edit(name) -> Result<(PathBuf, AgentProfile)>`, `save_profile(&Path, &mut AgentProfile)`)
  - `crate::cmd::agent_mcp_pin::{resolve_command, compute_binary_sha256, build_pinned_entry}` (all `pub fn`; `build_pinned_entry(name, command, args, binary_sha256, description_hash, publisher) -> McpServerEntry`)
  - `mur_common::skill::scan::scan_skill(&SkillManifest) -> Result<ContentScanReport, _>` + `ContentScanReport::has_blocking_findings()`
  - `mur_common::skill::write_to_dir(&Path, &SkillManifest) -> Result<PathBuf, _>`
  - `mur_common::agent::AddonRef`

> Verify `crate::cmd::agent_mcp_pin` is reachable (it is declared `pub mod agent_mcp_pin` in `mur-core/src/cmd/mod.rs`). If it is `mod` not `pub mod`, change it to `pub(crate) mod` in `cmd/mod.rs` as the first step.

- [ ] **Step 1: Write the failing test** — append to `mur-core/src/cmd/agent/addon/import.rs`. It builds a temp `~/.mur` + a fixture plugin dir, imports, and asserts the fail-closed + isolation + env-notice invariants (§13):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    // Minimal agent profile yaml on disk so load_profile_for_edit works.
    fn write_agent(home: &std::path::Path, name: &str) {
        let dir = home.join("agents").join(name);
        fs::create_dir_all(&dir).unwrap();
        let mut p = mur_common::agent::AgentProfile::default_for_tests();
        p.inner_name_set_for_test(name); // see note below
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
        fs::write(root.join("commands/review.toml"), "prompt = \"review {{args}}\"\n").unwrap();
        // MCP pointing at a real, always-present executable so sha256 pins.
        fs::write(
            root.join(".mcp.json"),
            r#"{"mcpServers":{"echo":{"command":"/bin/echo","args":["hi"],"env":{"TOKEN":"x"}}}}"#,
        )
        .unwrap();
    }

    #[test]
    fn import_is_fail_closed_isolated_and_pins_mcp() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        // Point the importer at this home.
        let _guard = crate::test_support::set_mur_home(home); // see note below
        write_agent(home, "alice");
        write_agent(home, "bob");
        let plugin = home.join("sample-plugin");
        write_plugin(&plugin);

        cmd_addon_import("alice", plugin.to_str().unwrap(), false).unwrap();

        // Reload alice's profile.
        let (_p, alice) =
            crate::cmd::agent::load_profile_for_edit("alice").unwrap();
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
        assert!(home.join("agents/alice/skills/brainstorm/skill.yaml").exists());
        assert!(!home.join("agents/bob/skills/brainstorm/skill.yaml").exists());
    }

    #[test]
    fn rejects_path_escaping_member_name() {
        // safe_member_name is the traversal guard.
        assert!(safe_member_name("ok").is_ok());
        assert!(safe_member_name("../evil").is_err());
        assert!(safe_member_name("a/b").is_err());
        assert!(safe_member_name("").is_err());
    }
}
```

> **Test-harness notes (resolve while implementing, do not invent silently):**
> - `cmd_addon_import` must resolve the mur home the same way `load_profile_for_edit` does. If the codebase keys off `$MUR_HOME`/`HOME`, the test sets that env var directly instead of a `test_support` helper — use whatever the existing `cmd/agent` tests already do (grep `tempdir` in `mur-core/src/cmd/agent`). Replace `crate::test_support::set_mur_home` and `inner_name_set_for_test` with the established pattern; if `AgentProfile.default_for_tests()` already names the agent, set the on-disk dir to match that name instead of renaming.
> - `tempfile` is already a dev-dependency in `mur-core` (used by sibling tests); confirm with `grep tempfile mur-core/Cargo.toml`.

- [ ] **Step 2: Run the test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core addon::import`
Expected: FAIL — `cmd_addon_import`/`safe_member_name` undefined.

- [ ] **Step 3: Implement the importer** — replace `mur-core/src/cmd/agent/addon/import.rs`'s placeholder with:

```rust
//! Claude plugin importer (Phase 2). Expands a local plugin dir into
//! per-agent skills + command-skills + MCP entries, recorded as one
//! fail-closed (disabled) `AddonRef`.

use std::fs;
use std::path::Path;

use anyhow::{Result, bail};

use mur_common::agent::AddonRef;
use mur_common::skill::scan::scan_skill;
use mur_common::skill::write_to_dir;

use super::parse::{
    PluginJson, command_to_manifest, manifest_version, parse_mcp_json, skill_md_to_manifest,
};
use crate::cmd::agent::{load_profile_for_edit, save_profile};
use crate::cmd::agent_mcp_pin::{build_pinned_entry, compute_binary_sha256, resolve_command};

/// Provenance tag for a plugin imported from a local directory (no marketplace).
const SOURCE_LOCAL: &str = "claude-local";
/// Shell-ish argv tokens that warrant an advisory (non-blocking) warning.
const SHELLISH_TOKENS: &[&str] = &["-c", "eval"];

/// Reject member names that could escape the agent's skills dir.
pub fn safe_member_name(n: &str) -> Result<()> {
    if n.is_empty() || n.contains('/') || n.contains('\\') || n.contains("..") {
        bail!("unsafe add-on member name: {n:?}");
    }
    Ok(())
}

pub fn cmd_addon_import(name: &str, plugin_dir: &str, force: bool) -> Result<()> {
    let (profile_path, mut profile) = load_profile_for_edit(name)?;
    let mur_home = crate::cmd::resolve_mur_home()?;
    let agent_skills_dir = mur_home.join("agents").join(name).join("skills");

    // Canonicalize the plugin root (rejects a non-existent dir).
    let root = fs::canonicalize(plugin_dir)
        .map_err(|e| anyhow::anyhow!("plugin dir {plugin_dir:?}: {e}"))?;
    let plugin: PluginJson = serde_json::from_str(
        &fs::read_to_string(root.join("plugin.json"))
            .map_err(|e| anyhow::anyhow!("read plugin.json: {e}"))?,
    )?;

    let addon_id = plugin.name.clone();
    if profile.addons.iter().any(|g| g.id == addon_id) {
        bail!("add-on '{addon_id}' already imported into '{name}'; remove it first");
    }

    let mut skill_members: Vec<String> = Vec::new();
    let mut cmd_members: Vec<String> = Vec::new();
    let mut mcp_members: Vec<String> = Vec::new();

    // 1. skills/<dir>/SKILL.md
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
            write_to_dir(&agent_skills_dir.join(&manifest.name), &manifest)
                .map_err(|e| anyhow::anyhow!("write skill {}: {e}", manifest.name))?;
            skill_members.push(manifest.name);
        }
    }

    // 2. commands/<name>.toml
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
            write_to_dir(&agent_skills_dir.join(&manifest.name), &manifest)
                .map_err(|e| anyhow::anyhow!("write command {}: {e}", manifest.name))?;
            cmd_members.push(manifest.name);
        }
    }

    // 3. .mcp.json — validate command, pin sha256, surface env as a notice.
    let mcp_path = root.join(".mcp.json");
    if mcp_path.is_file() {
        let j = parse_mcp_json(&fs::read_to_string(&mcp_path)?)?;
        for (server, srv) in j.mcp_servers {
            if profile.mcp_servers.iter().any(|m| m.name == server) {
                bail!("MCP server '{server}' already exists on '{name}'; rename or remove it first");
            }
            // Resolve + hash the binary (rejects path-escape / missing binary).
            let resolved = resolve_command(&srv.command)
                .map_err(|e| anyhow::anyhow!("MCP '{server}' command {:?}: {e}", srv.command))?;
            let sha = compute_binary_sha256(&resolved)?;
            if srv.args.iter().any(|a| SHELLISH_TOKENS.contains(&a.as_str())) {
                eprintln!(
                    "warning: MCP '{server}' args contain shell-ish tokens {SHELLISH_TOKENS:?}; review before enabling"
                );
            }
            if !srv.env.is_empty() {
                println!("note: MCP '{server}' declares env vars (NOT imported). Wire them with:");
                for k in srv.env.keys() {
                    println!("  mur agent secret set {name} {k}");
                }
            }
            let entry =
                build_pinned_entry(&server, &srv.command, &srv.args, sha, String::new(), None);
            profile.mcp_servers.push(entry);
            mcp_members.push(server);
        }
    }

    // 4. Record the group — FAIL-CLOSED (disabled). The only choke point.
    profile.addons.push(AddonRef {
        id: addon_id.clone(),
        source: format!("{SOURCE_LOCAL}:{}@{}", plugin.name, manifest_version(&plugin)),
        enabled: false,
        skills: skill_members,
        mcp: mcp_members,
        commands: cmd_members,
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

fn _path_unused(_: &Path) {}
```

> Drop the `_path_unused`/`Path` import if clippy flags them — they're only there to keep the `use std::path::Path` honest; prefer removing both.

- [ ] **Step 4: Run the test to verify it passes**

Run: `ORT_STRATEGY=download cargo test -p mur-core addon::import`
Expected: PASS (2 tests). If `resolve_mur_home`/home-resolution differs, fix the test harness note in Step 1 — not the production resolution.

- [ ] **Step 5: Lint**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`
Expected: clean.

- [ ] **Step 6: Commit**

```bash
git add mur-core/src/cmd/agent/addon/import.rs
git commit -m "feat(addons): Claude plugin importer — per-agent, fail-closed, MCP-pinned (Phase 2)"
```

---

### Task 5: Add-on lifecycle CLI + audit (`list`/`enable`/`disable`/`remove`/`disable-all`)

**Files:**
- Modify: `mur-core/src/cmd/agent/addon/mod.rs` (add the handlers + audit helper)
- Modify: `mur-core/src/conversations/audit.rs` (add `AuditAction::AddonToggle`)
- Modify: `mur-core/src/cli/agent.rs` (add `AgentAddonAction` enum + `Addon` variant on `AgentAction`)
- Modify: `mur-core/src/dispatch.rs` (route `AgentAction::Addon`)
- Test: inline in `mur-core/src/cmd/agent/addon/mod.rs`

**Interfaces:**
- Produces (in `crate::cmd::agent::addon`):
  - `pub fn cmd_addon_list(name: &str) -> Result<()>`
  - `pub fn cmd_addon_set_enabled(name: &str, addon_id: &str, enabled: bool) -> Result<()>`
  - `pub fn cmd_addon_remove(name: &str, addon_id: &str) -> Result<()>`
  - `pub fn cmd_addon_disable_all(name: &str) -> Result<()>`
- Consumes: `AuditAction::AddonToggle { agent, target, enabled }`, `Audit::open`/`append`, Task 4's `cmd_addon_import`, Task 1's profile helpers.

- [ ] **Step 1: Add the audit variant** — in `mur-core/src/conversations/audit.rs`, add to the `AuditAction` enum (after `Governance { … }`):

```rust
    /// An add-on (plugin-group / skill / mcp) was enabled/disabled for an agent.
    AddonToggle {
        agent: String,
        target: String,
        enabled: bool,
    },
```

- [ ] **Step 2: Write the failing handler test** — append to `mur-core/src/cmd/agent/addon/mod.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggle_and_remove_addon_round_trip() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        let _guard = crate::test_support::set_mur_home(home); // match Task 4's harness pattern
        let dir = home.join("agents/alice");
        std::fs::create_dir_all(dir.join("skills/s1")).unwrap();
        let mut p = mur_common::agent::AgentProfile::default_for_tests();
        // name the on-disk agent "alice" per the established test pattern
        p.addons.push(mur_common::agent::AddonRef {
            id: "grp".into(),
            source: "claude-local:grp@1.0.0".into(),
            enabled: false,
            skills: vec!["s1".into()],
            mcp: vec![],
            commands: vec![],
        });
        std::fs::write(dir.join("profile.yaml"), serde_yaml_ng::to_string(&p).unwrap()).unwrap();
        std::fs::write(dir.join("skills/s1/skill.yaml"), "name: s1\n").unwrap();

        cmd_addon_set_enabled("alice", "grp", true).unwrap();
        let (_pp, after) = crate::cmd::agent::load_profile_for_edit("alice").unwrap();
        assert!(after.addons[0].enabled);

        // unknown id errors
        assert!(cmd_addon_set_enabled("alice", "nope", true).is_err());

        // remove deletes the per-agent skill dir + drops the AddonRef
        cmd_addon_remove("alice", "grp").unwrap();
        let (_pp, gone) = crate::cmd::agent::load_profile_for_edit("alice").unwrap();
        assert!(gone.addons.is_empty());
        assert!(!home.join("agents/alice/skills/s1").exists());
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `ORT_STRATEGY=download cargo test -p mur-core addon::tests::toggle_and_remove`
Expected: FAIL — handlers undefined.

- [ ] **Step 4: Implement the handlers** — prepend to `mur-core/src/cmd/agent/addon/mod.rs` (above the test mod, keeping the `pub mod` lines):

```rust
//! Per-agent add-on (Claude plugin) import + lifecycle (Phase 2).

pub mod import;
pub mod parse;

use anyhow::{Result, bail};

use crate::cmd::agent::{load_profile_for_edit, save_profile};
use crate::conversations::audit::{Audit, AuditAction};

pub use import::cmd_addon_import;

pub fn cmd_addon_list(name: &str) -> Result<()> {
    let (_path, profile) = load_profile_for_edit(name)?;
    if profile.addons.is_empty() {
        println!("No add-ons imported for '{name}'.");
        return Ok(());
    }
    for g in &profile.addons {
        println!(
            "{}  {}  [{}]  (skills:{} mcp:{} commands:{})",
            if g.enabled { "on " } else { "off" },
            g.id,
            g.source,
            g.skills.len(),
            g.mcp.len(),
            g.commands.len(),
        );
    }
    Ok(())
}

pub fn cmd_addon_set_enabled(name: &str, addon_id: &str, enabled: bool) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    if !profile.set_addon_enabled(addon_id, enabled) {
        bail!("add-on '{addon_id}' not found on '{name}'");
    }
    save_profile(&path, &mut profile)?;
    audit_toggle(name, addon_id, enabled);
    println!(
        "{} add-on '{addon_id}' for '{name}' (restart the agent to apply)",
        if enabled { "Enabled" } else { "Disabled" }
    );
    Ok(())
}

pub fn cmd_addon_remove(name: &str, addon_id: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    let Some(pos) = profile.addons.iter().position(|g| g.id == addon_id) else {
        bail!("add-on '{addon_id}' not found on '{name}'");
    };
    let group = profile.addons.remove(pos);
    let mur_home = crate::cmd::resolve_mur_home()?;
    let skills_dir = mur_home.join("agents").join(name).join("skills");

    // Remove per-agent skill + command-skill dirs.
    for member in group.skills.iter().chain(group.commands.iter()) {
        let d = skills_dir.join(member);
        if d.exists() {
            let _ = std::fs::remove_dir_all(&d);
        }
    }
    // Drop member MCP entries and tidy denylists (no orphaned names).
    profile.mcp_servers.retain(|m| !group.mcp.contains(&m.name));
    profile
        .disabled_skills
        .retain(|n| !group.skills.contains(n) && !group.commands.contains(n));
    profile.disabled_mcp.retain(|n| !group.mcp.contains(n));

    save_profile(&path, &mut profile)?;
    println!("Removed add-on '{addon_id}' from '{name}'.");
    Ok(())
}

pub fn cmd_addon_disable_all(name: &str) -> Result<()> {
    let (path, mut profile) = load_profile_for_edit(name)?;
    profile.disable_all_addons();
    save_profile(&path, &mut profile)?;
    audit_toggle(name, "*", false);
    println!("Kill-switch: disabled ALL add-ons for '{name}' (restart the agent to apply).");
    Ok(())
}

/// Best-effort audit; a logging failure must not block the toggle.
fn audit_toggle(agent: &str, target: &str, enabled: bool) {
    if let Ok(mur_home) = crate::cmd::resolve_mur_home()
        && let Ok(audit) = Audit::open(Some(&mur_home))
    {
        let _ = audit.append(
            AuditAction::AddonToggle {
                agent: agent.to_string(),
                target: target.to_string(),
                enabled,
            },
            String::new(),
        );
    }
}
```

> Confirm `Audit::open`'s exact signature (`Audit::open(Some(&Path))` per `audit.rs`) and the `crate::conversations::audit` path; adjust the import if the module is re-exported elsewhere. If `Audit::open` takes `Option<&Path>` vs `Option<PathBuf>`, match it.

- [ ] **Step 5: Add the CLI enum + variant** — in `mur-core/src/cli/agent.rs`:

Add the `Addon` variant to the `AgentAction` enum (mirror the existing `Mcp`/`Skill`/`Secret` subcommand variants):

```rust
    /// Manage imported add-ons (Claude plugin-groups).
    Addon {
        #[command(subcommand)]
        action: AgentAddonAction,
    },
```

Add the new enum near `AgentMcpAction`:

```rust
#[derive(Subcommand)]
pub enum AgentAddonAction {
    /// Import a local Claude plugin directory as a per-agent add-on (installs DISABLED).
    Import {
        name: String,
        plugin_dir: String,
        #[arg(long)]
        force: bool,
    },
    /// List imported add-ons and their on/off state.
    List { name: String },
    /// Enable an imported add-on (cascade to its members).
    Enable { name: String, addon_id: String },
    /// Disable an imported add-on (cascade to its members).
    Disable { name: String, addon_id: String },
    /// Uninstall an add-on: delete its per-agent skills + member MCP entries.
    Remove { name: String, addon_id: String },
    /// Emergency kill-switch: disable every add-on and deny all members.
    DisableAll { name: String },
}
```

- [ ] **Step 6: Route the dispatch** — in `mur-core/src/dispatch.rs`, add an arm for `AgentAction::Addon` mirroring `AgentAction::Skill`. Add `use crate::cli::agent::AgentAddonAction;` (or the qualified path) and:

```rust
        AgentAction::Addon { action } => match action {
            AgentAddonAction::Import { name, plugin_dir, force } => {
                cmd::agent::addon::cmd_addon_import(&name, &plugin_dir, force)?
            }
            AgentAddonAction::List { name } => cmd::agent::addon::cmd_addon_list(&name)?,
            AgentAddonAction::Enable { name, addon_id } => {
                cmd::agent::addon::cmd_addon_set_enabled(&name, &addon_id, true)?
            }
            AgentAddonAction::Disable { name, addon_id } => {
                cmd::agent::addon::cmd_addon_set_enabled(&name, &addon_id, false)?
            }
            AgentAddonAction::Remove { name, addon_id } => {
                cmd::agent::addon::cmd_addon_remove(&name, &addon_id)?
            }
            AgentAddonAction::DisableAll { name } => {
                cmd::agent::addon::cmd_addon_disable_all(&name)?
            }
        },
```

> Match the existing dispatch style: if `AgentAction::Skill` is destructured as `{ action }` and matched inline, copy that; if it calls a `dispatch_agent_skill(action)` helper, add a parallel `dispatch_agent_addon`. Grep `AgentSkillAction::Enable` in `dispatch.rs` and follow it exactly.

- [ ] **Step 7: Run the handler test + build the binary**

Run: `ORT_STRATEGY=download cargo test -p mur-core addon::`
Expected: PASS (parse + import + lifecycle tests).

Run: `ORT_STRATEGY=download cargo build -p mur-core`
Expected: clean (CLI + dispatch compile; `AuditAction` match arms exhaustive wherever it's matched — fix any non-exhaustive match the compiler flags).

- [ ] **Step 8: Manual smoke (optional but recommended)**

```bash
ORT_STRATEGY=download cargo run -p mur-core -- agent addon import <existing-agent> /path/to/a/claude/plugin
ORT_STRATEGY=download cargo run -p mur-core -- agent addon list <existing-agent>
ORT_STRATEGY=download cargo run -p mur-core -- agent addon enable <existing-agent> <id>
ORT_STRATEGY=download cargo run -p mur-core -- agent addon disable-all <existing-agent>
```
Expected: import prints "(disabled)"; list shows `off`; enable flips to `on`; disable-all returns everything to `off`.

- [ ] **Step 9: Lint + commit**

Run: `ORT_STRATEGY=download cargo clippy -p mur-core -- -D warnings`

```bash
git add mur-core/src/cmd/agent/addon/mod.rs mur-core/src/conversations/audit.rs mur-core/src/cli/agent.rs mur-core/src/dispatch.rs
git commit -m "feat(addons): addon list/enable/disable/remove/disable-all CLI + audit (Phase 2)"
```

---

### Task 6: Hub backend — `AgentDetail.addons` + Tauri commands + group-aware row flags

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/detail.rs` (add `InstalledAddonView`; add `addons` to `AgentDetail`; make `InstalledSkillView`/`McpServerView` `enabled` group-aware + add `addon_id: Option<String>`)
- Modify: `mur-hub-gui/src-tauri/src/mcp_skills.rs` (add `agent_addon_import`/`agent_addon_toggle`/`agent_addon_remove` Tauri commands)
- Modify: `mur-hub-gui/src-tauri/src/lib.rs` (register the three commands in `generate_handler!`)
- Test: inline in `detail.rs`

**Interfaces:**
- Produces:
  - `InstalledAddonView { id: String, source: String, enabled: bool, skills: Vec<String>, mcp: Vec<String>, commands: Vec<String> }`
  - `AgentDetail.addons: Vec<InstalledAddonView>`
  - `InstalledSkillView.addon_id: Option<String>`, `McpServerView.addon_id: Option<String>`
  - Tauri: `agent_addon_import(name, plugin_dir, force: Option<bool>) -> Result<AgentDetail, String>`, `agent_addon_toggle(name, addon_id, enabled) -> …`, `agent_addon_remove(name, addon_id) -> …`
- Consumes: Task 4/5 handlers; Task 1 `AgentProfile::{skill_enabled, mcp_enabled, group_of}`.

- [ ] **Step 1: Write the failing detail test** — append to `mur-hub-gui/src-tauri/src/detail.rs`:

```rust
#[cfg(test)]
mod addon_detail_tests {
    use super::*;

    #[test]
    fn detail_surfaces_addons_and_group_off_dims_members() {
        let mut p = mur_common::agent::AgentProfile::default_for_tests();
        p.installed_skills.push(mur_common::agent::SkillCardEntry {
            name: "g_skill".into(),
            ..Default::default()
        });
        p.addons.push(mur_common::agent::AddonRef {
            id: "grp".into(),
            source: "claude-local:grp@1.0.0".into(),
            enabled: false,
            skills: vec!["g_skill".into()],
            mcp: vec![],
            commands: vec![],
        });

        let detail = build_agent_detail_from_profile(&p); // the existing profile->view mapper
        assert_eq!(detail.addons.len(), 1);
        assert!(!detail.addons[0].enabled);
        let row = detail.installed_skills.iter().find(|s| s.name == "g_skill").unwrap();
        // group off => member shown disabled, with its addon id for the badge
        assert!(!row.enabled);
        assert_eq!(row.addon_id.as_deref(), Some("grp"));
    }
}
```

> Replace `build_agent_detail_from_profile` with the actual internal function `detail.rs` uses to map an `AgentProfile` into `AgentDetail` (grep `InstalledSkillView {` in `detail.rs` to find where the views are constructed). If the mapper only exists inside `get_agent_detail` (which reads from disk), extract the profile→detail mapping into a small `fn build_detail(profile: &AgentProfile) -> AgentDetail` and call it from both — that refactor is part of this step.

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml addon_detail`
Expected: FAIL — `addons`/`addon_id` undefined.

- [ ] **Step 3: Extend the view structs** — in `mur-hub-gui/src-tauri/src/detail.rs`:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct InstalledAddonView {
    pub id: String,
    pub source: String,
    pub enabled: bool,
    pub skills: Vec<String>,
    pub mcp: Vec<String>,
    pub commands: Vec<String>,
}
```

Add `pub addon_id: Option<String>` to `InstalledSkillView` and `McpServerView`. Add `pub addons: Vec<InstalledAddonView>` to `AgentDetail`.

In the profile→detail mapping, set each skill/mcp row's `enabled` via the group-aware predicate and populate `addon_id`:

```rust
            enabled: profile.skill_enabled(&name),
            addon_id: profile.group_of(&name).map(|g| g.id.clone()),
```
```rust
            enabled: profile.mcp_enabled(&server.name),
            addon_id: profile.group_of(&server.name).map(|g| g.id.clone()),
```

And build `addons`:

```rust
        addons: profile
            .addons
            .iter()
            .map(|g| InstalledAddonView {
                id: g.id.clone(),
                source: g.source.clone(),
                enabled: g.enabled,
                skills: g.skills.clone(),
                mcp: g.mcp.clone(),
                commands: g.commands.clone(),
            })
            .collect(),
```

- [ ] **Step 4: Add the Tauri commands** — in `mur-hub-gui/src-tauri/src/mcp_skills.rs` (mirroring `agent_skill_toggle`):

```rust
#[tauri::command]
pub fn agent_addon_import(
    name: String,
    plugin_dir: String,
    force: Option<bool>,
) -> Result<AgentDetail, String> {
    mur_core::cmd::agent::addon::cmd_addon_import(&name, &plugin_dir, force.unwrap_or(false))
        .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_addon_toggle(
    name: String,
    addon_id: String,
    enabled: bool,
) -> Result<AgentDetail, String> {
    mur_core::cmd::agent::addon::cmd_addon_set_enabled(&name, &addon_id, enabled)
        .map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}

#[tauri::command]
pub fn agent_addon_remove(name: String, addon_id: String) -> Result<AgentDetail, String> {
    mur_core::cmd::agent::addon::cmd_addon_remove(&name, &addon_id).map_err(|e| format!("{e:#}"))?;
    get_agent_detail(name)
}
```

> Match the existing crate path the Hub uses to reach core handlers (the P1 toggles call `cmd_skill_set_enabled` — find its `use`/path and mirror it; it may be `mur_core::cmd::agent::…` or a re-export). The Hub reads `addons` straight off `AgentDetail`, so **no** `agent_addon_list` command is needed — skipped intentionally.

- [ ] **Step 5: Register the commands** — in `mur-hub-gui/src-tauri/src/lib.rs` `generate_handler!`, beside `agent_skill_toggle`/`agent_mcp_toggle`:

```rust
            mcp_skills::agent_addon_import,
            mcp_skills::agent_addon_toggle,
            mcp_skills::agent_addon_remove,
```

- [ ] **Step 6: Run the test + build the crate**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml addon_detail`
Expected: PASS.

Run: `cargo build --manifest-path mur-hub-gui/src-tauri/Cargo.toml`
Expected: clean.

- [ ] **Step 7: Format (excluded-crate fmt) + commit**

Run: `cargo fmt --manifest-path mur-hub-gui/src-tauri/Cargo.toml`

```bash
git add mur-hub-gui/src-tauri/src/detail.rs mur-hub-gui/src-tauri/src/mcp_skills.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(addons): Hub backend — addons in AgentDetail + import/toggle/remove commands (Phase 2)"
```

---

### Task 7: Hub frontend — "Plugins" tab + `(plugin off)` member badges

**Files:**
- Modify: `mur-hub-gui/ui/src/types.ts` (add `InstalledAddonView`; add `addons` to `AgentDetail`; add `addon_id?: string | null` to `InstalledSkillView`/`McpServerView`; add `"plugins"` to the `DetailTab` union)
- Modify: `mur-hub-gui/ui/src/components/DetailPanel.tsx` (add the Plugins tab: cascade toggle + Import button + remove; add `(plugin off)` badge on Skills/MCP member rows)

**Interfaces:**
- Consumes Task 6 Tauri commands `agent_addon_import` / `agent_addon_toggle` / `agent_addon_remove` and the new `AgentDetail.addons` field.

**Test gate:** matching Phase-1 precedent, the Hub UI has no unit-test framework — the gate is `tsc` + `vite build` clean plus a manual click-through. (Don't add a test framework; YAGNI.)

- [ ] **Step 1: Extend the TS types** — in `mur-hub-gui/ui/src/types.ts`:

```ts
export interface InstalledAddonView {
  id: string;
  source: string;
  enabled: boolean;
  skills: string[];
  mcp: string[];
  commands: string[];
}
```

Add to `InstalledSkillView` and `McpServerView`:
```ts
  addon_id?: string | null;
```
Add to `AgentDetail`:
```ts
  addons: InstalledAddonView[];
```
Add `"plugins"` to the `DetailTab` union type.

- [ ] **Step 2: Add the Plugins tab nav entry** in `DetailPanel.tsx` — beside the existing `skills`/`mcp` tab buttons, add a `plugins` tab button following the exact same pattern (the `setTab("plugins")` button + the conditional `{tab === "plugins" && (…)}` panel).

- [ ] **Step 3: Implement the Plugins panel** — inside the `{tab === "plugins" && (…)}` block, mirror the Skills tab's `toggleSkill`/`removeSkill` structure:

```tsx
async function importPlugin() {
  setError(null);
  setBusy(true);
  try {
    const dir = await open({ directory: true, title: "Select a Claude plugin folder" });
    if (!dir || Array.isArray(dir)) return;
    const updated = await invoke<AgentDetail>("agent_addon_import", {
      name: detail.agent_name,
      pluginDir: dir,
      force: false,
    });
    onSaved(updated);
  } catch (e) {
    setError(String(e));
  } finally {
    setBusy(false);
  }
}

async function toggleAddon(id: string, enabled: boolean) {
  setError(null);
  setBusy(true);
  try {
    const updated = await invoke<AgentDetail>("agent_addon_toggle", {
      name: detail.agent_name,
      addonId: id,
      enabled,
    });
    onSaved(updated);
  } catch (e) {
    setError(String(e));
  } finally {
    setBusy(false);
  }
}

async function removeAddon(id: string) {
  setError(null);
  setBusy(true);
  try {
    const updated = await invoke<AgentDetail>("agent_addon_remove", {
      name: detail.agent_name,
      addonId: id,
    });
    onSaved(updated);
  } catch (e) {
    setError(String(e));
  } finally {
    setBusy(false);
  }
}
```

```tsx
<div className="tab-panel">
  <button className="btn-primary" disabled={busy} onClick={importPlugin}>
    Import plugin…
  </button>
  {detail.addons.length === 0 && <p className="field-muted">No add-ons imported.</p>}
  <ul className="item-list">
    {detail.addons.map((g) => (
      <li key={g.id} className={g.enabled ? "item-card" : "item-card item-card-off"}>
        <button className="item-card-remove" onClick={() => removeAddon(g.id)}>×</button>
        <label className="item-card-toggle" title={g.enabled ? "Disable" : "Enable"}>
          <input
            type="checkbox"
            checked={g.enabled}
            disabled={busy}
            onChange={(e) => toggleAddon(g.id, e.target.checked)}
          />
        </label>
        <div className="item-card-name">{g.id}</div>
        <div className="item-card-desc">{g.source}</div>
        <span className="field-muted">
          skills:{g.skills.length} · mcp:{g.mcp.length} · commands:{g.commands.length}
        </span>
      </li>
    ))}
  </ul>
</div>
```

> `open` is `@tauri-apps/plugin-dialog`'s directory picker — reuse the exact `import { open } from …` the Skills tab already uses for skill install (grep `from "@tauri-apps/plugin-dialog"` in `DetailPanel.tsx`). The Tauri arg keys are camelCase (`pluginDir`, `addonId`) — Tauri snake_case↔camelCase maps them to the Rust `plugin_dir`/`addon_id`.

- [ ] **Step 4: Add the `(plugin off)` badge** on Skills + MCP member rows. In the existing `detail.installed_skills.map(...)` and `detail.mcp_servers.map(...)` row JSX, after the name, add:

```tsx
{!s.enabled && s.addon_id && <span className="badge-sm">plugin off</span>}
```
(and the analogous `!m.enabled && m.addon_id` for MCP rows). The `item-card-off` dim class is already applied by the P1 `enabled` ternary — group-disabled rows now dim automatically because Task 6 made `enabled` group-aware.

- [ ] **Step 5: Type-check + build**

Run: `cd mur-hub-gui/ui && npm run build`
Expected: `tsc` + `vite build` clean (no type errors; `addons`/`addon_id`/`"plugins"` all resolve).

- [ ] **Step 6: Manual click-through** (if a Hub `.app` is available): open an agent → **Plugins** tab → **Import plugin…** → pick a plugin dir → row appears **off** → flip on → switch to **Skills**/**MCP** tabs → members no longer show "plugin off" → remove → rows disappear.

- [ ] **Step 7: Commit**

```bash
git add mur-hub-gui/ui/src/types.ts mur-hub-gui/ui/src/components/DetailPanel.tsx
git commit -m "feat(addons): Hub Plugins tab — import + cascade toggle + plugin-off badges (Phase 2)"
```

---

## Self-Review

**Spec coverage:**
- §3.1 `AddonRef` + `addons` field → Task 1. ✔
- §3.2 per-agent import (skills→agent skills dir, mcp→profile, commands→per-agent, `AddonRef` recorded) → Task 4. ✔ Uninstall (remove dirs + member MCP + AddonRef, no refcount) → Task 5 `cmd_addon_remove`. ✔
- §3.3 effective-enabled rule (all four consequences) → Task 1 truth-table test. ✔
- §4 enforcement: skills+triggers at `prepare_runtime` (single filter, group-aware) → Task 2; MCP via `enabled_mcp_servers`→`mcp_enabled` (no site edit) → Task 1. Agent-card exclusion is "low-pri/nice-to-have" (§4.3) → **deliberately skipped** (not a security boundary; flagged here). ✔
- §6 importer: SKILL.md→Context skill (see deviation note below), commands→Command skill, .mcp.json→pinned entry with env-notice, hooks ignored → Tasks 3+4. ✔
- §7 security: disabled-by-default at construction (Task 4), Sandboxed trust floor (no trust-store write → loader defaults Sandboxed), scan gate (`scan_or_block`), MCP sandbox reuse via `build_pinned_entry`/`resolve_command`/`compute_binary_sha256`, env not imported, audit action, kill-switch, path-traversal `safe_member_name` → Tasks 4+5. ✔
- §8 P2 Plugins tab + cascade switch + import + remove + `(plugin off)` badge + `effective_enabled` in `detail.rs` + `generate_handler!` registration → Tasks 6+7. ✔
- §9 P2 CLI (`import`/`list`/`enable`/`disable`/`remove`/`disable-all`) → Task 5. ✔
- §10 bundled skills as add-ons: native role-bundles may install `enabled=true` — the data model supports it (`AddonRef.enabled` settable); no importer change needed for the local-plugin first cut. Native-bundle install path is out of scope for this plan (Goal 2 = Claude plugins). Flagged. ✔
- §13 P2 tests: converter round-trip (Task 3), `enabled=false` assertion (Task 4), per-agent isolation (Task 4), MCP path-escape reject + sha pin (Task 4), env-not-written (Task 4). ✔

**Open-question resolutions (spec §14):** Local-dir-only import (id = `plugin.name`, source `claude-local:<name>@<version>`); marketplace-ref import deferred. No global library / no refcounting. env surfaced as notice, not imported.

**Deliberate spec deviation (flagged):** §6 says SKILL.md "body → `content.procedure`". Implemented as `content.context` (Category::Context) instead — a Claude SKILL.md is a freeform instruction blob, and `Content::mode()` requires category↔content agreement (`procedure` would make it Workflow mode and mismatch `category: Context`, failing `validate()`/scan and blocking every import). `content.context` is the faithful, validation-passing representation. If structured procedures are later wanted, that's a separate converter.

**Placeholder scan:** No TBD/"add error handling"/"similar to Task N". Each code step is complete. Two test-harness specifics (mur-home resolution helper, profile→detail mapper fn name) are explicitly called out as "grep the existing pattern and match it" rather than invented — these are real codebase conventions the implementer must read, not hand-waves.

**Type consistency:** `AddonRef`/`InstalledAddonView` field names (`id`/`source`/`enabled`/`skills`/`mcp`/`commands`) consistent across Rust + TS. `cmd_addon_set_enabled` (not `cmd_addon_toggle`) used consistently in Tasks 5/6. `addon_id` param name consistent CLI↔Tauri↔TS (camelCased to `addonId` at the Tauri boundary, noted). `skill_enabled`/`mcp_enabled`/`group_of`/`set_addon_enabled`/`disable_all_addons` signatures identical in Task 1 definition and Tasks 2/6 call sites.
