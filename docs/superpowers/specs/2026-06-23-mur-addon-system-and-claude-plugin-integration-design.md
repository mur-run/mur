# MUR Add-on System & Claude Plugin Integration — Design

- **Date:** 2026-06-23
- **Status:** Draft v2 (approved in brainstorming; revised after a 5-dimension adversarial review + a code-grounded filter-site/security trace)
- **Author:** David Chang (with Claude)
- **Related:** `mur-fleet-design`, `model-registry-and-secret-refs-design`, `mur-model-library-design`, `agent-export-host-data-model-design`

## 0. What the review changed (v1 → v2)

The first draft proposed per-entry `enabled` bits and a global `~/.mur/addons/`
library. A code-grounded review found that fatal and over-built. v2 corrects:

- **Enforcement was a no-op.** Skills load *globally* and inject by scope —
  `inject_layer2()` never consults an agent's `installed_skills`
  (`mur-agent-runtime/src/skills/injector.rs:23`). An `enabled` bit on
  `SkillCardEntry` would never be read. → **Per-agent denylist filtered at the one
  agent-context site** (`supervisor_runner.rs` `prepare_runtime`).
- **Global library leaked + needed refcounting.** Globally-installed imported
  skills auto-inject into *every* agent (scope=User is always visible). → **Imports
  install per-agent**; the global library is dropped. No refcounting.
- **env secret-detection was scope creep + a leak risk.** → **Don't import env
  values at all**; surface them as a notice to wire via `mur agent secret`.
- Added: import-time MCP command validation, an audit action, an emergency
  kill-switch, command-skill dispatch semantics, legacy-skill handling.

## 1. Problem & Goals

MUR attaches **skills** and **MCP servers** to agents, but the only way to "turn one
off" is to **delete** it — discarding `SkillStats` (lifecycle/trust/usage) and forcing an
MCP re-pin. There is no enable/disable. Separately, the Claude Code ecosystem ships a
large library of **plugins** (bundles of skills + slash-commands + hooks + MCP) that MUR
users cannot bring to their MUR agents.

**Goals**

1. **Non-destructive enable/disable**, per-agent, surfaced in the MUR Hub.
2. **Consume** Claude Code plugins: import their skills / MCP / slash-commands into a MUR
   agent as managed add-ons (Claude's marketplace is a *source*; MUR's skill stays native).
3. Resolve how an **agent's bundled skills** are handled — they become add-ons under the
   same mechanism.

**Non-goals (this design)**

- No parallel "plugin runtime" or MUR marketplace; reuse existing primitives.
- **No global shared add-on library** — imports are per-agent (see §0). A cross-agent
  shared library is a possible later phase, not now.
- **Hooks** (Claude `hooks.json`, shell-on-event) are **deferred to Phase 3**, gated
  behind opt-in OFF + sandbox + audit + fail-closed.
- No **export** of MUR as Claude plugins ("Produce" direction dropped).

## 1a. Glossary

- **Claude plugin** — the source artifact on disk (`plugin.json` + `skills/` +
  `commands/*.toml` + `hooks/hooks.json` + `.mcp.json`).
- **add-on** — umbrella for the three shapes MUR enables/disables: `skill`, `mcp`,
  `plugin-group`.
- **plugin-group** — a MUR `AddonRef`: a named bundle of skills + mcp + commands a single
  agent imported (from a Claude plugin, a `.muragent`/`.fleet`, or grouped natively).
- **bundle** — reserved for `.muragent` / `.fleet` *export artifacts* (existing term); not
  reused for add-ons.

## 2. Core Concept: the Add-on

An **add-on** is the smallest unit the Hub can enable/disable. Three shapes, all backed by
primitives that already exist:

| Add-on shape | Backed by | Source |
|---|---|---|
| `skill` | `SkillManifest` (already a superset of Claude `SKILL.md`) | native / Claude `SKILL.md` |
| `mcp` | `McpServerEntry` | native / Claude `.mcp.json` |
| `plugin-group` | an `AddonRef` (skills + mcp + commands, imported per-agent) | Claude plugin / `.muragent` / `.fleet` / native grouping |

A Claude plugin is **not** a new runtime object — it is an *import source* that expands
into the shapes above, recorded as one `AddonRef` for provenance, cascade-toggle, and
uninstall-as-a-unit.

## 3. Data Model

### 3.1 Per-agent enable state (denylist + addon allowlist)

All enable state lives on `AgentProfile` (`mur-common/src/agent.rs`). Nothing is added to
`McpServerEntry` or `SkillCardEntry` — a per-entry bit cannot be enforced (§0).

```rust
// AgentProfile additions:

/// Skill names installed/visible to this agent but suppressed.
/// Non-destructive: stats/trust/files are kept. Absent/empty => all
/// visible skills enabled (back-compat).
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub disabled_skills: Vec<String>,

/// McpServerEntry names suppressed for this agent (non-destructive).
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub disabled_mcp: Vec<String>,

/// Plugin-groups imported by this agent (P2). Each is self-contained.
#[serde(default, skip_serializing_if = "Vec::is_empty")]
pub addons: Vec<AddonRef>,
```

```rust
/// A plugin-group referenced by an agent. Self-contained — members are
/// installed PER-AGENT (skills -> ~/.mur/agents/<a>/skills/, mcp -> this
/// profile's mcp_servers). No global library, no refcounting.
pub struct AddonRef {
    pub id: String,        // "superpowers@claude-plugins-official"
    pub source: String,    // provenance, free-text:
                           //   "claude:claude-plugins-official/superpowers@6.0.3"
                           //   "muragent:<sha>" | "fleet:<name>" | "native"
    /// Fail-closed. Imports construct this `false`; the importer/installer
    /// sets it explicitly. A trusted native role-bundle installer MAY set
    /// `true`. No serde default-true magic — see §7.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)] pub skills: Vec<String>,   // member skill names
    #[serde(default)] pub mcp: Vec<String>,      // member mcp names
    #[serde(default)] pub commands: Vec<String>, // member command-skill names
}
```

Asymmetry, by design: **native standalone items default enabled** (denylist semantics —
absent name = on, back-compat); **plugin-groups default disabled** (allowlist semantics —
`AddonRef.enabled=false` until opted in). This is the fail-closed posture for external
code (§7).

### 3.2 Imports are per-agent (no global library)

`mur agent addon import` installs a plugin into **one** agent:

- skills → `~/.mur/agents/<agent>/skills/<name>/` (loaded only for that agent by
  `load_all(mur_home, agent_name)`; never injected into other agents)
- mcp → appended to this profile's `mcp_servers`
- commands → `Category::Command` skills installed per-agent like skills
- an `AddonRef{ id, source, enabled:false, members }` recorded in `profile.addons`

Uninstall (`mur agent addon remove <id>`) removes the per-agent skill dirs, the member MCP
entries, and the `AddonRef`. Because nothing is shared, there is no refcounting or GC.
Importing the same plugin into two agents imports twice (acceptable; skills are small).

## 3.3 Effective-enabled rule

Computed once at agent load (§4), for both skills and mcp:

```
group_of(name, p)   = p.addons.iter().find(|g| g.{skills|mcp|commands}.contains(name))

skill_enabled(s, p) = s.name ∉ p.disabled_skills
                      && group_of(s.name, p).map_or(true, |g| g.enabled)

mcp_enabled(m, p)   = m.name ∉ p.disabled_mcp
                      && group_of(m.name, p).map_or(true, |g| g.enabled)
```

Consequences (verified-intent, locked by tests §13):
- native standalone item, no entry anywhere → **enabled** (back-compat).
- name in `disabled_*` → **off** (overrides everything, incl. an enabled group → lets you
  silence one member of an enabled plugin).
- grouped item, group `enabled=false` → **off** (you cannot enable a single member of a
  disabled group — matches Claude's per-plugin boolean).
- grouped item, group `enabled=true`, name not denied → **on**.

## 4. Enforcement: where disabled items drop out

Skill loading is agent-aware (`load_all(mur_home, &profile.inner.name)`), but injection is
not gated by per-agent membership — so we filter **once, at the single site where the
profile and the loaded skills are both in scope**:

1. **Skills + command-triggers — `mur-agent-runtime/src/supervisor_runner.rs:495-496`
   (`prepare_runtime`).** After `load_all()`, drop any skill failing `skill_enabled`, then
   `RuntimeSkills::build(filtered)`. Because the trigger registry is built from the
   filtered list (`trigger_matcher::register_from`), this single filter covers Layer-2
   injection, Layer-3 trigger injection, **and** command-skill firing — no per-turn
   threading into `task_runner` needed.
2. **MCP — `supervisor_runner.rs:233`.** Pre-filter `profile.inner.mcp_servers` by
   `mcp_enabled` before `McpPool::new(...)`. Disabled servers are never spawned and never
   advertised in `tools/list`.
3. **(Consistency, low-pri) Agent Card.** When broadcasting `installed_skills`, exclude
   disabled ones so peers don't see a capability the agent won't use. Not a security
   boundary; nice-to-have.

Toggling is non-destructive: `SkillStats`, `event.jsonl`, trust entries, MCP pins are
untouched. **Apply model:** the filter runs at `prepare_runtime` (startup), so a toggle
takes effect on the **next agent restart** — consistent with how entitlement changes apply
today. The Hub triggers a supervisor restart after a toggle (same pattern it already uses
for `model_ref`).

## 5. Enable/disable semantics

- **Non-destructive.** `disable` keeps the item installed; only `remove`/`uninstall` is
  destructive.
- **Defaults.** Native items already on an agent → **enabled** (denylist empty). Imported
  plugin-groups → **disabled** (`AddonRef.enabled=false`), set at import (§7).
- **Scope.** Per-agent.
- **Re-export.** `.muragent` / `.fleet` carry `addons` + denylists so a shared agent
  arrives configured, **but every imported (external-sourced) `AddonRef` re-lands
  `enabled=false` on the receiver regardless of nesting** — fail-closed even inside a
  trusted bundle. Native role-bundle groups follow the existing fleet/agent import trust
  ladder for their default.

## 6. Claude plugin importer (Phase 2)

`mur agent addon import <name> <plugin-dir>` reads `plugin.json`, then:

- **`skills/*/SKILL.md` → `SkillManifest`** (installed under the agent): `name`/
  `description` map directly; body → `content.procedure`, description → `content.abstract`;
  `category = Context`; `provenance = Hybrid`; `scope = User`; `trust_level = Sandboxed`;
  `publisher`/`tags` from `plugin.json`. Triggers: `SKILL.md` declares none → derive
  `Keyword` triggers from the name + a `Manual` trigger. **No `SessionStart`** auto-inject
  for imported skills.
- **`commands/*.toml` → `SkillManifest`**: `category = Command`; `content.command =
  prompt` (TOML `prompt`, `{{args}}` preserved); `triggers = [Command(<name>)]`. **Runtime
  semantics:** a command-skill is *instruction injection* — `match_prompt` matches
  `prompt.starts_with(<cmd>)` and the skill's `content` is injected as Layer-3 context. No
  tool execution, no dispatcher. (Argument-bearing `/cmd <args>` execution is Phase 3+.)
- **`.mcp.json` → `McpServerEntry`**: `name`, `command`, `args` only. **env is NOT
  imported** — if a server declares `env`, the importer prints a notice listing the
  variables and the `mur agent secret set` command to wire them. (No env field, no
  secret-detection heuristic — avoids both the leak and the scope creep.)
- **`hooks/hooks.json`** — ignored (Phase 3).

**Import-time validation (security, §7):** every imported skill runs through
`mur skill validate` (blocks on findings unless `--force`). For each imported MCP: require
`command` to be an absolute path or a `$PATH`/`~/.mur/bin` name; canonicalize and reject
paths escaping the allowed tree; verify the binary exists and is executable; **compute and
pin `binary_sha256` at import**; advisory-warn (non-blocking) if `args` contain shell-ish
tokens (`-c`, `eval`). All imported members install **disabled** (`AddonRef.enabled=false`).

## 7. Security / safety lens

Applying the autonomy-safety rule (opt-in OFF + sandbox + audit + fail-closed). The review
**confirmed against code** that imported MCP inherits the same protections as native MCP
and that the existing scan/secret paths hold:

- **Disabled by default.** `AddonRef.enabled=false` is set by the importer at construction
  — the single choke point; the Hub/CLI cannot create an enabled imported group. (`§3.1`)
- **Trust floor.** Imported skills land `TrustLevel::Sandboxed`.
- **Scan gates import.** `cmd_validate` → `scan_skill` → `has_blocking_findings()` →
  `bail!` (`mur-core/src/cmd/skill_cmd.rs:30-46`). Confirmed.
- **MCP sandbox reuse — confirmed.** Spawn goes through
  `sandbox::child::spawn_sandboxed(cmd, policy)` (`protocol/mcp_client.rs:56-63`) with the
  agent's `Entitlements`-derived `SandboxPolicy`; `Command::args` (no shell) prevents shell
  injection. `binary_sha256` pinning + spawn allowlist apply identically to native MCP.
- **Secrets — confirmed.** Resolve via `SecretRef`→OS keychain (`mur-common/src/secret.rs`).
  Nothing secret enters `profile.yaml`; env is not imported at all (§6).
- **Audit.** Add `AuditAction::AddonToggle { agent, target, enabled }` to the hash-chained
  log (`mur-core/src/conversations/audit.rs`); CLI and Hub write it on every toggle.
- **Emergency kill-switch.** `mur agent addon disable-all <agent>` clears every
  `AddonRef.enabled` and appends all addon members to the denylists in one atomic
  `profile.yaml` write (temp+rename), then restarts the supervisor. Last-resort: edit
  `profile.yaml` directly.
- **Path traversal.** Importer canonicalizes plugin paths and rejects member install paths
  escaping `~/.mur/agents/<a>/skills/` (§6).

## 8. Hub UI

P1 reuses existing tabs; P2 adds one tab. No global Library modal (no global library).

- **P1 — toggles on existing tabs.** The **Skills** tab and **MCP** tab each render an
  enable/disable switch per row (beside the existing remove button). Tauri commands
  `agent_skill_toggle(name, skill, enabled)` / `agent_mcp_toggle(name, server, enabled)`
  shell out to the CLI (the `mcp_skills.rs` pattern) and return refreshed `AgentDetail`.
  Legacy `AgentProfile.skills` paths render as **"legacy (always on)"** with no switch.
- **P2 — "Plugins" tab.** Lists imported `AddonRef`s with one **cascade switch** each +
  an **Import** button (file/dir picker) + remove. When a group is off, its member rows in
  the Skills/MCP tabs show a greyed **"(plugin off)"** badge; the switch's source of truth
  is `effective_enabled` computed in `detail.rs`. Tauri: `agent_addon_import`,
  `agent_addon_list`, `agent_addon_toggle`, `agent_addon_remove`.
- Register all commands in `lib.rs` `generate_handler!`; extend `AgentDetail`/`DetailPatch`
  (`detail.rs`) with the enable state. Optional args use `Option<T>` (Tauri undefined-drop
  gotcha). After a toggle, the Hub restarts the agent (existing `model_ref` pattern).

## 9. CLI surface

All per-agent, matching the existing `mur agent <subsystem> <action>` convention
(parallels the existing `mur skill` ↔ `mur agent skill` split):

- `mur agent skill enable|disable <name>` — edits `disabled_skills`
- `mur agent mcp enable|disable <server_id>` — edits `disabled_mcp`
- `mur agent addon import <id> <plugin-dir> [--force]` (P2)
- `mur agent addon list` · `mur agent addon enable|disable <id>` ·
  `mur agent addon remove <id>` (P2)
- `mur agent addon disable-all` — kill-switch (§7)

Existing `mur agent skill add/remove`, `mur agent mcp add/remove/...` are unchanged.

## 10. Ask C — bundled skills resolved

An agent's bundled skills **are** add-ons:

- Individually-shipped skills → `skill` add-ons with per-agent toggles (Skills tab).
- A role-group (rustsmith's 5 skills, or a `.muragent`/`.fleet`) → an `AddonRef` with
  `source = "muragent:…"|"fleet:…"|"native"` and one cascade toggle — the **same
  mechanism** as a Claude plugin import. A *native* role-bundle's group may install
  `enabled=true` (trusted), while an external import installs `enabled=false`. The three
  asks converge on one model.

## 11. Phasing

- **P1 — enable/disable core (ships independently).** §3.1 denylist fields, §3.3 rule, §4
  filter sites (skills+triggers+mcp), §9 enable/disable CLI, §8 toggles on existing tabs,
  §7 audit action + kill-switch. Operates on native skills/MCP. Delivers Goals 1 & 3 with
  zero external-trust surface.
- **P2 — Claude plugin importer.** §3.2 per-agent import, `AddonRef`, §6 importer + import
  validation, §8 Plugins tab. Delivers Goal 2. First cut: local plugin-dir only;
  marketplace-ref import is a later sub-step.
- **P3 — hooks (deferred, gated).** Sandboxed `hooks.json` runner; opt-in OFF, audited,
  fail-closed. Own design doc.

## 12. Back-compat & migration

- All new fields are `#[serde(default ...)]`; existing `profile.yaml` loads unchanged —
  empty denylists + no addons ⇒ everything enabled. **No schema bump, no migration.**
- P2 creates per-agent imported skills under existing `~/.mur/agents/<a>/skills/`; no new
  top-level dirs.
- Legacy `AgentProfile.skills` paths stay always-on and are excluded from the Add-ons UI.

## 13. Testing

- **P1 (`assert`-based, no framework):**
  - `skill_enabled`/`mcp_enabled` truth table: standalone (denylist on/off) × grouped
    (group on/off) × member-denied — all four+ combinations.
  - Runtime: a denied skill is absent from `inject_layer2` output **and** from the trigger
    registry; a denied MCP server is never spawned / not in `tools/list`.
  - Kill-switch: `disable-all` clears every `AddonRef.enabled` and denies all members.
- **P2:**
  - Converters round-trip on the two installed sample plugins (SKILL.md, commands/*.toml,
    .mcp.json).
  - Import lands `AddonRef.enabled=false` (fail-closed assertion).
  - **Per-agent isolation:** a skill imported into agent A does **not** inject for agent B.
  - Import rejects an MCP `command` that path-escapes; pins `binary_sha256`.
  - env declared in `.mcp.json` is **not** written to `profile.yaml` (notice only).

## 14. Open questions (resolve in plans)

- Marketplace-ref import (`<name>@<marketplace>`) vs local-dir-only for the P2 first cut.
- Whether a future cross-agent shared add-on library is worth the refcounting it
  reintroduces (deferred; per-agent imports are the v1 stance).
- Adding an `env` field to `McpServerEntry` later (typed `{ key, value|secret_ref }`) if
  imported servers commonly need non-secret env — deferred; v1 surfaces env as a notice.
