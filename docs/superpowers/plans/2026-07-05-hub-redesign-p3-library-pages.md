# Hub Redesign Phase 3: Library Pages Implementation Plan (3/5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Five Library pages (Skills, MCP Servers, Models, Plugins, Workflows) replace six browse-type modals, sharing one component family.

**Architecture:** One shared template — `LibraryPage = InstalledList + DiscoverGrid + install-source actions` — instantiated five times with per-type data adapters. Existing modal INTERNALS (registry fetch, discover, install flows) are reused as page content; the modal shells die. ModelPicker and all consent/wizard dialogs remain dialogs. Spec §3. **Depends on Phase 1; Phase 2's upgrade-status command feeds the InstalledList status column.**

**Tech Stack:** React + TS + vitest; existing tauri commands only (no new Rust).

## Global Constraints

- Same as Phase 1. One page per PR (spec §6). No new Rust commands — if a page seems to need one, the old modal already had a way; find it.
- Every InstalledList shows an upgrade-status column sourced from `skill_upgrade_status` (skills) or "—" where the concept doesn't apply yet.

---

### Task 1: Shared LibraryPage template

**Files:**
- Create: `mur-hub-gui/ui/src/components/library/LibraryPage.tsx`
- Create: `mur-hub-gui/ui/src/components/library/types.ts`
- Create: `mur-hub-gui/ui/src/styles/components/library.css`
- Test: `mur-hub-gui/ui/src/components/library/libraryFilter.test.ts`

**Interfaces (produces):**

```ts
export interface LibraryItem { id: string; name: string; description: string;
  installed: boolean; official: boolean; status?: string; meta?: Record<string,string>; }
export interface LibraryAdapter {
  listInstalled(): Promise<LibraryItem[]>;
  discover?(query: string): Promise<LibraryItem[]>;   // absent => no discover section
  actions: { labelKey: string; onClick(): void }[];    // install-source buttons (open existing dialogs)
  onSelect(item: LibraryItem): void;                   // Phase 4 wires inspector; until then no-op or detail dialog
}
export function LibraryPage(props: { adapter: LibraryAdapter }): JSX.Element;
export function filterItems(items: LibraryItem[], q: string): LibraryItem[]; // name+description, case-insensitive
```

- [ ] **Step 1: Failing test** — `filterItems`: matches name, matches description, case-insensitive, empty query = all.
- [ ] **Step 2:** fail. **Step 3:** implement template (search field, installed table w/ status column, discover grid w/ official badge, action buttons row). **Step 4:** PASS + build. **Step 5:** commit `feat(hub-ui): shared Library page template`.

### Task 2: Skills page (PR of its own, pattern-setter)

**Files:**
- Create: `mur-hub-gui/ui/src/components/library/SkillsPage.tsx` (adapter)
- Modify: `DashboardApp.tsx` (skills placeholder → SkillsPage)
- Gut: `SkillRegistryModal.tsx`, `SkillAddUrlModal.tsx` — registry browse/list logic moves into the adapter; the URL-install FORM stays a dialog opened from an action button; delete the registry-browse modal shell.

- [ ] **Step 1:** Adapter: `listInstalled` from the tauri command SkillRegistryModal already uses for installed skills, merged with `skill_upgrade_status` → status column (`up to date` / `update available` / `modified` / `—` for unstamped); `discover` = registry search command; actions = [Install from URL (existing dialog), Install pack… (new thin dialog calling `mur agent skill install-pack` command if exposed, else CLI-copy hint)].
- [ ] **Step 2:** `npm test` + build green; modal shells deleted, no dangling imports.
- [ ] **Step 3:** .app smoke: browse registry, install one, status column updates.
- [ ] **Step 4:** Commit `feat(hub-ui): Skills library page (replaces registry/url modals)`.

### Task 3: MCP Servers page

Same recipe as Task 2 with `McpDiscoverModal` (discover logic → adapter) + `McpAddRemoteModal` (stays a dialog, opened by action button; it's a multi-step auth flow). InstalledList from the existing per-agent MCP listing command (aggregate across agents; `meta` shows which agents use it).

- [ ] Adapter + page + placeholder swap; discover modal shell deleted.
- [ ] Tests/build/smoke; commit `feat(hub-ui): MCP Servers library page`.

### Task 4: Models page

`ModelLibrary.tsx` + `ModelLibraryPanels.tsx` become the page content (they're already panel-shaped; move, reskin to library.css, delete their modal wrapper). `ModelPickerModal` and `ModelSetupWizard` untouched (dialogs by design). InstalledList = registry aliases from `models_admin` commands; discover = provider connect + local-runtime detect panels (existing logic).

- [ ] Move + rewire + delete wrapper; placeholder swap.
- [ ] Tests (existing modelLibraryHelpers.test.ts keeps passing) / build / smoke; commit `feat(hub-ui): Models library page`.

### Task 5: Plugins page

Existing plugin import/discover flows (`PresetImportModal` stays a dialog; plugin discover/import commands from the #506 work) behind the template. InstalledList shows per-agent enablement summary in `meta`.

- [ ] Adapter + page; commit `feat(hub-ui): Plugins library page`.

### Task 6: Workflows page (new, small)

**Files:** `mur-hub-gui/ui/src/components/library/WorkflowsPage.tsx`; a thin tauri command `workflows_list() -> Vec<{name, description, path, updated_at}>` reading `~/.mur/workflows/*.yaml` via `mur_common::Workflow` parse (this is the phase's one allowed new Rust command — the concept had no modal before).

- [ ] Rust test: temp dir with one valid + one invalid yaml → list returns the valid one, warns on the other. Implement; nextest PASS.
- [ ] Page: InstalledList only (no discover — Dashboard/registry workflow discovery arrives with the server side); actions = [Open folder]. Receives relay-installed workflows automatically (they land in the same dir).
- [ ] Tests/build/smoke; commit `feat(hub): Workflows library page + workflows_list command`.
