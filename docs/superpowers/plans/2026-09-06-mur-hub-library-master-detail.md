# Plan — MUR Hub 2.0 Phase 2(a): Library pages on the master–detail shell

> **Execute with `mur-executing-plans`.** Two PRs, in order. Each is its own
> branch cut from a **fresh `main` after the previous PR merged** (squash
> merges break stacks). Work in a worktree under
> `/Volumes/Firecuda4tb/Projects/mur/.worktrees/hub-2-pr<N>`.

Design: `docs/superpowers/specs/2026-09-06-mur-hub-library-master-detail-design.md`.
Phase 1 (the shell, `SourceList`, `DetailPage`, menus, tokens) is on `main` as #1165–#1169; its plan `docs/superpowers/plans/2026-09-06-mur-hub-master-detail-shell.md` records the conventions this plan reuses.

## Goal

Make Skills, Workflows, MCP and Plugins master–detail pages like Agents and Fleets, with a detail that shows which agents use an item and lets you enable / remove it per agent, and retire the Library inspector.

## Architecture

One generic `LibraryPage<T>` owns the master–detail layout, loading, selection persistence, filter/facets and per-agent actions; each of the four pages is a thin configuration of it (row / facet / item builders from `libraryModel.ts`, its install menu and modals). The detail pane is the shared `LibraryDetail`. One Tauri command changes: `skills_installed` gains `agents` and `path`.

## Tech stack

Same as Phase 1: React 18 + TypeScript 5.5, Vite 5, Vitest 4 without jsdom (pure functions + `renderToStaticMarkup`), plain CSS on semantic tokens, Tauri 2. Rust 2024 in `mur-hub-gui/src-tauri` (workspace-excluded crate; its unit tests run from that directory).

## Global Constraints

Copied from the design and `CLAUDE.md`. Every task includes all of them.

1. Brand name is uppercase **MUR** in every user-visible string.
2. Single source file ≤ 800 lines.
3. Every new user-visible string lands in both `src/i18n/en.ts` and `src/i18n/zh-TW.ts` in the same commit (`tsc` enforces the table).
4. Components reference only semantic tokens; no raw hex in component CSS or TSX.
5. No hardcoded numbers or storage keys in TSX: named constants.
6. Never pair `Foo.tsx` with `foo.ts` in one directory (APFS is case-insensitive; Vite and `tsc` resolve the wrong file).
7. Tests never touch the DOM: pure functions, or `renderToStaticMarkup` for markup (`useT` needs a provider, so markup tests cover only components without it).
8. Every commit is gated on the real exit code: `set -o pipefail; npm test 2>&1 | grep …` — never on grep's.
9. Rust fail-open conventions of the Hub crate: an unreadable profile is skipped with `tracing::warn!`, never an error to the UI.
10. Every PR leaves the app usable: `npm run build`, `npm test`, `npm run lint` green, the Hub crate compiles, and that PR's manual acceptance list passes.

## Working agreement

- Paths are relative to `mur-hub-gui/ui/` unless they start with `mur-hub-gui/src-tauri/` or `docs/`.
- Line numbers cite `main` at `cd15fa28` (2026-09-06); re-check with `grep -n` before cutting.
- UI commands from `mur-hub-gui/ui/`: `npm test -- <path>`, `npm test`, `npm run build`, `npm run lint`.
- Rust commands from `mur-hub-gui/src-tauri/`: `ORT_STRATEGY=download cargo test skills_installed`. The Hub target is large (~24 GB); if the project drive cannot take it, push and rely on the **Hub GUI crate** CI job, and say so in the PR.
- Browser acceptance: `npm run dev` on 5174, inject the Tauri stub the Phase 1 plan describes (`window.__TAURI_INTERNALS__` with `metadata.currentWindow`, an `invoke` stub, `plugin:event|listen → 1`, `plugin:dialog|message → "Ok"`), inject twice (Vite's dep-optimize reload wipes the first), click the error boundary's **Try again**.
- Commit after every task with the message given.

## File structure

| PR | File | Responsibility |
|---|---|---|
| 6 | `mur-hub-gui/src-tauri/src/skills_installed.rs` (modify) | `agents_by_skill`, `agents` + `path` on `InstalledSkillView`, profile scan |
| 6 | `src/components/shell/sourceListModel.ts` (modify) | `status` optional |
| 6 | `src/components/shell/SourceList.tsx` (+ test) (modify) | `toolbar` slot, optional dot, `createItems` menu |
| 6 | `src/components/shell/DetailPage.tsx` (+ test) (modify) | no tablist for a single tab; `status` optional |
| 6 | `src/components/detail/library/libraryModel.ts` (+ `.test.ts`) (new) | backend shapes, `LibraryItem`, `LibraryAgentUse`, row / facet / item builders |
| 6 | `src/components/detail/library/useInstallTarget.ts` (+ `.test.ts`) (new) | persisted install-target agent |
| 6 | `src/components/detail/library/LibraryDetail.tsx` (new) | the shared detail pane |
| 6 | `src/components/detail/library/LibraryPage.tsx` (new) | the generic master–detail page the four pages configure |
| 6 | `src/components/library/LibraryGlyph.tsx` (new) | the four kind glyphs (28 / 48 px) |
| 6 | `src/components/library/SkillsPage.tsx` (rewrite) | Skills configuration |
| 6 | `src/styles/components/library.css` (new) | `.library-*` rules |
| 7 | `src/components/library/{McpPage,PluginsPage,WorkflowsPage}.tsx` (rewrite) | the other three configurations |
| 7 | `src/components/shell/Inspector.tsx`, `src/components/DashboardApp.tsx` (modify) | no library inspector, no `libItem` |
| 7 | delete `src/components/inspector/LibraryInspector.tsx`; `detail-panel.css` cleanup | — |
| all | `src/i18n/en.ts`, `src/i18n/zh-TW.ts` | every new string |

---

## PR 6 — Rust usage data, shell extensions, `LibraryPage`, Skills

Branch `feat/hub-2-library-skills`.

### Task 6.1 — `skills_installed` gains `agents` and `path`

- [x] In `mur-hub-gui/src-tauri/src/skills_installed.rs`, add to the `tests` module:

```rust
    #[test]
    fn agents_by_skill_folds_agents_per_skill_sorted() {
        use mur_common::agent::SkillCardEntry;
        let card = |name: &str| SkillCardEntry { name: name.to_string(), ..Default::default() };
        let profiles = vec![
            ("scout".to_string(), vec![card("mur-dev"), card("mur-tdd")]),
            ("aura".to_string(), vec![card("mur-dev")]),
            ("muse".to_string(), vec![]),
        ];
        let map = agents_by_skill(&profiles);
        assert_eq!(map.get("mur-dev").unwrap(), &vec!["aura".to_string(), "scout".to_string()]);
        assert_eq!(map.get("mur-tdd").unwrap(), &vec!["scout".to_string()]);
        assert!(!map.contains_key("nope"));
    }
```
  If `SkillCardEntry` does not derive `Default` (check `mur-common/src/agent.rs` line 15), build the entries with every field spelled out instead of `..Default::default()` — do not add a derive to `mur-common` for a test.
- [x] Run `ORT_STRATEGY=download cargo test skills_installed` from `mur-hub-gui/src-tauri/` → the new test fails to compile (`agents_by_skill` missing). If the crate cannot build locally (disk), skip to the code and let CI run it; note this in the PR.
- [x] Add the fields and the fold:

```rust
/// One installed skill as shown in the Skills library list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct InstalledSkillView {
    pub name: String,
    pub description: String,
    pub category: String,
    pub origin_version: Option<String>,
    pub status: String,
    /// Agents whose profile lists this skill, sorted by name.
    pub agents: Vec<String>,
    /// The global skill directory (for "Open folder"); `None` only if the
    /// path is not valid UTF-8.
    pub path: Option<String>,
}

/// Pure fold: (agent name, its `profile.installed_skills`) pairs → skill
/// name → sorted agent names. Kept free of I/O so it is unit-testable.
pub fn agents_by_skill(
    profiles: &[(String, Vec<mur_common::agent::SkillCardEntry>)],
) -> std::collections::BTreeMap<String, Vec<String>> {
    let mut map: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for (agent, cards) in profiles {
        for card in cards {
            map.entry(card.name.clone()).or_default().push(agent.clone());
        }
    }
    for agents in map.values_mut() {
        agents.sort();
        agents.dedup();
    }
    map
}

/// Read every `agents/*/profile.yaml` the way `mcp_installed` does; an
/// unreadable or unparsable profile is skipped with a warning.
fn read_agent_skill_cards(agents_dir: &Path) -> Vec<(String, Vec<mur_common::agent::SkillCardEntry>)> {
    let entries = match std::fs::read_dir(agents_dir) {
        Ok(entries) => entries,
        Err(e) => {
            tracing::warn!(dir = %agents_dir.display(), error = %e, "skills_installed: cannot read agents dir");
            return vec![];
        }
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let Some(agent_name) = dir.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        match std::fs::read(dir.join("profile.yaml")) {
            Ok(bytes) => match serde_yaml_ng::from_slice::<mur_common::AgentProfile>(&bytes) {
                Ok(profile) => out.push((agent_name.to_string(), profile.installed_skills.clone())),
                Err(e) => tracing::warn!(agent = %agent_name, error = %e, "skills_installed: skipping unparsable profile"),
            },
            Err(e) => tracing::warn!(agent = %agent_name, error = %e, "skills_installed: skipping unreadable profile"),
        }
    }
    out
}
```
  In `skills_installed()`, before `Ok(list_skills(...))`, add `let usage = agents_by_skill(&read_agent_skill_cards(&mur_home.join("agents")));` and pass `&usage` as a third argument. In `list_skills(skills_dir, status_by_name, usage: &std::collections::BTreeMap<String, Vec<String>>)` set `agents: usage.get(&manifest.name).cloned().unwrap_or_default()` and `path: dir.to_str().map(str::to_string)`. Update the two existing `list_skills` tests to pass `&std::collections::BTreeMap::new()` and assert `result[0].path.is_some()` in the manifest test.
- [x] `ORT_STRATEGY=download cargo test skills_installed` → pass (or CI). `cargo clippy -- -D warnings` on the crate if it builds locally.
- [x] Commit: `feat(hub): skills_installed reports which agents use each skill and its path`

> **Deviation recorded during execution (PR 6):** the Rust change was not
> compiled locally (drive full); CI's Hub GUI clippy caught one lint in the
> test (`unnecessary_get_then_check`, fixed above). Everything else compiled
> and the unit tests passed on all three platforms.

**Interfaces — Produces:** `InstalledSkillView.agents: string[]`, `InstalledSkillView.path: string | null` (as seen by the UI).

### Task 6.2 — `SourceList`: toolbar, optional status, "+" menu

- [x] Append to `src/components/shell/SourceList.test.tsx`:

```tsx
import type { MenuItemDef } from "./SplitButton";

describe("SourceList library extensions", () => {
  const base = {
    title: "Skills", count: 1, facets: [], allLabel: "All", activeFacet: null, onFacet: noop,
    filter: "", onFilter: noop, filterPlaceholder: "Filter", selectedId: null, onSelect: noop,
    createLabel: "Add", emptyState: <p>none</p>,
  };
  const row = { id: "s1", name: "mur-dev", subtitle: "workflow", avatar: "S", facets: ["workflow"] };
  it("renders the toolbar slot and no status dot for a row without status", () => {
    const html = renderToStaticMarkup(<SourceList {...base} rows={[row]} toolbar={<span id="tb">picker</span>} />);
    expect(html).toContain('id="tb"');
    expect(html).not.toContain("status-dot");
  });
  it("renders a menu trigger instead of a plain + when createItems is given, and nothing when neither", () => {
    const items: MenuItemDef[] = [{ id: "url", label: "From URL", onSelect: noop }];
    const withMenu = renderToStaticMarkup(<SourceList {...base} rows={[row]} createItems={items} />);
    expect(withMenu).toContain('aria-haspopup="menu"');
    const plain = renderToStaticMarkup(<SourceList {...base} rows={[row]} />);
    expect(plain).not.toContain("source-list__create");
  });
});
```
  (`noop` and `renderToStaticMarkup` are already imported at the top of the file.)
- [x] Run `npm test -- src/components/shell/SourceList.test.tsx` → the new cases fail (`toolbar` / `createItems` unknown, dot rendered).
- [x] `src/components/shell/sourceListModel.ts`: `status?: StatusKind;` with the doc comment `/** Omitted for items that have no runtime (Library rows). */`.
- [x] `src/components/shell/SourceList.tsx`:
  - props: `onCreate?: () => void; createItems?: MenuItemDef[]; toolbar?: ReactNode;` (import `type MenuItemDef`, `MenuList` from `./SplitButton` and `useMenu` from `./useMenu`).
  - header: replace the "+" button with

```tsx
        {p.createItems && p.createItems.length > 0 ? (
          <div className="split source-list__create-menu" ref={menu.rootRef}>
            <button type="button" className="source-list__create" aria-haspopup="menu" aria-expanded={menu.open} title={p.createLabel} aria-label={p.createLabel} onClick={() => menu.setOpen(!menu.open)}>
              +
            </button>
            {menu.open && <MenuList items={p.createItems} onPick={() => menu.setOpen(false)} />}
          </div>
        ) : p.onCreate ? (
          <button type="button" className="source-list__create" onClick={p.onCreate} title={p.createLabel} aria-label={p.createLabel}>
            +
          </button>
        ) : null}
```
    with `const menu = useMenu();` at the top of the component (hooks run unconditionally).
  - after the header: `{p.toolbar && <div className="source-list__toolbar">{p.toolbar}</div>}`.
  - row status: `{r.status && <StatusDot kind={r.status} />}`.
- [x] `src/styles/components/source-list.css`: `.source-list__toolbar { padding: 0 var(--space-5) var(--space-4); } .source-list__create-menu { margin-left: auto; } .source-list__create-menu .menu { right: 0; left: auto; }`.
- [x] Test → pass. `npm run build`. Commit: `feat(hub): SourceList toolbar slot, optional status dot, "+" as a menu`

**Interfaces — Produces:** `SourceListProps.toolbar?`, `createItems?`, `onCreate?`; `SourceRowData.status?`.

### Task 6.3 — `DetailPage` with a single tab and no status

- [x] Append to `src/components/shell/DetailPage.test.ts`:

```ts
import { hasTabBar } from "./DetailPage";

describe("hasTabBar", () => {
  it("only with two or more tabs", () => {
    expect(hasTabBar([{ id: "a", label: "A" }])).toBe(false);
    expect(hasTabBar([{ id: "a", label: "A" }, { id: "b", label: "B" }])).toBe(true);
  });
});
```
- [x] `src/components/shell/DetailPage.tsx`: `export function hasTabBar<T extends string>(tabs: DetailTabDef<T>[]): boolean { return tabs.length > 1; }`; wrap the tablist in `{hasTabBar(p.tabs) && (…)}`; make `status?: StatusKind` optional and render `{p.status && <StatusPill kind={p.status} />}`; give the body `className={\`detail-page__body${hasTabBar(p.tabs) ? "" : " detail-page__body--flush"}\`}`. Add `.detail-page__body--flush { padding-top: var(--space-6); border-top: 1px solid var(--border-line); margin-top: var(--space-6); }` to `detail-page.css`.
- [x] Test → pass. Commit: `feat(hub): DetailPage draws no tab bar for a single tab; status pill optional`

### Task 6.4 — `libraryModel.ts`, `LibraryGlyph`, `useInstallTarget`, `LibraryDetail`, `LibraryPage`

- [x] Write `src/components/detail/library/libraryModel.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { itemFor, mcpFacets, mcpRows, pluginRows, skillFacets, skillRows, workflowRows } from "./libraryModel";

const skills = [
  { name: "mur-dev", description: "dev", category: "workflow", origin_version: "1.2.0", status: "update available", agents: ["aura"], path: "/x/mur-dev" },
  { name: "mur-tdd", description: "tdd", category: "workflow", origin_version: null, status: "—", agents: [], path: null },
];
const mcps = [{ id: "fs", name: "filesystem", description: "files", transport: "stdio", agents: ["aura", "scout"] }];
const plugins = [{ id: "ghp", source: "/p/ghp", skill_count: 2, mcp_count: 1, command_count: 3, agents: [{ agent: "aura", enabled: true }] }];
const workflows = [{ name: "release", description: "cut a release", path: "/home/d/.mur/workflows/release.yaml" }];
const noAvatar = () => null;

describe("rows", () => {
  it("skills: subtitle carries category, version and status; rows have no status dot", () => {
    const r = skillRows(skills, "v", noAvatar);
    expect(r[0].subtitle).toBe("workflow · v1.2.0 · update available");
    expect(r[1].subtitle).toBe("workflow");
    expect(r[0].status).toBeUndefined();
    expect(r[0].facets).toEqual(["workflow"]);
  });
  it("mcp: subtitle is transport and usage count", () => {
    expect(mcpRows(mcps, (n) => `used by ${n}`, noAvatar)[0].subtitle).toBe("stdio · used by 2");
  });
  it("plugins and workflows", () => {
    expect(pluginRows(plugins, { skills: "skills", mcp: "MCP", commands: "commands" }, noAvatar)[0].subtitle).toBe("2 skills · 1 MCP · 3 commands");
    expect(workflowRows(workflows, noAvatar)[0].subtitle).toBe("release.yaml");
  });
});

describe("facets", () => {
  it("count per distinct value, sorted", () => {
    expect(skillFacets(skills)).toEqual([{ id: "workflow", label: "workflow", count: 2 }]);
    expect(mcpFacets(mcps)).toEqual([{ id: "stdio", label: "stdio", count: 1 }]);
  });
});

describe("itemFor", () => {
  it("maps a skill to an item with meta rows and path", () => {
    const it0 = itemFor("skill", skills[0], { category: "Category", version: "Version", status: "Status", path: "Path" });
    expect(it0.meta.map((m) => m.value)).toEqual(["workflow", "1.2.0", "update available", "/x/mur-dev"]);
    expect(it0.path).toBe("/x/mur-dev");
  });
});
```

- [x] Run it → fails (module missing).
- [x] Create `src/components/detail/library/libraryModel.ts`:

```ts
import type { ReactNode } from "react";
import type { SourceFacet, SourceRowData } from "../../shell/sourceListModel";

// ── Backend shapes (mirror src-tauri) ────────────────────────────────────────
export interface InstalledSkillView { name: string; description: string; category: string; origin_version: string | null; status: string; agents: string[]; path: string | null }
export interface InstalledMcpView { id: string; name: string; description: string; transport: string; agents: string[] }
export interface AddonAgentState { agent: string; enabled: boolean }
export interface InstalledAddonAgg { id: string; source: string; skill_count: number; mcp_count: number; command_count: number; agents: AddonAgentState[] }
export interface WorkflowView { name: string; description: string; path: string }

export type LibraryKind = "skill" | "mcp" | "plugin" | "workflow";

export interface LibraryItem {
  id: string;
  kind: LibraryKind;
  name: string;
  description?: string;
  meta: { label: string; value: string; mono?: boolean }[];
  path?: string | null;
}

/** One agent that uses the item. `enabled` undefined = no toggle offered. */
export interface LibraryAgentUse { agent: string; enabled?: boolean }

const SEP = " · ";
/** The backend's "not in the registry" status; not worth a word in the subtitle. */
const STATUS_NONE = "—";

function facetsOf(values: string[]): SourceFacet[] {
  const counts: Record<string, number> = {};
  for (const v of values) counts[v] = (counts[v] ?? 0) + 1;
  return Object.keys(counts).sort((a, b) => a.localeCompare(b)).map((v) => ({ id: v, label: v, count: counts[v] }));
}

type Avatar<T> = (r: T) => ReactNode;

export function skillRows(skills: InstalledSkillView[], versionPrefix: string, avatar: Avatar<InstalledSkillView>): SourceRowData[] {
  return skills.map((s) => ({
    id: s.name,
    name: s.name,
    subtitle: [s.category, s.origin_version ? `${versionPrefix}${s.origin_version}` : null, s.status !== STATUS_NONE ? s.status : null].filter(Boolean).join(SEP),
    avatar: avatar(s),
    facets: [s.category],
  }));
}
export const skillFacets = (skills: InstalledSkillView[]): SourceFacet[] => facetsOf(skills.map((s) => s.category));

export function mcpRows(servers: InstalledMcpView[], usedBy: (n: number) => string, avatar: Avatar<InstalledMcpView>): SourceRowData[] {
  return servers.map((s) => ({
    id: s.id,
    name: s.name,
    subtitle: [s.transport, usedBy(s.agents.length)].join(SEP),
    avatar: avatar(s),
    facets: [s.transport],
  }));
}
export const mcpFacets = (servers: InstalledMcpView[]): SourceFacet[] => facetsOf(servers.map((s) => s.transport));

export function pluginRows(addons: InstalledAddonAgg[], labels: { skills: string; mcp: string; commands: string }, avatar: Avatar<InstalledAddonAgg>): SourceRowData[] {
  return addons.map((a) => ({
    id: a.id,
    name: a.id,
    subtitle: [`${a.skill_count} ${labels.skills}`, `${a.mcp_count} ${labels.mcp}`, `${a.command_count} ${labels.commands}`].join(SEP),
    avatar: avatar(a),
    facets: [],
  }));
}

export function workflowRows(workflows: WorkflowView[], avatar: Avatar<WorkflowView>): SourceRowData[] {
  return workflows.map((w) => ({
    id: w.path,
    name: w.name,
    subtitle: w.path.split("/").pop() ?? w.path,
    avatar: avatar(w),
    facets: [],
  }));
}

type MetaLabels = Record<string, string>;

/** The detail's view of one record: meta rows in display order, path when known. */
export function itemFor(kind: "skill", r: InstalledSkillView, l: MetaLabels): LibraryItem;
export function itemFor(kind: "mcp", r: InstalledMcpView, l: MetaLabels): LibraryItem;
export function itemFor(kind: "plugin", r: InstalledAddonAgg, l: MetaLabels): LibraryItem;
export function itemFor(kind: "workflow", r: WorkflowView, l: MetaLabels): LibraryItem;
export function itemFor(kind: LibraryKind, r: InstalledSkillView | InstalledMcpView | InstalledAddonAgg | WorkflowView, l: MetaLabels): LibraryItem {
  switch (kind) {
    case "skill": {
      const s = r as InstalledSkillView;
      return {
        id: s.name, kind, name: s.name, description: s.description, path: s.path,
        meta: [
          { label: l.category, value: s.category },
          { label: l.version, value: s.origin_version ?? STATUS_NONE },
          { label: l.status, value: s.status },
          { label: l.path, value: s.path ?? STATUS_NONE, mono: true },
        ],
      };
    }
    case "mcp": {
      const s = r as InstalledMcpView;
      return { id: s.id, kind, name: s.name, description: s.description, meta: [{ label: l.transport, value: s.transport }, { label: l.serverId, value: s.id, mono: true }] };
    }
    case "plugin": {
      const a = r as InstalledAddonAgg;
      return {
        id: a.id, kind, name: a.id, path: a.source,
        meta: [
          { label: l.source, value: a.source, mono: true },
          { label: l.skills, value: String(a.skill_count) },
          { label: l.mcp, value: String(a.mcp_count) },
          { label: l.commands, value: String(a.command_count) },
        ],
      };
    }
    case "workflow": {
      const w = r as WorkflowView;
      return { id: w.path, kind, name: w.name, description: w.description, path: w.path, meta: [{ label: l.path, value: w.path, mono: true }] };
    }
  }
}
```

- [x] Create `src/components/library/LibraryGlyph.tsx` — the sidebar's four glyphs in a neutral tile:

```tsx
import type { ReactNode } from "react";
import type { LibraryKind } from "../detail/library/libraryModel";
import { Ico } from "../agents/GridCard";

// Copied from Sidebar.tsx GLYPHS (lines 47–65 at cd15fa28) so the list and
// the nav draw the same icon; if the sidebar's icons change, change these.
const GLYPH: Record<LibraryKind, ReactNode> = {
  skill: <path d="M12 2 2 7l10 5 10-5Zm0 15L2 12v5l10 5 10-5v-5Z" />,
  workflow: (
    <>
      <rect x="3" y="3" width="6" height="6" rx="1" />
      <rect x="15" y="15" width="6" height="6" rx="1" />
      <path d="M9 6h6a3 3 0 0 1 3 3v6" />
    </>
  ),
  mcp: (
    <>
      <rect x="4" y="4" width="16" height="16" rx="2" />
      <path d="M9 9h6v6H9z" />
    </>
  ),
  plugin: (
    <path d="M12.2 2h-.4a2 2 0 0 0-2 2v.2a2 2 0 0 1-1 1.7l-.4.3a2 2 0 0 1-2 0l-.2-.1a2 2 0 0 0-2.7.7l-.2.4a2 2 0 0 0 .7 2.7l.2.1a2 2 0 0 1 1 1.7v.5a2 2 0 0 1-1 1.7l-.2.1a2 2 0 0 0-.7 2.7l.2.4a2 2 0 0 0 2.7.7l.2-.1a2 2 0 0 1 2 0l.4.3a2 2 0 0 1 1 1.7V20a2 2 0 0 0 2 2h.4a2 2 0 0 0 2-2v-.2a2 2 0 0 1 1-1.7l.4-.3a2 2 0 0 1 2 0l.2.1a2 2 0 0 0 2.7-.7l.2-.4a2 2 0 0 0-.7-2.7l-.2-.1a2 2 0 0 1-1-1.7v-.5a2 2 0 0 1 1-1.7l.2-.1a2 2 0 0 0 .7-2.7l-.2-.4a2 2 0 0 0-2.7-.7l-.2.1a2 2 0 0 1-2 0l-.4-.3a2 2 0 0 1-1-1.7V4a2 2 0 0 0-2-2Z" />
  ),
};

export function LibraryGlyph({ kind, large }: { kind: LibraryKind; large?: boolean }) {
  return (
    <span className={`library-glyph${large ? " library-glyph--lg" : ""}`} aria-hidden="true">
      <Ico>{GLYPH[kind]}</Ico>
    </span>
  );
}
```
  Before committing, diff the four values against `Sidebar.tsx` `GLYPHS` (`skills`, `workflows`, `mcp`, `plugins`) — they must be byte-identical.

- [x] Write `src/components/detail/library/useInstallTarget.test.ts` and the hook:

```ts
// test
import { describe, expect, it } from "vitest";
import { resolveInstallTarget } from "./useInstallTarget";
describe("resolveInstallTarget", () => {
  it("keeps a stored agent that still exists, else the first agent, else empty", () => {
    const agents = [{ name: "aura" }, { name: "scout" }];
    expect(resolveInstallTarget("scout", agents)).toBe("scout");
    expect(resolveInstallTarget("ghost", agents)).toBe("aura");
    expect(resolveInstallTarget(null, [])).toBe("");
  });
});
```
```ts
// src/components/detail/library/useInstallTarget.ts
import { useEffect, useState } from "react";
import { readKey, writeKey } from "../../shell/persist";

export const INSTALL_TARGET_KEY = "mur.library.installTarget";

export function resolveInstallTarget(stored: string | null, agents: { name: string }[]): string {
  if (stored && agents.some((a) => a.name === stored)) return stored;
  return agents[0]?.name ?? "";
}

/** The agent Library installs go to, shared by all four pages and persisted. */
export function useInstallTarget(agents: { name: string }[]): [string, (name: string) => void] {
  const [target, setTarget] = useState(() => resolveInstallTarget(readKey(INSTALL_TARGET_KEY), agents));
  useEffect(() => {
    setTarget((t) => resolveInstallTarget(t || readKey(INSTALL_TARGET_KEY), agents));
  }, [agents]);
  function set(name: string) {
    setTarget(name);
    writeKey(INSTALL_TARGET_KEY, name);
  }
  return [target, set];
}
```

- [x] Create `src/components/detail/library/LibraryDetail.tsx`:

```tsx
import { useT } from "../../../i18n";
import type { TranslationKey } from "../../../i18n/types";
import { DetailPage } from "../../shell/DetailPage";
import { LibraryGlyph } from "../../library/LibraryGlyph";
import type { LibraryAgentUse, LibraryItem } from "./libraryModel";

const KIND_LABEL: Record<LibraryItem["kind"], TranslationKey> = {
  skill: "libraryInspector.kind.skill",
  mcp: "libraryInspector.kind.mcp",
  workflow: "libraryInspector.kind.workflow",
  plugin: "libraryInspector.kind.plugin",
};

export interface LibraryDetailProps {
  item: LibraryItem;
  /** Omit (undefined) for kinds without agent usage (workflows). */
  uses?: LibraryAgentUse[];
  busy: boolean;
  error: string | null;
  onToggle?: (agent: string, enabled: boolean) => void;
  onRemove?: (agent: string) => void;
  onOpenFolder?: () => void;
}

/** The shared Library detail (spec §3.2): description, meta rows, and the
 *  agents that use the item with per-agent enable / remove. */
export function LibraryDetail({ item, uses, busy, error, onToggle, onRemove, onOpenFolder }: LibraryDetailProps) {
  const { t } = useT();
  return (
    <DetailPage
      avatar={<LibraryGlyph kind={item.kind} large />}
      title={item.name}
      meta={<span>{t(KIND_LABEL[item.kind])}</span>}
      actions={
        item.path && onOpenFolder ? (
          <button type="button" className="btn btn--secondary" onClick={onOpenFolder}>
            {t("workflowslib.openFolder")}
          </button>
        ) : undefined
      }
      tabs={[{ id: "overview", label: t("detail.tab.overview") }]}
      activeTab="overview"
      onTab={() => {}}
    >
      {item.description && (
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("libraryInspector.readme")}</div>
          <p className="library-detail__desc">{item.description}</p>
        </div>
      )}
      <div className="detail-card">
        {item.meta.map((m) => (
          <div key={m.label} className="detail-kv">
            <span>{m.label}</span>
            <span className={m.mono ? "mono" : undefined}>{m.value}</span>
            <span />
          </div>
        ))}
      </div>
      {uses && (
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("library.usedBy")}</div>
          {uses.length === 0 && <p className="library-detail__muted">{t("library.notUsed")}</p>}
          {uses.map((u) => (
            <div key={u.agent} className="library-use">
              {u.enabled !== undefined && onToggle ? (
                <label className="library-use__toggle">
                  <input type="checkbox" checked={u.enabled} disabled={busy} onChange={(e) => onToggle(u.agent, e.target.checked)} />
                  <span>{u.agent}</span>
                </label>
              ) : (
                <span>{u.agent}</span>
              )}
              {onRemove && (
                <button type="button" className="btn btn--secondary" disabled={busy} onClick={() => onRemove(u.agent)}>
                  {t("pluginslib.remove")}
                </button>
              )}
            </div>
          ))}
          {error && <p className="save-error">{error}</p>}
        </div>
      )}
    </DetailPage>
  );
}
```

- [x] Create `src/components/detail/library/LibraryPage.tsx` — the generic page:

```tsx
import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../../i18n";
import { SourceList } from "../../shell/SourceList";
import type { SourceFacet, SourceRowData } from "../../shell/sourceListModel";
import type { MenuItemDef } from "../../shell/SplitButton";
import { ListDivider } from "../../shell/ListDivider";
import { LIST_WIDTH_DEFAULT, LIST_WIDTH_MAX, LIST_WIDTH_MIN, useResizableColumn } from "../../shell/useResizableColumn";
import { listModeFor } from "../../shell/breakpoints";
import { useWindowWidth } from "../../shell/useWindowWidth";
import { readKey, writeKey } from "../../shell/persist";
import { LibraryDetail } from "./LibraryDetail";
import type { LibraryAgentUse, LibraryItem } from "./libraryModel";

export type LibraryPageId = "skills" | "mcp" | "plugins" | "workflows";

export interface LibraryPageProps<T> {
  page: LibraryPageId;
  title: string;
  /** The Tauri command that lists the records. */
  listCommand: string;
  idOf: (r: T) => string;
  rows: (records: T[]) => SourceRowData[];
  facets?: (records: T[]) => SourceFacet[];
  item: (r: T) => LibraryItem;
  /** Undefined for kinds without agent usage. */
  uses?: (r: T) => LibraryAgentUse[];
  /** Per-agent commands; each resolves after the backend applied it. */
  toggle?: (r: T, agent: string, enabled: boolean) => Promise<void>;
  remove?: (r: T, agent: string) => Promise<void>;
  /** Header action: reveal this path in Finder. */
  folderOf?: (r: T) => string | null;
  createLabel?: string;
  createItems?: MenuItemDef[];
  toolbar?: ReactNode;
  copy: { loading: string; empty: string; filter: string; noMatch: string };
  /** Bump to reload after a modal installed something. */
  reloadToken?: number;
  /** Modals, rendered outside the grid. */
  children?: ReactNode;
}

export const libraryKeys = (page: LibraryPageId) => ({
  lastSelected: `mur.${page}.lastSelected`,
  listWidth: `mur.${page}.listWidth`,
});

/** The master–detail Library page (spec §3.1). Owns loading, selection
 *  persistence, filter / facet state and the per-agent action lifecycle; the
 *  four pages only supply builders, commands and modals. */
export function LibraryPage<T>(p: LibraryPageProps<T>) {
  const { t } = useT();
  const keys = libraryKeys(p.page);
  const [records, setRecords] = useState<T[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [facet, setFacet] = useState<string | null>(null);
  const [listShown, setListShown] = useState(false);
  const restored = useRef(false);
  const column = useResizableColumn(keys.listWidth, LIST_WIDTH_DEFAULT, LIST_WIDTH_MIN, LIST_WIDTH_MAX);
  const listMode = listModeFor(useWindowWidth());

  const refresh = useCallback(() => {
    setLoading(true);
    invoke<T[]>(p.listCommand)
      .then((res) => {
        setRecords(res);
        setError(null);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [p.listCommand]);

  useEffect(() => {
    refresh();
  }, [refresh, p.reloadToken]);

  // One-shot restore once the list exists; a selection that vanished after a
  // refresh clears itself. The write waits for the restore (Phase 1 lesson).
  useEffect(() => {
    if (records.length === 0) return;
    const ids = records.map(p.idOf);
    if (!restored.current) {
      restored.current = true;
      const last = readKey(keys.lastSelected);
      if (last && ids.includes(last)) setSelected(last);
      return;
    }
    if (selected && !ids.includes(selected)) setSelected(null);
  }, [records, selected, keys.lastSelected, p.idOf]);
  useEffect(() => {
    if (restored.current) writeKey(keys.lastSelected, selected);
  }, [selected, keys.lastSelected]);

  async function act(fn: () => Promise<void>) {
    setBusy(true);
    setActionError(null);
    try {
      await fn();
      refresh();
    } catch (e) {
      setActionError(String(e));
    } finally {
      setBusy(false);
    }
  }

  const current = records.find((r) => p.idOf(r) === selected) ?? null;
  const folder = current && p.folderOf ? p.folderOf(current) : null;
  const cls = `master-detail master-detail--${listMode}${listShown ? " master-detail--list-shown" : ""}`;

  return (
    <div className={cls} style={{ ["--md-list-width" as string]: `${column.width}px` }}>
      <SourceList
        title={p.title}
        count={records.length}
        rows={p.rows(records)}
        facets={p.facets ? p.facets(records) : []}
        allLabel={t("dashboard.all")}
        activeFacet={facet}
        onFacet={setFacet}
        filter={filter}
        onFilter={setFilter}
        filterPlaceholder={p.copy.filter}
        selectedId={selected}
        onSelect={(id) => {
          setSelected(id);
          setListShown(false);
        }}
        createLabel={p.createLabel ?? ""}
        createItems={p.createItems}
        toolbar={p.toolbar}
        emptyState={
          <div className="source-list__empty">
            {loading ? (
              p.copy.loading
            ) : error ? (
              <>
                <p className="save-error">{error}</p>
                <button type="button" className="btn btn--secondary" onClick={refresh}>
                  {t("app.refresh")}
                </button>
              </>
            ) : records.length === 0 ? (
              p.copy.empty
            ) : (
              p.copy.noMatch
            )}
          </div>
        }
      />
      <ListDivider column={column} label={t("shell.resizeList")} />
      <div className="master-detail__detail">
        {listMode === "overlay" && (
          <button type="button" className="btn btn--secondary master-detail__show-list" onClick={() => setListShown((v) => !v)}>
            {t("shell.showList")}
          </button>
        )}
        {current ? (
          <LibraryDetail
            key={p.idOf(current)}
            item={p.item(current)}
            uses={p.uses ? p.uses(current) : undefined}
            busy={busy}
            error={actionError}
            onToggle={p.toggle ? (agent, enabled) => { void act(() => p.toggle!(current, agent, enabled)); } : undefined}
            onRemove={p.remove ? (agent) => { void act(() => p.remove!(current, agent)); } : undefined}
            onOpenFolder={folder ? () => { invoke("reveal_in_finder", { path: folder }).catch((e) => setActionError(String(e))); } : undefined}
          />
        ) : (
          <div className="fleet-view__empty">
            <p>{t("library.selectHint")}</p>
          </div>
        )}
      </div>
      {p.children}
    </div>
  );
}
```

- [x] Create `src/styles/components/library.css` (import in `index.css` after `detail-page.css`):

```css
/* Library pages (spec 2026-09-06 Phase 2a). */
.library-glyph { display: grid; place-items: center; width: 28px; height: 28px; border-radius: var(--radius-md); background: var(--surface-secondary); color: var(--text-secondary); }
.library-glyph--lg { width: 48px; height: 48px; border-radius: var(--radius-lg); }
.library-glyph--lg svg { width: 24px; height: 24px; }
.library-detail__desc { margin: 0; font-size: 13.5px; line-height: 1.55; max-width: 62ch; }
.library-detail__muted { margin: 0; color: var(--text-tertiary); font-size: var(--text-sm); }
.library-use { display: flex; align-items: center; justify-content: space-between; gap: 12px; padding: 7px 0; border-top: 1px solid var(--border-line-subtle); font-size: 13px; }
.library-use:first-of-type { border-top: 0; }
.library-use__toggle { display: flex; align-items: center; gap: 8px; }
.library-picker { display: flex; align-items: center; gap: 8px; font-size: var(--text-sm); color: var(--text-secondary); }
.library-picker select { flex: 1; min-width: 0; }
```

- [x] i18n (both tables): `library.usedBy` "Used by"/"使用中的 agent", `library.notUsed` "No agent uses this yet."/"還沒有 agent 使用。", `library.selectHint` "Select an item, or add one with +."/"選一個項目，或用 + 新增。", `library.meta.category` "Category"/"類別", `library.meta.version` "Version"/"版本", `library.meta.status` "Status"/"狀態", `library.meta.path` "Path"/"路徑", `library.meta.transport` "Transport"/"傳輸", `library.meta.serverId` "Server id"/"伺服器 id", `library.meta.source` "Source"/"來源", `library.meta.skills` "Skills"/"技能", `library.meta.mcp` "MCP"/"MCP", `library.meta.commands` "Commands"/"指令", `library.usedByCount` "used by {count}"/"{count} 個 agent 使用", `library.versionPrefix` "v"/"v".
- [x] `npm test`, `npm run build`. Commit: `feat(hub): LibraryPage, LibraryDetail, libraryModel builders, install-target hook, kind glyphs`

**Interfaces — Produces:** everything exported from `libraryModel.ts`; `<LibraryPage<T> …>` (props above) and `libraryKeys(page)`; `<LibraryDetail item uses? busy error onToggle? onRemove? onOpenFolder?>`; `useInstallTarget(agents): [target, set]`; `<LibraryGlyph kind large?>`; `DetailPageProps.status?`.

### Task 6.5 — Skills page

- [x] Rewrite `src/components/library/SkillsPage.tsx`:

```tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAgents } from "../../context/AgentContext";
import { useT } from "../../i18n";
import { SkillRegistryModal } from "../SkillRegistryModal";
import { SkillAddUrlModal } from "../SkillAddUrlModal";
import { AgentPicker } from "./AgentPicker";
import { LibraryGlyph } from "./LibraryGlyph";
import { LibraryPage } from "../detail/library/LibraryPage";
import { itemFor, skillFacets, skillRows, type InstalledSkillView } from "../detail/library/libraryModel";
import { useInstallTarget } from "../detail/library/useInstallTarget";

/** Skills library (spec §3.1): installed skills | detail with usage.
 *  `skills_installed` does not report per-agent enabled state, so the toggle
 *  starts checked and only the unchecked direction reaches the backend;
 *  re-enabling is done from the agent's Capabilities tab. */
export function SkillsPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [target, setTarget] = useInstallTarget(agents);
  const [showRegistry, setShowRegistry] = useState(false);
  const [showAddUrl, setShowAddUrl] = useState(false);
  const [reload, setReload] = useState(0);
  const metaLabels = {
    category: t("library.meta.category"),
    version: t("library.meta.version"),
    status: t("library.meta.status"),
    path: t("library.meta.path"),
  };

  return (
    <LibraryPage<InstalledSkillView>
      page="skills"
      title={t("nav.skills")}
      listCommand="skills_installed"
      idOf={(s) => s.name}
      rows={(skills) => skillRows(skills, t("library.versionPrefix"), () => <LibraryGlyph kind="skill" />)}
      facets={skillFacets}
      item={(s) => itemFor("skill", s, metaLabels)}
      uses={(s) => s.agents.map((agent) => ({ agent, enabled: true }))}
      toggle={async (s, agent, enabled) => { await invoke("agent_skill_toggle", { name: agent, skillId: s.name, enabled }); }}
      remove={async (s, agent) => { await invoke("agent_skill_uninstall", { name: agent, skillId: s.name }); }}
      folderOf={(s) => s.path}
      createLabel={t("skillslib.add")}
      createItems={[
        { id: "url", label: t("detail.installSkillUrl"), onSelect: () => setShowAddUrl(true), disabled: !target },
        { id: "registry", label: t("detail.browseRegistry"), onSelect: () => setShowRegistry(true), disabled: !target },
      ]}
      toolbar={<div className="library-picker"><AgentPicker agents={agents} value={target} onChange={setTarget} /></div>}
      copy={{ loading: t("skillslib.loading"), empty: t("detail.noSkills"), filter: t("skillslib.filter"), noMatch: t("skillslib.noMatch") }}
      reloadToken={reload}
    >
      {showAddUrl && target && (
        <SkillAddUrlModal agentName={target} onClose={() => setShowAddUrl(false)} onSaved={() => { setReload((n) => n + 1); setShowAddUrl(false); }} />
      )}
      {showRegistry && target && (
        <SkillRegistryModal agentName={target} onClose={() => setShowRegistry(false)} onSaved={() => { setReload((n) => n + 1); setShowRegistry(false); }} />
      )}
    </LibraryPage>
  );
}
```
  `statusBadgeClass` and its test: `grep -rn statusBadgeClass src` — if only the old page used it, delete both.
- [x] i18n (both): `skillslib.filter` "Filter skills"/"篩選技能", `skillslib.noMatch` "No skills match."/"沒有符合的技能。", `skillslib.add` "Add skill"/"新增技能".
- [x] `src/components/DashboardApp.tsx`: `<SkillsPage />` (drop `onSelect`). `Inspector.tsx` is untouched in PR 6: the page no longer reports a selection, so no inspector opens.
- [x] `npm test`, `npm run build`, `npm run lint`.
- [x] Commit: `feat(hub): Skills page is master–detail — usage per agent, enable/remove, install menu`

**Manual acceptance PR 6:** Skills list with category chips and status in the subtitle; selecting shows description, meta, Used by with the agents from `skills_installed`; unchecking an agent calls `agent_skill_toggle` and the list refreshes; Remove calls `agent_skill_uninstall`; "+" opens the two modals with the picked agent and the list reloads after a save; Open folder reveals the skill dir; last selection restores; Esc clears to the hint; the Rust test passes in the Hub GUI CI job.

---

## PR 7 — MCP, Plugins, Workflows; inspector retired

Branch `feat/hub-2-library-rest`.

### Task 7.1 — MCP page

- [x] Rewrite `src/components/library/McpPage.tsx`:

```tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAgents } from "../../context/AgentContext";
import { useT } from "../../i18n";
import { McpDiscoverModal } from "../McpDiscoverModal";
import { McpAddRemoteModal } from "../McpAddRemoteModal";
import { AgentPicker } from "./AgentPicker";
import { LibraryGlyph } from "./LibraryGlyph";
import { LibraryPage } from "../detail/library/LibraryPage";
import { itemFor, mcpFacets, mcpRows, type InstalledMcpView } from "../detail/library/libraryModel";
import { useInstallTarget } from "../detail/library/useInstallTarget";

/** MCP library (spec §3.1). `mcp_installed` reports which agents configure a
 *  server but not the per-agent enabled flag, so the toggle starts checked. */
export function McpPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [target, setTarget] = useInstallTarget(agents);
  const [showDiscover, setShowDiscover] = useState(false);
  const [showAddRemote, setShowAddRemote] = useState(false);
  const [reload, setReload] = useState(0);
  const metaLabels = { transport: t("library.meta.transport"), serverId: t("library.meta.serverId") };

  return (
    <LibraryPage<InstalledMcpView>
      page="mcp"
      title={t("nav.mcp")}
      listCommand="mcp_installed"
      idOf={(s) => s.id}
      rows={(servers) => mcpRows(servers, (n) => t("library.usedByCount", { count: n }), () => <LibraryGlyph kind="mcp" />)}
      facets={mcpFacets}
      item={(s) => itemFor("mcp", s, metaLabels)}
      uses={(s) => s.agents.map((agent) => ({ agent, enabled: true }))}
      toggle={async (s, agent, enabled) => { await invoke("agent_mcp_toggle", { name: agent, serverId: s.id, enabled }); }}
      remove={async (s, agent) => { await invoke("agent_mcp_remove", { name: agent, serverId: s.id }); }}
      createLabel={t("mcplib.add")}
      createItems={[
        { id: "discover", label: t("detail.discoverMcp"), onSelect: () => setShowDiscover(true), disabled: !target },
        { id: "remote", label: t("detail.addRemoteMcp"), onSelect: () => setShowAddRemote(true), disabled: !target },
      ]}
      toolbar={<div className="library-picker"><AgentPicker agents={agents} value={target} onChange={setTarget} /></div>}
      copy={{ loading: t("mcplib.loading"), empty: t("mcplib.empty"), filter: t("mcplib.filter"), noMatch: t("mcplib.noMatch") }}
      reloadToken={reload}
    >
      {showDiscover && target && (
        <McpDiscoverModal agentName={target} onClose={() => setShowDiscover(false)} onImported={() => { setReload((n) => n + 1); setShowDiscover(false); }} />
      )}
      {showAddRemote && target && (
        <McpAddRemoteModal agentName={target} onClose={() => setShowAddRemote(false)} onSaved={() => { setReload((n) => n + 1); setShowAddRemote(false); }} />
      )}
    </LibraryPage>
  );
}
```
- [x] i18n (both): `mcplib.filter` "Filter MCP servers"/"篩選 MCP 伺服器", `mcplib.noMatch` "No MCP servers match."/"沒有符合的 MCP 伺服器。", `mcplib.add` "Add MCP server"/"新增 MCP 伺服器".
- [x] `DashboardApp`: `<McpPage />`. `npm test`, `npm run build`, `npm run lint`. Commit: `feat(hub): MCP page is master–detail`

### Task 7.2 — Plugins page

- [x] Rewrite `src/components/library/PluginsPage.tsx`:

```tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { useAgents } from "../../context/AgentContext";
import { useT } from "../../i18n";
import { AgentPicker } from "./AgentPicker";
import { LibraryGlyph } from "./LibraryGlyph";
import { LibraryPage } from "../detail/library/LibraryPage";
import { itemFor, pluginRows, type InstalledAddonAgg } from "../detail/library/libraryModel";
import { useInstallTarget } from "../detail/library/useInstallTarget";

/** Plugins (add-ons) library (spec §3.1). `addons_installed` carries the real
 *  per-agent enabled flag, so the toggle reflects it. */
export function PluginsPage() {
  const { t } = useT();
  const { agents } = useAgents();
  const [target, setTarget] = useInstallTarget(agents);
  const [reload, setReload] = useState(0);
  const [importError, setImportError] = useState<string | null>(null);
  const metaLabels = {
    source: t("library.meta.source"),
    skills: t("library.meta.skills"),
    mcp: t("library.meta.mcp"),
    commands: t("library.meta.commands"),
  };

  async function importPlugin() {
    if (!target) return;
    setImportError(null);
    try {
      const dir = await open({ directory: true, title: t("pluginslib.import") });
      if (!dir || Array.isArray(dir)) return;
      await invoke("agent_addon_import", { name: target, pluginDir: dir, force: false });
      setReload((n) => n + 1);
    } catch (e) {
      setImportError(String(e));
    }
  }

  return (
    <LibraryPage<InstalledAddonAgg>
      page="plugins"
      title={t("nav.plugins")}
      listCommand="addons_installed"
      idOf={(a) => a.id}
      rows={(addons) => pluginRows(addons, { skills: t("pluginslib.skills"), mcp: t("pluginslib.mcp"), commands: t("pluginslib.commands") }, () => <LibraryGlyph kind="plugin" />)}
      item={(a) => itemFor("plugin", a, metaLabels)}
      uses={(a) => a.agents.map(({ agent, enabled }) => ({ agent, enabled }))}
      toggle={async (a, agent, enabled) => { await invoke("agent_addon_toggle", { name: agent, addonId: a.id, enabled }); }}
      remove={async (a, agent) => { await invoke("agent_addon_remove", { name: agent, addonId: a.id }); }}
      folderOf={(a) => a.source}
      createLabel={t("pluginslib.import")}
      createItems={[{ id: "import", label: t("pluginslib.import"), onSelect: () => { void importPlugin(); }, disabled: !target }]}
      toolbar={
        <div className="library-picker">
          <AgentPicker agents={agents} value={target} onChange={setTarget} />
          {importError && <p className="save-error">{importError}</p>}
        </div>
      }
      copy={{ loading: t("pluginslib.loading"), empty: t("pluginslib.empty"), filter: t("pluginslib.filter"), noMatch: t("pluginslib.noMatch") }}
      reloadToken={reload}
    />
  );
}
```
- [x] i18n (both): `pluginslib.filter` "Filter plugins"/"篩選外掛", `pluginslib.noMatch` "No plugins match."/"沒有符合的外掛。".
- [x] `DashboardApp`: `<PluginsPage />`. Verify. Commit: `feat(hub): Plugins page is master–detail`

### Task 7.3 — Workflows page

- [x] Rewrite `src/components/library/WorkflowsPage.tsx`:

```tsx
import { useT } from "../../i18n";
import { LibraryGlyph } from "./LibraryGlyph";
import { LibraryPage } from "../detail/library/LibraryPage";
import { itemFor, workflowRows, type WorkflowView } from "../detail/library/libraryModel";

// Note: no discover/install section here — workflows arrive in
// `~/.mur/workflows/` automatically (relay-installed or authored locally).
// A server-side shared-registry discovery view is a later concern.

/** Workflows library (spec §3.1): list + detail, Open folder, no agent usage. */
export function WorkflowsPage() {
  const { t } = useT();
  const metaLabels = { path: t("library.meta.path") };
  return (
    <LibraryPage<WorkflowView>
      page="workflows"
      title={t("nav.workflows")}
      listCommand="workflows_list"
      idOf={(w) => w.path}
      rows={(workflows) => workflowRows(workflows, () => <LibraryGlyph kind="workflow" />)}
      item={(w) => itemFor("workflow", w, metaLabels)}
      folderOf={(w) => w.path}
      copy={{ loading: t("workflowslib.loading"), empty: t("workflowslib.empty"), filter: t("workflowslib.filter"), noMatch: t("workflowslib.noMatch") }}
    />
  );
}
```
- [x] i18n (both): `workflowslib.filter` "Filter workflows"/"篩選工作流程", `workflowslib.noMatch` "No workflows match."/"沒有符合的工作流程。".
- [x] `DashboardApp`: `<WorkflowsPage />`. Verify. Commit: `feat(hub): Workflows page is master–detail`

### Task 7.4 — Retire the Library inspector

- [x] Delete `src/components/inspector/LibraryInspector.tsx`. In `src/components/shell/Inspector.tsx` remove the `LibrarySelection` import and the `library` field of `InspectorSelection`, make the `isLibrary(page)` branch of `hasInspector` return `false` with the comment `// Library pages own their detail (Phase 2a); no inspector column.`, and delete the library render branch (and the now-unused `isLibrary` import if nothing else uses it). In `DashboardApp.tsx` remove `libItem`, `setLibItem`, `onLibrarySelect`, the `LibrarySelection` import and `library: libItem` from `inspectorSelection`. `grep -rn 'LibrarySelection\|libItem\|onLibrarySelect' src` → none.
- [x] CSS: `grep -rn 'item-card\|item-list\|tab-empty\|agent-picker' src --include='*.tsx'`; delete from `detail-panel.css` only the `.item-card*` / `.item-list` / `.tab-empty` rules with no remaining user (the agent Capabilities tabs still use `item-card`; keep whatever they reference). Keep `.agent-picker*`.
- [x] i18n: `grep -rn 'libraryInspector\.' src --include='*.tsx'`; remove `libraryInspector.version` and `libraryInspector.origin` from both tables if unused; keep `libraryInspector.kind.*` and `libraryInspector.readme` (LibraryDetail uses them).
- [x] `npm test`, `npm run build`, `npm run lint`. Commit: `refactor(hub): retire LibraryInspector — the Library pages own their detail`

**Deviations (PR #1173):** Tasks 7.2–7.4 landed in one commit — 7.2/7.3 do not compile on their own once no page reports a selection (`noUnusedLocals` on `onLibrarySelect`), so the DashboardApp half of 7.4 travelled with them. Besides `libraryInspector.version`/`origin`, `mcplib.usedBy` and `pluginslib.usedBy` were also unused after the rewrite and were removed; `pluginslib.remove` and `workflowslib.openFolder` stay (LibraryDetail uses them). CSS: only `.item-card--selected` had no remaining user. Verified with `npm test` / `build` / `lint` and the browser acceptance below; not in a real Tauri window (Hub crate target does not fit the drive; no Rust changed in this PR).

**Manual acceptance PR 7:** each of MCP / Plugins / Workflows selects, restores, filters (chips on MCP by transport), opens its install flow with the picked agent and reloads after it, toggles / removes from an agent and refreshes; Plugins shows the real enabled state per agent; Workflows shows Open folder and no Used-by card; no inspector column appears on any Library page; Chats still opens its inspector; Models is unchanged.

## Spec coverage

| Spec § | Task |
|---|---|
| 3.1 pages table | 6.5, 7.1, 7.2, 7.3 |
| 3.2 SourceList / DetailPage / LibraryDetail / libraryModel | 6.2, 6.3, 6.4 |
| 3.3 Rust | 6.1 |
| 3.4 state, persistence, install target | 6.4 (`LibraryPage`, `useInstallTarget`) |
| 3.5 errors, empty states | 6.4 (empty-state slot, action error in the card, Plugins import error in the toolbar) |
| 3.6 tests | every task's first step |
| 3.7 PRs | this plan's structure |
| Inspector retirement | 7.4 |
