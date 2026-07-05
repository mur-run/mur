# Hub Redesign Phase 1: Three-Pane Shell Implementation Plan (1/5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Replace DashboardApp's header-tab navigation with a macOS-style three-pane shell (vibrancy sidebar / content / inspector slot); old views render unchanged in the content area.

**Architecture:** New `Shell.tsx` owns navigation state (extends the existing `surface` state to the full page inventory); `DashboardApp.tsx` (959 lines) is decomposed into shell + pages + GridCard. No behavior change to any view. Spec: `docs/superpowers/specs/2026-07-05-hub-mission-control-redesign.md` §1, §6 phase 1.

**Tech Stack:** React + TS, vitest, CSS in `mur-hub-gui/ui/src/styles/` (tokens exist: `tokens/primitives.css`, `tokens/semantic.css`).

## Global Constraints

- ≤800 lines per source file (this phase EXISTS to fix DashboardApp's 959).
- UI tests: `cd mur-hub-gui/ui && npm test` (vitest). Rust untouched this phase.
- Build check: `npm run build` in `mur-hub-gui/ui` (Rust hub clippy needs `ui/dist` — keep dist buildable).
- Brand copy uppercase "MUR". System font stack, currentColor SVG icons.
- Pure movement first; any behavior change is a plan bug.

---

### Task 1: Navigation model + Sidebar component

**Files:**
- Create: `mur-hub-gui/ui/src/components/shell/nav.ts`
- Create: `mur-hub-gui/ui/src/components/shell/Sidebar.tsx`
- Create: `mur-hub-gui/ui/src/styles/components/shell.css` (import from `styles/index.css`)
- Test: `mur-hub-gui/ui/src/components/shell/nav.test.ts`

**Interfaces (produces):**

```ts
// nav.ts
export type PageId = "home" | "chats" | "agents" | "fleets"
  | "skills" | "workflows" | "mcp" | "models" | "plugins";
export interface NavItem { id: PageId; labelKey: string; group: "workspace" | "library"; }
export const NAV_ITEMS: NavItem[];               // ordered per spec §1
export function isLibrary(id: PageId): boolean;
// Sidebar.tsx
export function Sidebar(props: { active: PageId; badge: number;
  onSelect: (id: PageId) => void }): JSX.Element;
```

- [ ] **Step 1: Failing test** (`nav.test.ts`): NAV_ITEMS order is exactly home,chats,agents,fleets,skills,workflows,mcp,models,plugins; groups split 4/5; `isLibrary("skills")===true`, `isLibrary("home")===false`.
- [ ] **Step 2:** `npm test -- nav` → fail.
- [ ] **Step 3:** Implement `nav.ts`; `Sidebar.tsx` renders two labeled groups, active highlight, badge pill on Home when `badge>0`, monochrome inline SVG icons (extend the DashboardApp glyph pattern). i18n keys via `useT` like existing components.
- [ ] **Step 4:** `npm test` PASS.
- [ ] **Step 5:** Commit `feat(hub-ui): sidebar + navigation model`.

### Task 2: Shell layout with inspector slot

**Files:**
- Create: `mur-hub-gui/ui/src/components/shell/Shell.tsx`
- Modify: `mur-hub-gui/ui/src/styles/components/shell.css`
- Test: `mur-hub-gui/ui/src/components/shell/shell.test.ts` (pure helpers)

**Interfaces (produces):**

```ts
export function Shell(props: {
  page: PageId; onNavigate: (id: PageId) => void; badge: number;
  inspector?: ReactNode;        // undefined => column hidden
  children: ReactNode;          // content area
}): JSX.Element;
```

Grid layout: `sidebar 220px | content 1fr | inspector 320px (when present)`; `⌘⌥I` toggles a `inspectorVisible` state that hides the column even when `inspector` is provided. Sidebar gets the vibrancy treatment: `backdrop-filter: blur` + translucent background var — **validate on the real .app**; if WKWebView artifacts appear, flip the `--sidebar-bg` token to solid (fallback documented in spec §5, non-blocking).

- [ ] **Step 1: Failing test** — keyboard-shortcut matcher helper `isInspectorToggle(e: KeyboardEvent): boolean` (meta+alt+i, case-insensitive, no other modifiers).
- [ ] **Step 2:** fail.
- [ ] **Step 3:** Implement Shell + CSS (tokens from `styles/tokens/`; 8px radii, 13px base, `-apple-system` stack — put shared values in tokens if missing rather than hardcoding).
- [ ] **Step 4:** `npm test` + `npm run build` PASS.
- [ ] **Step 5:** Commit `feat(hub-ui): three-pane shell layout`.

### Task 3: Decompose DashboardApp into the shell

**Files:**
- Create: `mur-hub-gui/ui/src/components/agents/AgentsPage.tsx` (the grid/list + role filter, moved)
- Create: `mur-hub-gui/ui/src/components/agents/GridCard.tsx` (moved verbatim)
- Modify: `mur-hub-gui/ui/src/components/DashboardApp.tsx` — becomes the shell host: keeps the modal orchestration, update banner, CLI-skew banner; maps old `surface` state to `PageId`; renders `<Shell page=…>{pageContent}</Shell>`; page mapping: agents→AgentsPage, chats→ChatsView, fleets→FleetView, work→WorkView (kept until Phase 2), home/skills/workflows/mcp/models/plugins→`<PlaceholderPage id/>` (one 20-line component: icon + "coming in this redesign" + link to the old modal where one exists, e.g. models placeholder opens ModelLibrary modal).
- Test: existing tests must stay green.

- [ ] **Step 1:** Move GridCard + agent grid into the new files (pure movement; imports only).
- [ ] **Step 2:** Rewire DashboardApp to Shell; DetailPanel: keep current full-page behavior for now by rendering it in the content area when an agent is open (inspector migration is Phase 4) — pass `inspector={undefined}`.
- [ ] **Step 3:** `npm test` + `npm run build` green; `wc -l` on DashboardApp.tsx, AgentsPage.tsx ≤ 800 each.
- [ ] **Step 4:** Visual smoke on dev (`npm run dev` inside the tauri dev shell or local .app build): all four old surfaces reachable from sidebar, modals still open, pet/popover/chat windows unaffected (`App.tsx` routes untouched).
- [ ] **Step 5:** Commit `refactor(hub-ui): decompose DashboardApp into three-pane shell`.

### Task 4: Local .app visual acceptance

- [ ] Build per the verified recipe (sidecars from installed app, `npx @tauri-apps/cli build` native, ad-hoc sign), install, launch.
- [ ] Verify: vibrancy sidebar OK or fallback flipped; dark + light mode; window resize (min-width guard so three panes don't crush); badge renders with a fake count.
- [ ] Commit any token/CSS fixes: `fix(hub-ui): shell polish from .app acceptance`.
