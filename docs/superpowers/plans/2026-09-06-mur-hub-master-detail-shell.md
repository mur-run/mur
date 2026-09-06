# Plan — MUR Hub 2.0 master–detail shell (Phase 1)

> **Execute with `mur-executing-plans`.** Five PRs, strictly in order. Each PR
> is its own branch cut from a **fresh `main` after the previous PR merged**
> (`main` is squash-merged; stacked branches break). Work in a worktree under
> `/Volumes/Firecuda4tb/Projects/mur/.worktrees/hub-2-pr<N>`. Do not begin a
> PR until the previous one is merged and `main` is pulled.

Design: `docs/superpowers/specs/2026-09-06-mur-hub-master-detail-shell-design.md`.
Mockup: https://claude.ai/code/artifact/afaac847-5a8a-4ed5-9823-358d6731c256

## Goal

Replace the 320 px inspector on the Agents and Fleets pages with a
master–detail shell (fixed-width source list + full-width detail) and a
neutral-first design language, with no window that resizes itself.

## Architecture

The `Shell` keeps its sidebar + content grid and learns to collapse the sidebar
by breakpoint. The Agents and Fleets **pages** render a `SourceList`, a
draggable `ListDivider` and a `DetailPage` inside the content column; the
inspector column is no longer produced for those pages. Tokens change once,
globally (PR 1); layouts change per page (PR 4, PR 5). New shared code lives in
`components/shell/` (Status, breakpoints, SourceList, DetailPage, SplitButton,
OverflowMenu, CommandPalette) and page-specific detail code in
`components/detail/{agent,fleet}/`.

## Tech stack

React 18 + TypeScript 5.5, Vite 5, Vitest 4 **without jsdom** (tests are pure
functions or `renderToStaticMarkup`), plain CSS with two-tier tokens
(`styles/tokens/primitives.css` → `styles/tokens/semantic.css`), Tauri 2
(`@tauri-apps/api` 2, `@tauri-apps/plugin-dialog`), the Hub's lightweight i18n
(`i18n/en.ts` defines the key set; `i18n/zh-TW.ts` is typed `Table =
Record<TranslationKey, string>`, so a missing key fails `tsc`).

## Global Constraints

Copied from the approved design and `CLAUDE.md`. Every task includes all of them.

1. Brand name is uppercase **MUR** in every user-visible string.
2. Single source file ≤ 800 lines; split on the sibling pattern (`components/detail/agent/{Header,Overview,…}.tsx`).
3. Every new user-visible string is added to **both** `src/i18n/en.ts` and `src/i18n/zh-TW.ts` in the same commit.
4. Components reference only semantic tokens from `src/styles/tokens/semantic.css`; no raw hex in component CSS or TSX (the existing `CATEGORY_COLORS` table is the one exception).
5. No hardcoded numbers in TSX: widths, breakpoints, limits and storage keys are named constants.
6. Nothing calls `getCurrentWindow().setSize()` or `setMinSize()` after launch.
7. Every new token is defined for light in the bare `:root` block and for dark in **both** the `@media (prefers-color-scheme: dark)` block and the `:root[data-theme="dark"]` block, and restated in `:root[data-theme="light"]`.
8. All motion is disabled under `prefers-reduced-motion: reduce`.
9. Tests never touch the DOM: pure functions, or `renderToStaticMarkup` for markup.
10. Every PR leaves the app usable: `npm run build`, `npm test`, `npm run lint` green, and that PR's manual acceptance list passes.
11. Unattended approvals, budgets and the fleet kill-switch are not touched by this plan.

## Working agreement

- Paths are relative to `mur-hub-gui/ui/` unless they start with `mur-hub-gui/src-tauri/` or `docs/`.
- Line numbers cite `main` at commit `32cc07cf` (2026-09-06). They drift; re-check with `grep -n` before cutting.
- Commands run from `mur-hub-gui/ui/`:
  - `npm test -- <path>` runs one test file; `npm test` runs all (Vitest prints `Test Files  N passed`).
  - `npm run build` is the type gate (`tsc -b && vite build`; success ends with `✓ built in …`).
  - `npm run lint`.
- Preview in the real window (two terminals, from `mur-hub-gui/ui/`): `npm run dev` (port 5174), then `cd .. && npx --yes @tauri-apps/cli@2 dev`. The three sidecar stubs under `mur-hub-gui/src-tauri/binaries/` must first be replaced by the real binaries from the installed app:
  ```bash
  APP="/Applications/MUR Hub.app/Contents/MacOS"
  cd mur-hub-gui/src-tauri/binaries
  for b in mur mur-agent-runtime mlx-server; do cp "$APP/$b" "./$b-aarch64-apple-darwin"; chmod +x "./$b-aarch64-apple-darwin"; done
  ```
  Never commit those binaries.
- Commit after every task with the message given. Never push tags.

## File structure

| PR | File | Responsibility |
|---|---|---|
| 1 | `src/components/shell/Status.tsx` (new) | `StatusKind`, `statusOf`, `fleetStatusOf`, `StatusDot`, `StatusPill`, `NeedsYouBadge` |
| 1 | `src/components/shell/Status.test.tsx` (new) | mapping + markup tests |
| 1 | `src/styles/components/status.css` (new) | the only CSS for status dots/pills/badge |
| 1 | `src/styles/tokens/primitives.css` (modify) | neutral ramps, radius aliases, shell size primitives, mono font |
| 1 | `src/styles/tokens/semantic.css` (modify) | surface/status/attention tokens, both themes |
| 1 | `src/styles/index.css` (modify) | import `status.css` |
| 1 | `src/styles/components/{dashboard,shell,detail-panel,primitives,fleet}.css` (modify) | recolor only: drop tints/gradients/glow, adopt new tokens, remove legacy `.pill*` and `.fleet-rail__status*` |
| 1 | `src/utils.ts` (modify) | delete `runtimePill` |
| 1 | `src/components/agents/{AgentsPage,GridCard}.tsx`, `src/components/inspector/AgentInspector.tsx`, `src/components/fleet/{FleetDetail,FleetRail,FleetView}.tsx` (modify) | adopt `StatusPill`/`StatusDot` |
| 2 | `src/components/shell/persist.ts` (new) | `readKey`/`writeKey` localStorage wrappers |
| 2 | `src/components/shell/breakpoints.ts` (+ `.test.ts`) (new) | width → sidebar/list mode; sidebar pref persistence |
| 2 | `src/components/shell/useWindowWidth.ts` (new) | `window.innerWidth` hook |
| 2 | `src/components/shell/platform.ts` (new) | `isMac()` |
| 2 | `src/components/shell/Shell.tsx` (modify) | sidebar collapse, ⌘\, title-bar inset, banners slot |
| 2 | `src/components/shell/Sidebar.tsx` (modify) | collapsed rendering, Settings footer, version |
| 2 | `src/components/shell/useResizableColumn.ts` (+ `.test.ts`), `ListDivider.tsx` (new) | draggable, persisted column width |
| 2 | `src/styles/components/shell.css` (modify) | collapsed rail, inset, page slot, divider, master-detail grid |
| 2 | `mur-hub-gui/src-tauri/tauri.conf.json`, `mur-hub-gui/src-tauri/capabilities/default.json` (modify) | overlay title bar, 1200×760 / 900×560, start-dragging permission |
| 2 | `src/components/DashboardApp.tsx` (modify) | delete window auto-resize; banners → Shell slot; Settings → sidebar |
| 3 | `src/components/shell/sourceList.ts` (+ `.test.ts`), `SourceList.tsx` (+ `.test.tsx`) (new) | list pane: header, filter, facet chips, rows, keyboard |
| 3 | `src/components/shell/DetailPage.tsx` (+ `.test.ts`) (new) | header + ARIA tabs + body |
| 3 | `src/components/shell/dirty.tsx` (+ `.test.ts`) (new) | `DirtyProvider`, `useMarkDirty`, `useDirtyGuard` |
| 3 | `src/components/shell/useMenu.ts`, `SplitButton.tsx`, `OverflowMenu.tsx` (+ `.test.tsx`) (new) | split primary action; ⋯ menu |
| 3 | `src/components/shell/detailTabs.ts` (+ `.test.ts`) (new) | agent/fleet tab ids, labels, `detailGroupOf` |
| 3 | `src/components/shell/palette.ts` (+ `.test.ts`), `CommandPalette.tsx` (new) | ⌘K ranking + overlay |
| 3 | `src/components/DashboardApp.tsx`, `Sidebar.tsx`, `fleet/FleetView.tsx` (modify) | palette wiring, sidebar search button, `requestedName` |
| 3 | `src/styles/components/{source-list,detail-page,menus,palette}.css` (new) | styles for the above |
| 4 | `src/components/home/inbox.ts`, `useInbox.ts`, `needsYou.ts` (+ tests) (modify/new) | `InboxItem.agent`, `needsYouCounts` |
| 4 | `src/components/detail/agent/agentOverview.ts` (+ `.test.ts`) (new) | per-agent channel activity |
| 4 | `src/components/detail/agent/{AgentDetail,OverviewTab,IdentityTab,CapabilitiesTab,ChannelsTab}.tsx` (new) | agent detail pane; `AgentInspector` content relocated |
| 4 | `src/components/inspector/tabs/{PersonaTab,StyleTab}.tsx` (modify) | `useMarkDirty` |
| 4 | `src/components/agents/{AgentsPage,AgentsOverview}.tsx` (rewrite/new) | master–detail page; overview when nothing selected |
| 4 | `src/components/chats/ChatsPage.tsx` (modify) | `initialAgent` prop + own filter field |
| 4 | `src/components/shell/Inspector.tsx`, `src/components/DashboardApp.tsx` (modify) | no inspector for agents; global bar removed; `openChatWith` |
| 4 | delete `src/components/inspector/AgentInspector.tsx`; CSS cleanup in `dashboard.css`, `detail-panel.css` | — |
| 5 | `src/components/detail/fleet/{fleetActions.ts,FleetHeader,FleetOverview,FleetMembers,FleetJobs,FleetSettings}.tsx` (new) | `FleetDetail.tsx` split by tab |
| 5 | `src/components/fleet/FleetView.tsx` (rewrite) | master–detail page on `SourceList` |
| 5 | delete `src/components/fleet/{FleetDetail,FleetRail}.tsx`, `src/components/inspector/FleetInspector.tsx`; `fleet.css` cleanup | — |
| all | `src/i18n/en.ts`, `src/i18n/zh-TW.ts` (modify) | every new string |

---

## PR 1 — Tokens and one status vocabulary

Branch `feat/hub-2-tokens`. Recolor only; no layout changes.

### Task 1.1 — `Status.tsx`

- [x] Write `src/components/shell/Status.test.tsx`:

```tsx
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { NeedsYouBadge, StatusDot, fleetStatusOf, statusOf } from "./Status";

describe("statusOf", () => {
  it("maps every runtime state; stopped and unknown read as idle", () => {
    expect(statusOf({ state: "running", pid: 1 })).toBe("running");
    expect(statusOf({ state: "restarting", attempt: 1, backoff_secs: 2 })).toBe("restarting");
    expect(statusOf({ state: "failed" })).toBe("failed");
    expect(statusOf({ state: "stopped" })).toBe("idle");
    expect(statusOf(undefined)).toBe("idle");
  });
});

describe("fleetStatusOf", () => {
  it("kill-switch wins over running", () => {
    expect(fleetStatusOf({ stopped: true, running: true })).toBe("stopped");
    expect(fleetStatusOf({ stopped: false, running: true })).toBe("running");
    expect(fleetStatusOf({ stopped: false, running: false })).toBe("idle");
  });
});

describe("markup", () => {
  it("dot carries the kind as a modifier class", () => {
    expect(renderToStaticMarkup(<StatusDot kind="failed" />)).toContain("status-dot--failed");
  });
  it("badge renders nothing at zero and caps at 99+", () => {
    expect(renderToStaticMarkup(<NeedsYouBadge count={0} />)).toBe("");
    expect(renderToStaticMarkup(<NeedsYouBadge count={120} />)).toContain("99+");
    expect(renderToStaticMarkup(<NeedsYouBadge count={3} />)).toContain(">3<");
  });
});
```

- [x] Run `npm test -- src/components/shell/Status.test.tsx`. Expect `FAIL` with `Error: Failed to resolve import "./Status"`.
- [x] Create `src/components/shell/Status.tsx`:

```tsx
import { useT } from "../../i18n";
import type { TranslationKey } from "../../i18n/types";
import type { RuntimeState } from "../../types";

/** The only status vocabulary in the Hub (spec §4.5). Amber on a BADGE means
 *  "needs you"; amber on a PILL means restarting. They never share a shape. */
export type StatusKind = "running" | "idle" | "restarting" | "stopped" | "failed";

/** Agent runtime → kind. A stopped agent is the ordinary not-running state and
 *  reads as idle; the red `stopped` kind is reserved for a fleet kill-switch. */
export function statusOf(rt: RuntimeState | undefined): StatusKind {
  switch (rt?.state) {
    case "running":
      return "running";
    case "restarting":
      return "restarting";
    case "failed":
      return "failed";
    default:
      return "idle";
  }
}

export function fleetStatusOf(f: { stopped: boolean; running: boolean }): StatusKind {
  if (f.stopped) return "stopped";
  return f.running ? "running" : "idle";
}

const LABEL_KEY: Record<StatusKind, TranslationKey> = {
  running: "status.running",
  idle: "status.idle",
  restarting: "status.restarting",
  stopped: "status.stopped",
  failed: "status.failed",
};

export function StatusDot({ kind, title }: { kind: StatusKind; title?: string }) {
  return (
    <span
      className={`status-dot status-dot--${kind}`}
      title={title}
      role={title ? "img" : undefined}
      aria-label={title}
      aria-hidden={title ? undefined : true}
    />
  );
}

export function StatusPill({ kind }: { kind: StatusKind }) {
  const { t } = useT();
  return (
    <span className={`status-pill status-pill--${kind}`}>
      <span className="status-pill__dot" aria-hidden="true" />
      {t(LABEL_KEY[kind])}
    </span>
  );
}

const BADGE_CAP = 99;

export function NeedsYouBadge({ count, title }: { count: number; title?: string }) {
  if (count <= 0) return null;
  return (
    <span className="needs-you" title={title} aria-label={title}>
      {count > BADGE_CAP ? `${BADGE_CAP}+` : count}
    </span>
  );
}
```

- [x] Add to `src/i18n/en.ts` (next to the existing `status.*` keys, line 52) and the same keys to `src/i18n/zh-TW.ts`:

```ts
  "status.restarting": "restarting",   // zh-TW: "重啟中"
  "status.stopped": "stopped",         // zh-TW: "已停止"
  "status.failed": "failed",           // zh-TW: "失敗"
  "status.needsYou": "{count} waiting for you",  // zh-TW: "{count} 件等你處理"
```

- [x] Add to the bare `:root` block of `src/styles/tokens/semantic.css` and to **each** of the two dark blocks and the light block:

```css
  --status-stopped:var(--red-600); --status-attention:var(--amber-500); --text-on-attention:#1A1200;
```
  (dark blocks use `--status-stopped:var(--red-500)`; the other two lines are identical in all four blocks.)

- [x] Create `src/styles/components/status.css`:

```css
/* One status vocabulary (spec §4.5). Colours come only from semantic tokens. */
.status-dot {
  display: inline-block; width: 8px; height: 8px; border-radius: 50%;
  background: currentColor; flex: none;
}
.status-pill {
  display: inline-flex; align-items: center; gap: 6px; height: 22px;
  padding: 0 9px 0 8px; border-radius: var(--radius-full);
  font-size: var(--text-xs); font-weight: var(--fw-semi); white-space: nowrap;
  background: var(--surface-secondary); color: var(--text-secondary);
}
.status-pill__dot { width: 7px; height: 7px; border-radius: 50%; background: var(--dot, currentColor); }
.status-dot--running, .status-pill--running { color: var(--status-running); }
.status-dot--idle { color: var(--status-idle); }
.status-pill--idle { --dot: var(--status-idle); }
.status-dot--restarting, .status-pill--restarting { color: var(--status-restarting); }
.status-dot--failed, .status-pill--failed { color: var(--status-failed); }
.status-dot--stopped, .status-pill--stopped { color: var(--status-stopped); }
.needs-you {
  display: inline-grid; place-items: center; min-width: 18px; height: 18px; padding: 0 6px;
  border-radius: var(--radius-full); font-size: var(--text-xs); font-weight: var(--fw-semi);
  font-variant-numeric: tabular-nums; background: var(--status-attention); color: var(--text-on-attention);
  flex: none;
}
```

- [x] In `src/styles/index.css` add `@import "./components/status.css";` directly after `@import "./components/primitives.css";`.
- [x] Run the test again → `Test Files  1 passed`. Run `npm run build` → `✓ built in`.
- [x] Commit: `feat(hub): one status vocabulary — StatusDot, StatusPill, NeedsYouBadge`

**Interfaces — Produces:** `StatusKind`, `statusOf(rt: RuntimeState | undefined): StatusKind`, `fleetStatusOf({stopped, running}): StatusKind`, `<StatusDot kind title?>`, `<StatusPill kind>`, `<NeedsYouBadge count title?>`; CSS `.status-dot--<kind>`, `.status-pill--<kind>`, `.needs-you`; tokens `--status-stopped`, `--status-attention`, `--text-on-attention`.

### Task 1.2 — Neutral-first tokens

- [x] Replace the light and dark neutral lines, the radius line, the type line and the shell-layout lines in `src/styles/tokens/primitives.css` (lines 13–19, 24, 27–30, 42–48) with:

```css
  /* neutral light — cool-biased greys, no blue tint (spec §5.1) */
  --n-bg:#F4F5F8; --n-sidebar:#EEF0F4; --n-list:#F9FAFC; --n-detail:#FFFFFF;
  --n-secondary:#F4F5F8; --n-card:#FFFFFF; --n-hover:#ECEEF2;
  --n-line:rgba(22,36,59,.10); --n-line-subtle:rgba(22,36,59,.06);
  --n-text:#16243B; --n-text2:#5F6C80; --n-text3:#98A3B3;
  /* neutral dark */
  --d-bg:#0F1117; --d-sidebar:#0F1117; --d-list:#13161E; --d-detail:#161922;
  --d-secondary:#20263A; --d-card:#1B2030; --d-hover:#232838;
  --d-line:rgba(255,255,255,.08); --d-line-subtle:rgba(255,255,255,.05);
  --d-text:#EEF2F7; --d-text2:#94A3B8; --d-text3:#6B7688;
```
```css
  /* radius — 6 controls, 8 rows, 12 cards/panes. lg/xl/2xl are aliases kept
     until nothing references them (spec §5.2). */
  --radius-sm:6px; --radius-md:8px; --radius-lg:12px; --radius-xl:12px; --radius-2xl:12px; --radius-full:9999px;
```
```css
  --font-mono:ui-monospace,"SF Mono",Menlo,Consolas,monospace;
  --text-xs:11px; --text-sm:12.5px; --text-base:14px; --text-md:15px;
  --text-lg:17px; --text-xl:20px; --text-2xl:22px;
```
```css
  /* shell layout (spec §3.1) */
  --shell-sidebar-width:220px; --shell-sidebar-collapsed-width:56px; --shell-inspector-width:320px;
  --shell-list-width:300px; --shell-list-width-compact:280px; --shell-list-min:240px; --shell-list-max:400px;
  --shell-titlebar-inset:28px; --shell-blur:20px;
  --n-sidebar-vibrancy:rgba(238,240,244,.72); --n-sidebar-solid:#EEF0F4;
  --d-sidebar-vibrancy:rgba(15,17,23,.6); --d-sidebar-solid:#0F1117;
```
  Also add `--blue-400:#5B9CE0;` after `--blue-500`, change `--blue-soft` to `rgba(74,144,217,.14)` and `--blue-soft-dark` to `rgba(91,156,224,.18)`, and add `--slate-500:#7C8798;` after `--slate-400`.

- [x] Replace the whole of `src/styles/tokens/semantic.css` with:

```css
:root {
  --color-brand:var(--blue-500); --color-brand-strong:var(--blue-700);
  --text-on-brand:#fff;
  --color-accent:var(--coral-500); --color-accent-strong:var(--coral-600);
  --color-accent-soft:var(--coral-soft); --color-brand-soft:var(--blue-soft);
  /* surfaces (spec §5.1): window < sidebar < list < detail = card */
  --surface-bg:var(--n-bg); --surface-window:var(--n-bg); --surface-sidebar:var(--n-sidebar);
  --surface-list:var(--n-list); --surface-detail:var(--n-detail);
  --surface-secondary:var(--n-secondary); --surface-card:var(--n-card); --surface-hover:var(--n-hover);
  --border-line:var(--n-line); --border-line-subtle:var(--n-line-subtle);
  --text-primary:var(--n-text); --text-secondary:var(--n-text2); --text-tertiary:var(--n-text3);
  --status-running:var(--green-600); --status-idle:var(--slate-400);
  --status-restarting:var(--amber-500); --status-failed:var(--red-600);
  --status-stopped:var(--red-600); --status-attention:var(--amber-500); --text-on-attention:#1A1200;
  --shadow-1:var(--shadow-card); --shadow-pop:var(--shadow-pop); --shadow-focus:var(--shadow-focus);
  --sidebar-bg:var(--n-sidebar-vibrancy); --sidebar-bg-solid:var(--n-sidebar-solid);
}
@media (prefers-color-scheme: dark) {
  :root:not([data-theme="light"]) {
    --color-brand:var(--blue-400); --color-brand-strong:var(--blue-300);
    --color-brand-soft:var(--blue-soft-dark); --color-accent-soft:var(--coral-soft-dark);
    --color-accent-strong:var(--coral-300);
    --surface-bg:var(--d-bg); --surface-window:var(--d-bg); --surface-sidebar:var(--d-sidebar);
    --surface-list:var(--d-list); --surface-detail:var(--d-detail);
    --surface-secondary:var(--d-secondary); --surface-card:var(--d-card); --surface-hover:var(--d-hover);
    --border-line:var(--d-line); --border-line-subtle:var(--d-line-subtle);
    --text-primary:var(--d-text); --text-secondary:var(--d-text2); --text-tertiary:var(--d-text3);
    --status-running:var(--green-500); --status-idle:var(--slate-500); --status-failed:var(--red-500);
    --status-stopped:var(--red-500); --status-attention:var(--amber-500); --text-on-attention:#1A1200;
    --sidebar-bg:var(--d-sidebar-vibrancy); --sidebar-bg-solid:var(--d-sidebar-solid);
  }
}
:root[data-theme="dark"] {
  --color-brand:var(--blue-400); --color-brand-strong:var(--blue-300);
  --color-brand-soft:var(--blue-soft-dark); --color-accent-soft:var(--coral-soft-dark);
  --color-accent-strong:var(--coral-300);
  --surface-bg:var(--d-bg); --surface-window:var(--d-bg); --surface-sidebar:var(--d-sidebar);
  --surface-list:var(--d-list); --surface-detail:var(--d-detail);
  --surface-secondary:var(--d-secondary); --surface-card:var(--d-card); --surface-hover:var(--d-hover);
  --border-line:var(--d-line); --border-line-subtle:var(--d-line-subtle);
  --text-primary:var(--d-text); --text-secondary:var(--d-text2); --text-tertiary:var(--d-text3);
  --status-running:var(--green-500); --status-idle:var(--slate-500); --status-failed:var(--red-500);
  --status-stopped:var(--red-500); --status-attention:var(--amber-500); --text-on-attention:#1A1200;
  --sidebar-bg:var(--d-sidebar-vibrancy); --sidebar-bg-solid:var(--d-sidebar-solid);
}
:root[data-theme="light"] {
  --color-brand:var(--blue-500); --color-brand-strong:var(--blue-700);
  --color-brand-soft:var(--blue-soft); --color-accent-soft:var(--coral-soft);
  --color-accent-strong:var(--coral-600);
  --surface-bg:var(--n-bg); --surface-window:var(--n-bg); --surface-sidebar:var(--n-sidebar);
  --surface-list:var(--n-list); --surface-detail:var(--n-detail);
  --surface-secondary:var(--n-secondary); --surface-card:var(--n-card); --surface-hover:var(--n-hover);
  --border-line:var(--n-line); --border-line-subtle:var(--n-line-subtle);
  --text-primary:var(--n-text); --text-secondary:var(--n-text2); --text-tertiary:var(--n-text3);
  --status-running:var(--green-600); --status-idle:var(--slate-400); --status-failed:var(--red-600);
  --status-stopped:var(--red-600); --status-attention:var(--amber-500); --text-on-attention:#1A1200;
  --sidebar-bg:var(--n-sidebar-vibrancy); --sidebar-bg-solid:var(--n-sidebar-solid);
}
```

- [x] Recolor edits (exact old → new):
  - `src/styles/components/dashboard.css` `.dashboard` (line 25): replace the `background:` radial-gradient declaration with `background: var(--surface-window);`.
  - `src/styles/components/dashboard.css` `.dashboard__hero` (line 243): `background: var(--surface-card); border-radius: var(--radius-lg); box-shadow: none;` (replace the gradient, `--radius-xl`, `--shadow-1` lines).
  - `src/styles/components/dashboard.css` `.grid-card` (line 346): `border-radius: var(--radius-lg);`.
  - `src/styles/components/shell.css` `.shell-sidebar-item--active`: `background: var(--color-brand-soft); color: var(--color-brand-strong);` and `.shell-sidebar-item--active .shell-sidebar-item__badge { background: var(--status-attention); color: var(--text-on-attention); }`; `.shell-sidebar-item__badge` base: `background: var(--status-attention); color: var(--text-on-attention);`.
  - `src/styles/components/detail-panel.css` `.detail-panel__header` (line 23): `background: var(--surface-card);`.
  - `src/styles/components/primitives.css` line 5: `.btn--primary { background:var(--color-brand); color:var(--text-on-brand); }` (no glow). Add after it: `.btn--accent { background:var(--color-accent); color:#fff; }` — the coral CTA, used once per screen (spec §5.1).
  - `src/components/agents/AgentsPage.tsx` line 137 (empty-state CTA): `className="btn btn--primary"` → `className="btn btn--accent"`. Same for `src/components/home/HomePage.tsx` line 62 (`home.quick.newChat`).
- [x] `npm run build` → `✓ built in`. Preview light and dark: no light-blue page tint, sidebar active item is a translucent blue tint, primary buttons are blue, the two empty-state CTAs are coral.
- [x] Commit: `style(hub): neutral-first tokens, radius aliases, brand-blue primary`

**Interfaces — Produces:** tokens `--surface-window`, `--surface-sidebar`, `--surface-list`, `--surface-detail`, `--border-line-subtle`, `--font-mono`, `--shell-sidebar-collapsed-width`, `--shell-list-width`, `--shell-list-width-compact`, `--shell-list-min`, `--shell-list-max`, `--shell-titlebar-inset`; class `.btn--accent`.

### Task 1.3 — Adopt the status vocabulary everywhere

- [x] `src/components/agents/AgentsPage.tsx` `ListRow` (lines 18–64): delete `const pill = runtimePill(runtime?.state);` and replace the `<span className={pill.cls}>…</span>` block (lines 42–45) with `<StatusPill kind={statusOf(runtime?.state)} />`. Import `{ StatusPill, statusOf } from "../shell/Status"`; drop `runtimePill` from the `../../utils` import.
- [x] `src/components/agents/GridCard.tsx`: line 56 `const pill = runtimePill(runtime?.state);` → delete; find the render that uses `pill.cls` (`grep -n 'pill\.' src/components/agents/GridCard.tsx`) and replace that `<span className={pill.cls}>…</span>` with `<StatusPill kind={statusOf(runtime?.state)} />`; fix imports as above.
- [x] `src/components/inspector/AgentInspector.tsx`: line 146 `const statusPill = runtimePill(runtime?.state);` → delete; lines 197–200 → `<StatusPill kind={statusOf(runtime?.state)} />`; fix imports.
- [x] `src/components/fleet/FleetDetail.tsx`: add `running: boolean;` to `Props` (after `jobs`); delete `statusPillClass` and `statusLabel` (lines 55–66); line 372 → `<StatusPill kind={fleetStatusOf({ stopped: detail.stopped, running })} />`; destructure `running` in the component signature. In `src/components/fleet/FleetView.tsx` pass `running={fleets.find((f) => f.name === detail.name)?.running ?? false}` next to `fleetLabels=`.
- [x] `src/components/fleet/FleetRail.tsx`: delete `statusClass` (lines 16–20); line 87 `<span className={\`fleet-rail__status ${statusClass(f)}\`} />` → `<StatusDot kind={fleetStatusOf(f)} />`. Delete the `.fleet-rail__status*` rules from `src/styles/components/fleet.css`.
- [x] `src/utils.ts`: delete `runtimePill` (lines 60–73). `src/styles/components/primitives.css`: delete `.pill`, `.pill__dot`, `.pill--run`, `.pill--idle`, `.pill--fail` (lines 20–27). Delete `.fleet-detail__status-pill*` rules from `fleet.css`.
- [x] `command grep -rn 'runtimePill\|pill--\|pill__dot\|fleet-rail__status' src` → no output.
- [x] `npm test` → all pass. `npm run build`, `npm run lint` → clean.
- [x] Commit: `refactor(hub): every surface renders status through StatusPill/StatusDot`

**Manual acceptance PR 1** (light + dark): Agents grid/list, agent inspector header, Fleets rail dots, fleet detail header all show the same dot/pill family; a `.stopped` fleet is red "stopped"; a stopped agent is grey "idle".

---

## PR 2 — Shell: breakpoints, title bar, divider, banners slot

Branch `feat/hub-2-shell`.

### Task 2.1 — `persist.ts` and `breakpoints.ts`

- [ ] Write `src/components/shell/breakpoints.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import {
  BP_COMPACT, BP_WIDE, SIDEBAR_PREF_KEY, listModeFor, readSidebarPref,
  sidebarModeFor, togglePref, writeSidebarPref,
} from "./breakpoints";

function mem(): Pick<Storage, "getItem" | "setItem"> & { data: Record<string, string> } {
  const data: Record<string, string> = {};
  return { data, getItem: (k) => data[k] ?? null, setItem: (k, v) => { data[k] = v; } };
}

describe("sidebarModeFor", () => {
  it("auto follows the width", () => {
    expect(sidebarModeFor(BP_WIDE, "auto")).toBe("expanded");
    expect(sidebarModeFor(BP_WIDE - 1, "auto")).toBe("collapsed");
    expect(sidebarModeFor(BP_COMPACT - 1, "auto")).toBe("collapsed");
  });
  it("a pin wins over the width", () => {
    expect(sidebarModeFor(800, "expanded")).toBe("expanded");
    expect(sidebarModeFor(2000, "collapsed")).toBe("collapsed");
  });
});

describe("listModeFor", () => {
  it("three bands", () => {
    expect(listModeFor(BP_WIDE)).toBe("wide");
    expect(listModeFor(BP_COMPACT)).toBe("compact");
    expect(listModeFor(BP_COMPACT - 1)).toBe("overlay");
  });
});

describe("togglePref", () => {
  it("toggles relative to what is shown, and returns to auto when the pin matches auto", () => {
    expect(togglePref("auto", 1400)).toBe("collapsed");
    expect(togglePref("collapsed", 1400)).toBe("auto");
    expect(togglePref("auto", 1000)).toBe("expanded");
    expect(togglePref("expanded", 1000)).toBe("auto");
  });
});

describe("pref persistence", () => {
  it("round-trips and defaults to auto on junk", () => {
    const s = mem();
    expect(readSidebarPref(s)).toBe("auto");
    writeSidebarPref(s, "collapsed");
    expect(s.data[SIDEBAR_PREF_KEY]).toBe("collapsed");
    expect(readSidebarPref(s)).toBe("collapsed");
    s.data[SIDEBAR_PREF_KEY] = "sideways";
    expect(readSidebarPref(s)).toBe("auto");
  });
});
```

- [ ] Run it → `FAIL … Failed to resolve import "./breakpoints"`.
- [ ] Create `src/components/shell/persist.ts`:

```ts
/** localStorage wrappers that never throw (private mode, quota, no window). */
export function readKey(key: string): string | null {
  try {
    return typeof localStorage === "undefined" ? null : localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeKey(key: string, value: string | null): void {
  try {
    if (typeof localStorage === "undefined") return;
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    /* private mode / quota — the value simply does not persist this session */
  }
}
```

- [ ] Create `src/components/shell/breakpoints.ts`:

```ts
// Shell breakpoints (spec §3.1). These are the ONLY numbers that decide the
// layout; the CSS custom properties carry the matching widths.
export const BP_WIDE = 1200;
export const BP_COMPACT = 960;
export const SIDEBAR_PREF_KEY = "mur.shell.sidebar";

export type SidebarPref = "auto" | "expanded" | "collapsed";
export type SidebarMode = "expanded" | "collapsed";
export type ListMode = "wide" | "compact" | "overlay";

export function sidebarModeFor(width: number, pref: SidebarPref): SidebarMode {
  if (pref !== "auto") return pref;
  return width >= BP_WIDE ? "expanded" : "collapsed";
}

export function listModeFor(width: number): ListMode {
  if (width >= BP_WIDE) return "wide";
  if (width >= BP_COMPACT) return "compact";
  return "overlay";
}

/** ⌘\ toggles what is currently shown; a pin equal to the auto result
 *  collapses back to `auto` so a window resize takes over again. */
export function togglePref(current: SidebarPref, width: number): SidebarPref {
  const shown = sidebarModeFor(width, current);
  const next: SidebarMode = shown === "expanded" ? "collapsed" : "expanded";
  return next === sidebarModeFor(width, "auto") ? "auto" : next;
}

export function readSidebarPref(storage: Pick<Storage, "getItem">): SidebarPref {
  try {
    const v = storage.getItem(SIDEBAR_PREF_KEY);
    return v === "expanded" || v === "collapsed" ? v : "auto";
  } catch {
    return "auto";
  }
}

export function writeSidebarPref(storage: Pick<Storage, "setItem">, pref: SidebarPref): void {
  try {
    storage.setItem(SIDEBAR_PREF_KEY, pref);
  } catch {
    /* private mode / quota */
  }
}
```

- [ ] Run the test → pass. Commit: `feat(hub): shell breakpoints and sidebar pref persistence`

**Interfaces — Produces:** `BP_WIDE`, `BP_COMPACT`, `SidebarPref`, `SidebarMode`, `ListMode`, `sidebarModeFor`, `listModeFor`, `togglePref`, `readSidebarPref`, `writeSidebarPref`, `readKey`, `writeKey`.

### Task 2.2 — Sidebar collapse and ⌘\

- [ ] Append to `src/components/shell/shell.test.ts`:

```ts
import { isSidebarToggle } from "./Shell";

describe("isSidebarToggle", () => {
  const base = { key: "\\", metaKey: true, altKey: false, ctrlKey: false, shiftKey: false };
  it("matches meta+backslash", () => {
    expect(isSidebarToggle(base as KeyboardEvent)).toBe(true);
  });
  it("rejects extra modifiers and other keys", () => {
    expect(isSidebarToggle({ ...base, altKey: true } as KeyboardEvent)).toBe(false);
    expect(isSidebarToggle({ ...base, key: "/" } as KeyboardEvent)).toBe(false);
  });
});
```

- [ ] Create `src/components/shell/useWindowWidth.ts`:

```ts
import { useEffect, useState } from "react";

export function useWindowWidth(): number {
  const [width, setWidth] = useState(() => (typeof window === "undefined" ? 0 : window.innerWidth));
  useEffect(() => {
    function onResize() {
      setWidth(window.innerWidth);
    }
    window.addEventListener("resize", onResize);
    return () => window.removeEventListener("resize", onResize);
  }, []);
  return width;
}
```

- [ ] Create `src/components/shell/platform.ts`:

```ts
/** macOS gets the overlay title bar (traffic lights inside the sidebar);
 *  other platforms keep native decorations, so their inset is 0. */
export function isMac(): boolean {
  return typeof navigator !== "undefined" && /Mac/.test(navigator.platform);
}
```

- [ ] Replace `src/components/shell/Shell.tsx` with:

```tsx
import { useEffect, useState, type ReactNode } from "react";
import { Sidebar } from "./Sidebar";
import type { PageId } from "./nav";
import {
  readSidebarPref, sidebarModeFor, togglePref, writeSidebarPref, type SidebarPref,
} from "./breakpoints";
import { useWindowWidth } from "./useWindowWidth";
import { isMac } from "./platform";

// ⌘⌥I toggles the inspector column, independent of whether the caller
// currently provides an `inspector` node. No other modifiers may be held.
export function isInspectorToggle(e: KeyboardEvent): boolean {
  return e.metaKey && e.altKey && !e.ctrlKey && !e.shiftKey && e.key.toLowerCase() === "i";
}

/** ⌘\ toggles the sidebar between labels and the icon rail (spec §3.1). */
export function isSidebarToggle(e: KeyboardEvent): boolean {
  return e.metaKey && !e.altKey && !e.ctrlKey && !e.shiftKey && e.key === "\\";
}

function initialPref(): SidebarPref {
  return typeof localStorage === "undefined" ? "auto" : readSidebarPref(localStorage);
}

export interface ShellProps {
  page: PageId;
  onNavigate: (id: PageId) => void;
  badge: number;
  inspector?: ReactNode;
  /** Banners render at the top of the content column, never above the sidebar. */
  banners?: ReactNode;
  onSettings: () => void;
  children: ReactNode;
}

export function Shell({ page, onNavigate, badge, inspector, banners, onSettings, children }: ShellProps) {
  const [inspectorVisible, setInspectorVisible] = useState(true);
  const [pref, setPref] = useState<SidebarPref>(initialPref);
  const width = useWindowWidth();
  const sidebarMode = sidebarModeFor(width, pref);

  useEffect(() => {
    function onKeyDown(e: KeyboardEvent) {
      if (isInspectorToggle(e)) {
        e.preventDefault();
        setInspectorVisible((v) => !v);
        return;
      }
      if (isSidebarToggle(e)) {
        e.preventDefault();
        setPref((p) => {
          const next = togglePref(p, window.innerWidth);
          writeSidebarPref(localStorage, next);
          return next;
        });
      }
    }
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  const showInspector = inspector !== undefined && inspectorVisible;
  const cls = [
    "shell",
    showInspector ? "shell--with-inspector" : "",
    sidebarMode === "collapsed" ? "shell--sidebar-collapsed" : "",
    isMac() ? "shell--titlebar-inset" : "",
  ].filter(Boolean).join(" ");

  return (
    <div className={cls}>
      <div className="shell__sidebar" data-tauri-drag-region>
        <Sidebar
          active={page}
          badge={badge}
          onSelect={onNavigate}
          collapsed={sidebarMode === "collapsed"}
          onSettings={onSettings}
        />
      </div>
      <div className="shell__content">
        {banners}
        <div className="shell__page">{children}</div>
      </div>
      {showInspector && <div className="shell__inspector">{inspector}</div>}
    </div>
  );
}
```

- [ ] `src/components/shell/Sidebar.tsx`: add `collapsed: boolean; onSettings: () => void;` to `SidebarProps`; add `title={t(labelKey)}` to the item `<button>`; add a gear glyph and a footer. Replace the `return (…)` block with:

```tsx
  return (
    <nav className={`shell-sidebar${collapsed ? " shell-sidebar--collapsed" : ""}`} aria-label="Primary">
      <div className="shell-sidebar__group">
        <div className="shell-sidebar__group-label">{t("nav.groupWorkspace")}</div>
        {workspace.map((i) => renderItem(i.id, i.labelKey))}
      </div>
      <div className="shell-sidebar__group">
        <div className="shell-sidebar__group-label">{t("nav.groupLibrary")}</div>
        {library.map((i) => renderItem(i.id, i.labelKey))}
      </div>
      <div className="shell-sidebar__footer">
        <button type="button" className="shell-sidebar-item" onClick={onSettings} title={t("app.settings")}>
          <span className="shell-sidebar-item__icon"><Ico>{GEAR}</Ico></span>
          <span className="shell-sidebar-item__label">{t("app.settings")}</span>
        </button>
        {version && <div className="shell-sidebar__version">{t("shell.version", { version })}</div>}
      </div>
    </nav>
  );
```
  with, above the component, `const GEAR = (<><circle cx="12" cy="12" r="3" /><path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9L17 7M7 17l-2.1 2.1" /></>);` and, inside the component, the version read copied from `src/components/settings/AboutSettings.tsx` line 16: `const [version, setVersion] = useState<string | null>(null); useEffect(() => { getVersion().then(setVersion).catch(() => {}); }, []);` (`import { getVersion } from "@tauri-apps/api/app"`). Remove the two `as TranslationKey` casts — the keys exist (`en.ts` lines 848–849).
- [ ] i18n: `"shell.version": "MUR Hub {version}"` in both tables.
- [ ] `src/styles/components/shell.css`: replace the `.shell`, `.shell--with-inspector`, `.shell__content` rules with:

```css
.shell {
  display: grid;
  grid-template-columns: var(--shell-sidebar-width) 1fr;
  flex: 1; min-height: 0; width: 100%;
  font-family: var(--font-sans); font-size: var(--text-base);
  color: var(--text-primary); background: var(--surface-window);
}
.shell--sidebar-collapsed { grid-template-columns: var(--shell-sidebar-collapsed-width) 1fr; }
.shell--with-inspector { grid-template-columns: var(--shell-sidebar-width) 1fr var(--shell-inspector-width); }
.shell--sidebar-collapsed.shell--with-inspector {
  grid-template-columns: var(--shell-sidebar-collapsed-width) 1fr var(--shell-inspector-width);
}
.shell--titlebar-inset .shell__sidebar { padding-top: var(--shell-titlebar-inset); }
.shell__content { display: flex; flex-direction: column; min-width: 0; overflow: hidden; }
.shell__page { flex: 1; min-height: 0; overflow: auto; display: flex; flex-direction: column; }
.shell__page > * { flex: 1; min-height: 0; }
```
  and append:

```css
.shell-sidebar--collapsed .shell-sidebar__group-label,
.shell-sidebar--collapsed .shell-sidebar-item__label,
.shell-sidebar--collapsed .shell-sidebar__version { display: none; }
.shell-sidebar--collapsed .shell-sidebar-item {
  position: relative; justify-content: center; width: 40px; padding: 7px 0; margin: 0 auto;
}
.shell-sidebar--collapsed .shell-sidebar-item__badge {
  position: absolute; top: 2px; right: 2px; min-width: 14px; height: 14px; padding: 0 4px; font-size: 9.5px;
}
.shell-sidebar__footer { margin-top: auto; display: flex; flex-direction: column; gap: 2px; }
.shell-sidebar__version { font-size: var(--text-xs); color: var(--text-tertiary); padding: 4px 10px 0; }
@media (prefers-reduced-motion: no-preference) {
  .shell { transition: grid-template-columns var(--dur-base) var(--ease-out); }
}
```
  Also `.shell__sidebar { background: var(--surface-sidebar); … }` keeps the vibrancy line as is.
- [ ] `src/components/DashboardApp.tsx`: pass `onSettings={() => setSettingsOpen(true)}` to `<Shell>`. Leave the gear in the global bar for now (removed in PR 4).
- [ ] `npm test`, `npm run build` → green. Preview: at 1200+ the sidebar shows labels; drag the window under 1200 → icon rail; ⌘\ pins; relaunch keeps the pin.
- [ ] Commit: `feat(hub): sidebar collapses by breakpoint, ⌘\ pins it, Settings moves to the sidebar footer`

**Interfaces — Produces:** `Shell` props `banners?`, `onSettings`; `Sidebar` props `collapsed`, `onSettings`; classes `.shell--sidebar-collapsed`, `.shell--titlebar-inset`, `.shell__page`, `.shell-sidebar--collapsed`.

### Task 2.3 — Overlay title bar, window size, no auto-resize

- [ ] `mur-hub-gui/src-tauri/tauri.conf.json`: the `dashboard` window becomes

```json
      {
        "label": "dashboard",
        "title": "MUR Hub",
        "url": "index.html",
        "width": 1200,
        "height": 760,
        "minWidth": 900,
        "minHeight": 560,
        "resizable": true,
        "fullscreen": false,
        "visible": false,
        "titleBarStyle": "Overlay",
        "hiddenTitle": true
      },
```
- [ ] `mur-hub-gui/src-tauri/capabilities/default.json`: if `"core:window:allow-start-dragging"` is not in `permissions`, add it (the sidebar is a drag region). Do not remove `allow-set-size`: other windows share this capability.
- [ ] `src/components/DashboardApp.tsx`: delete the auto-resize block — from the comment `// Auto-resize the window when the contextual inspector opens/closes` through the closing `}, [inspectorOpen]);` (lines 239–268), including the `const inspectorOpen = hasInspector(…)` it contains. Then `command grep -n 'getCurrentWindow\|currentMonitor\|LogicalSize' src/components/DashboardApp.tsx`; when only the import (line 4) remains, delete it.
- [ ] `command grep -rn 'setSize\|setMinSize' src` → only files outside `DashboardApp.tsx` (Panel/Pet own windows), otherwise the task is not done.
- [ ] `npm run build`. Preview in the Tauri dev window: traffic lights sit inside the sidebar's top inset; the sidebar drags the window; clicking an agent no longer resizes the window.
- [ ] Commit: `feat(hub): overlay title bar, 1200×760 default window, no auto-resize on selection`

### Task 2.4 — `useResizableColumn` and `ListDivider`

- [ ] Write `src/components/shell/useResizableColumn.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { clampWidth, parseStoredWidth } from "./useResizableColumn";

describe("clampWidth", () => {
  it("rounds and clamps", () => {
    expect(clampWidth(239.6, 240, 400)).toBe(240);
    expect(clampWidth(1000, 240, 400)).toBe(400);
    expect(clampWidth(300.4, 240, 400)).toBe(300);
  });
});

describe("parseStoredWidth", () => {
  it("falls back on junk and clamps stored values", () => {
    expect(parseStoredWidth(null, 300, 240, 400)).toBe(300);
    expect(parseStoredWidth("abc", 300, 240, 400)).toBe(300);
    expect(parseStoredWidth("9999", 300, 240, 400)).toBe(400);
    expect(parseStoredWidth("260", 300, 240, 400)).toBe(260);
  });
});
```

- [ ] Create `src/components/shell/useResizableColumn.ts`:

```ts
import { useCallback, useRef, useState, type PointerEvent as ReactPointerEvent } from "react";
import { readKey, writeKey } from "./persist";

export function clampWidth(w: number, min: number, max: number): number {
  return Math.min(max, Math.max(min, Math.round(w)));
}

export function parseStoredWidth(raw: string | null, fallback: number, min: number, max: number): number {
  const n = raw === null ? Number.NaN : Number(raw);
  return Number.isFinite(n) ? clampWidth(n, min, max) : fallback;
}

export interface ResizableColumn {
  width: number;
  onPointerDown: (e: ReactPointerEvent<HTMLElement>) => void;
  /** Double-click on the divider: back to the default width. */
  reset: () => void;
}

/** A pointer-dragged column width, clamped to [min, max] and persisted under
 *  `storageKey` on release (spec §3.1). */
export function useResizableColumn(storageKey: string, fallback: number, min: number, max: number): ResizableColumn {
  const [width, setWidth] = useState(() => parseStoredWidth(readKey(storageKey), fallback, min, max));
  const drag = useRef<{ startX: number; startW: number } | null>(null);

  const onPointerDown = useCallback(
    (e: ReactPointerEvent<HTMLElement>) => {
      if (e.button !== 0) return;
      e.preventDefault();
      const target = e.currentTarget;
      target.setPointerCapture(e.pointerId);
      drag.current = { startX: e.clientX, startW: width };
      function onMove(ev: PointerEvent) {
        if (!drag.current) return;
        setWidth(clampWidth(drag.current.startW + (ev.clientX - drag.current.startX), min, max));
      }
      function onUp() {
        drag.current = null;
        target.removeEventListener("pointermove", onMove);
        target.removeEventListener("pointerup", onUp);
        setWidth((w) => {
          writeKey(storageKey, String(w));
          return w;
        });
      }
      target.addEventListener("pointermove", onMove);
      target.addEventListener("pointerup", onUp);
    },
    [width, min, max, storageKey],
  );

  const reset = useCallback(() => {
    setWidth(fallback);
    writeKey(storageKey, null);
  }, [fallback, storageKey]);

  return { width, onPointerDown, reset };
}
```

- [ ] Create `src/components/shell/ListDivider.tsx`:

```tsx
import type { ResizableColumn } from "./useResizableColumn";

export function ListDivider({ column, label }: { column: ResizableColumn; label: string }) {
  return (
    <div
      className="list-divider"
      role="separator"
      aria-orientation="vertical"
      aria-label={label}
      title={label}
      onPointerDown={column.onPointerDown}
      onDoubleClick={column.reset}
    />
  );
}
```

- [ ] Append to `src/styles/components/shell.css`:

```css
/* Master–detail page layout (spec §3.1): list | divider | detail. The page
   sets --md-list-width from useResizableColumn. */
.master-detail {
  display: grid; grid-template-columns: var(--md-list-width, var(--shell-list-width)) 6px 1fr;
  height: 100%; min-height: 0; overflow: hidden;
}
.list-divider { position: relative; cursor: col-resize; background: transparent; }
.list-divider::after {
  content: ""; position: absolute; top: 0; bottom: 0; left: 2px; width: 1px; background: var(--border-line);
}
.list-divider:hover::after, .list-divider:active::after { width: 2px; background: var(--color-brand); }
```
- [ ] i18n: `"shell.resizeList": "Drag to resize the list · double-click to reset"` (zh-TW: `"拖曳調整清單寬度 · 按兩下還原"`).
- [ ] Test → pass. Commit: `feat(hub): draggable, persisted list divider (unused until the Agents page adopts it)`

**Interfaces — Produces:** `useResizableColumn(storageKey, fallback, min, max): ResizableColumn`, `<ListDivider column label>`, class `.master-detail` reading `--md-list-width`.

### Task 2.5 — Banners into the content column

- [ ] In `src/components/DashboardApp.tsx`, cut the four banner blocks (`{showAppsBanner && …}` through the `{cliSkew && …}` block, lines 362–455) out of the JSX and, above the `return (`, define:

```tsx
  const banners = (
    <>
      {/* the four blocks, unchanged, pasted here */}
    </>
  );
```
  Pass `banners={banners}` to `<Shell>`.
- [ ] `npm run build`. Preview: trigger the CLI-skew banner (or temporarily force `cliSkew` in devtools) — it renders above the page content, the sidebar does not move.
- [ ] Commit: `refactor(hub): banners render in the content column, not above the shell`

**Manual acceptance PR 2:** 1200 and 960 widths × light/dark; ⌘\ pin survives relaunch; traffic lights inside the sidebar; window never resizes on selection; banners never push the sidebar down; Settings opens from the sidebar footer.

---

## PR 3 — Component family

Branch `feat/hub-2-components`. Nothing adopts these yet except the command palette, which is global.

### Task 3.1 — `SourceList`

- [ ] Write `src/components/shell/sourceList.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { filterRows, moveSelection } from "./sourceList";

const rows = [
  { id: "aura", name: "AURA", subtitle: "Engineer · claude-sonnet", facets: ["Engineer"] },
  { id: "scout", name: "Scout", subtitle: "Research", facets: ["Research"] },
  { id: "muse", name: "Muse", subtitle: undefined, facets: ["__none__"] },
];

describe("filterRows", () => {
  it("text matches name or subtitle, case-insensitive", () => {
    expect(filterRows(rows, "sonnet", null).map((r) => r.id)).toEqual(["aura"]);
    expect(filterRows(rows, "SCOUT", null).map((r) => r.id)).toEqual(["scout"]);
  });
  it("facet and text compose", () => {
    expect(filterRows(rows, "", "Research").map((r) => r.id)).toEqual(["scout"]);
    expect(filterRows(rows, "a", "Engineer").map((r) => r.id)).toEqual(["aura"]);
  });
});

describe("moveSelection", () => {
  it("steps within bounds and enters from either end", () => {
    expect(moveSelection(rows, null, 1)).toBe("aura");
    expect(moveSelection(rows, null, -1)).toBe("muse");
    expect(moveSelection(rows, "aura", 1)).toBe("scout");
    expect(moveSelection(rows, "muse", 1)).toBe("muse");
    expect(moveSelection([], "x", 1)).toBeNull();
  });
});
```

- [ ] Create `src/components/shell/sourceList.ts`:

```ts
import type { ReactNode } from "react";
import type { StatusKind } from "./Status";

export interface SourceRowData {
  id: string;
  name: string;
  subtitle?: string;
  status: StatusKind;
  /** Amber "needs you" count; 0 or undefined hides the badge. */
  needsYou?: number;
  avatar: ReactNode;
  /** Facet ids this row belongs to (a role, label ids, …). */
  facets: string[];
}

export interface SourceFacet {
  id: string;
  label: string;
  count: number;
}

export function filterRows<T extends { name: string; subtitle?: string; facets: string[] }>(
  rows: T[],
  text: string,
  facet: string | null,
): T[] {
  const q = text.trim().toLowerCase();
  return rows.filter(
    (r) =>
      (facet === null || r.facets.includes(facet)) &&
      (!q || r.name.toLowerCase().includes(q) || (r.subtitle ?? "").toLowerCase().includes(q)),
  );
}

export function moveSelection<T extends { id: string }>(rows: T[], selectedId: string | null, delta: 1 | -1): string | null {
  if (rows.length === 0) return null;
  const i = rows.findIndex((r) => r.id === selectedId);
  if (i === -1) return delta === 1 ? rows[0].id : rows[rows.length - 1].id;
  return rows[Math.min(rows.length - 1, Math.max(0, i + delta))].id;
}
```

- [ ] Create `src/components/shell/SourceList.tsx`:

```tsx
import { useEffect, useRef, type KeyboardEvent, type ReactNode } from "react";
import { NeedsYouBadge, StatusDot } from "./Status";
import { filterRows, moveSelection, type SourceFacet, type SourceRowData } from "./sourceList";

/** ⌘F focuses this list's filter field. One SourceList is mounted per page. */
export function isFilterShortcut(e: globalThis.KeyboardEvent): boolean {
  return (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "f";
}

export interface SourceListProps {
  title: string;
  count: number;
  rows: SourceRowData[];
  facets: SourceFacet[];
  allLabel: string;
  activeFacet: string | null;
  onFacet: (id: string | null) => void;
  filter: string;
  onFilter: (q: string) => void;
  filterPlaceholder: string;
  selectedId: string | null;
  onSelect: (id: string | null) => void;
  onCreate: () => void;
  createLabel: string;
  emptyState: ReactNode;
}

export function SourceList(p: SourceListProps) {
  const visible = filterRows(p.rows, p.filter, p.activeFacet);
  const filterRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    function onKey(e: globalThis.KeyboardEvent) {
      if (isFilterShortcut(e)) {
        e.preventDefault();
        filterRef.current?.focus();
      }
    }
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

  function onListKey(e: KeyboardEvent<HTMLDivElement>) {
    if (e.key === "ArrowDown" || e.key === "ArrowUp") {
      e.preventDefault();
      p.onSelect(moveSelection(visible, p.selectedId, e.key === "ArrowDown" ? 1 : -1));
    } else if (e.key === "Escape") {
      p.onSelect(null);
    }
  }

  return (
    <section className="source-list" aria-label={p.title}>
      <header className="source-list__head">
        <h2 className="source-list__title">
          {p.title} <span className="source-list__count">{p.count}</span>
        </h2>
        <button type="button" className="source-list__create" onClick={p.onCreate} title={p.createLabel} aria-label={p.createLabel}>
          +
        </button>
      </header>
      <input
        ref={filterRef}
        className="source-list__filter"
        type="search"
        value={p.filter}
        placeholder={p.filterPlaceholder}
        onChange={(e) => p.onFilter(e.target.value)}
      />
      {p.facets.length > 0 && (
        <div className="source-list__chips" role="group">
          <button type="button" className={`chip${p.activeFacet === null ? " chip--on" : ""}`} onClick={() => p.onFacet(null)}>
            {p.allLabel} <i>{p.count}</i>
          </button>
          {p.facets.map((f) => (
            <button
              key={f.id}
              type="button"
              className={`chip${p.activeFacet === f.id ? " chip--on" : ""}`}
              onClick={() => p.onFacet(p.activeFacet === f.id ? null : f.id)}
            >
              {f.label} <i>{f.count}</i>
            </button>
          ))}
        </div>
      )}
      <div
        className="source-list__rows"
        role="listbox"
        tabIndex={0}
        aria-activedescendant={p.selectedId ? `row-${p.selectedId}` : undefined}
        onKeyDown={onListKey}
      >
        {visible.length === 0
          ? p.emptyState
          : visible.map((r) => (
              <div
                key={r.id}
                id={`row-${r.id}`}
                role="option"
                aria-selected={r.id === p.selectedId}
                className={`source-row${r.id === p.selectedId ? " source-row--on" : ""}`}
                onClick={() => p.onSelect(r.id)}
              >
                <span className="source-row__avatar">{r.avatar}</span>
                <span className="source-row__text">
                  <span className="source-row__name">{r.name}</span>
                  {r.subtitle && <span className="source-row__sub">{r.subtitle}</span>}
                </span>
                <span className="source-row__status">
                  <NeedsYouBadge count={r.needsYou ?? 0} />
                  <StatusDot kind={r.status} />
                </span>
              </div>
            ))}
      </div>
    </section>
  );
}
```

- [ ] Write `src/components/shell/SourceList.test.tsx`:

```tsx
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SourceList } from "./SourceList";

const noop = () => {};
const rows = [
  { id: "aura", name: "AURA", subtitle: "Engineer", status: "running" as const, needsYou: 1, avatar: "A", facets: ["Engineer"] },
  { id: "scout", name: "Scout", status: "idle" as const, avatar: "S", facets: ["Research"] },
];

describe("SourceList markup", () => {
  it("marks the selected row and renders the needs-you badge", () => {
    const html = renderToStaticMarkup(
      <SourceList title="Agents" count={2} rows={rows} facets={[{ id: "Engineer", label: "Engineer", count: 1 }]}
        allLabel="All" activeFacet={null} onFacet={noop} filter="" onFilter={noop} filterPlaceholder="Filter"
        selectedId="aura" onSelect={noop} onCreate={noop} createLabel="New" emptyState={<p>none</p>} />,
    );
    expect(html).toContain('id="row-aura"');
    expect(html).toContain("source-row--on");
    expect(html).toContain('class="needs-you"');
    expect(html).toContain("status-dot--idle");
  });
  it("shows the empty state when the filter matches nothing", () => {
    const html = renderToStaticMarkup(
      <SourceList title="Agents" count={2} rows={rows} facets={[]} allLabel="All" activeFacet={null} onFacet={noop}
        filter="zzz" onFilter={noop} filterPlaceholder="Filter" selectedId={null} onSelect={noop} onCreate={noop}
        createLabel="New" emptyState={<p>none</p>} />,
    );
    expect(html).toContain("<p>none</p>");
  });
});
```

- [ ] Create `src/styles/components/source-list.css` and import it in `index.css` after `shell.css`:

```css
/* SourceList (spec §4.1). Rows are 13px; the selected row is a translucent brand tint. */
.source-list { display: flex; flex-direction: column; min-width: 0; height: 100%; background: var(--surface-list); }
.source-list__head { display: flex; align-items: center; gap: 8px; padding: var(--space-6) var(--space-5) var(--space-4); }
.source-list__title { margin: 0; font-size: var(--text-md); font-weight: var(--fw-semi); letter-spacing: -.01em; }
.source-list__count { color: var(--text-tertiary); font-weight: var(--fw-regular); font-variant-numeric: tabular-nums; }
.source-list__create {
  margin-left: auto; width: 26px; height: 26px; display: grid; place-items: center; border-radius: var(--radius-sm);
  border: 1px solid var(--border-line-subtle); background: var(--surface-card); color: var(--text-secondary); cursor: pointer;
}
.source-list__create:hover { color: var(--text-primary); background: var(--surface-hover); }
.source-list__filter {
  margin: 0 var(--space-5) var(--space-4); height: 28px; padding: 0 var(--space-4); border-radius: var(--radius-sm);
  border: 1px solid var(--border-line-subtle); background: var(--surface-secondary); color: var(--text-primary); font: inherit; font-size: var(--text-sm);
}
.source-list__chips { display: flex; flex-wrap: wrap; gap: 6px; padding: 0 var(--space-5) var(--space-5); }
.chip {
  height: 22px; padding: 0 9px; border-radius: var(--radius-full); border: 1px solid var(--border-line);
  background: transparent; color: var(--text-secondary); font: inherit; font-size: var(--text-xs); cursor: pointer;
  display: inline-flex; align-items: center; gap: 5px; white-space: nowrap;
}
.chip i { font-style: normal; color: var(--text-tertiary); }
.chip--on { background: var(--text-primary); color: var(--surface-window); border-color: transparent; }
.chip--on i { color: inherit; opacity: .7; }
.source-list__rows { flex: 1; overflow: auto; padding: 0 var(--space-4) var(--space-4); display: flex; flex-direction: column; gap: 1px; outline: none; }
.source-list__rows:focus-visible { box-shadow: inset var(--shadow-focus); }
.source-row {
  display: grid; grid-template-columns: 28px 1fr auto; gap: 10px; align-items: center;
  padding: 7px var(--space-4); border-radius: var(--radius-md); cursor: pointer; min-width: 0; font-size: 13px;
}
.source-row:hover { background: var(--surface-hover); }
.source-row--on, .source-row--on:hover { background: var(--color-brand-soft); }
.source-row__avatar { display: flex; width: 28px; height: 28px; }
.source-row__text { min-width: 0; display: flex; flex-direction: column; }
.source-row__name { font-weight: 500; white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.source-row__sub { font-size: var(--text-xs); color: var(--text-secondary); white-space: nowrap; overflow: hidden; text-overflow: ellipsis; }
.source-row__status { display: flex; align-items: center; gap: 8px; }
```

- [ ] Run both tests → pass. Commit: `feat(hub): SourceList — the list pane the Agents, Fleets and Library pages share`

**Interfaces — Produces:** `SourceRowData`, `SourceFacet`, `filterRows`, `moveSelection`, `<SourceList …>` (props above), `isFilterShortcut`; CSS `.source-list*`, `.source-row*`, `.chip`, `.chip--on`.

### Task 3.2 — `DetailPage` and the dirty guard

- [ ] Write `src/components/shell/DetailPage.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { nextTab } from "./DetailPage";

const tabs = [{ id: "a", label: "A" }, { id: "b", label: "B" }, { id: "c", label: "C" }];

describe("nextTab", () => {
  it("wraps in both directions", () => {
    expect(nextTab(tabs, "a", 1)).toBe("b");
    expect(nextTab(tabs, "c", 1)).toBe("a");
    expect(nextTab(tabs, "a", -1)).toBe("c");
  });
});
```

- [ ] Write `src/components/shell/dirty.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { shouldConfirmLeave } from "./dirty";

describe("shouldConfirmLeave", () => {
  it("only when something is dirty", () => {
    expect(shouldConfirmLeave(new Set())).toBe(false);
    expect(shouldConfirmLeave(new Set(["persona"]))).toBe(true);
  });
});
```

- [ ] Create `src/components/shell/DetailPage.tsx`:

```tsx
import type { KeyboardEvent, ReactNode } from "react";
import { StatusPill, type StatusKind } from "./Status";

export interface DetailTabDef<T extends string> {
  id: T;
  label: string;
}

export function nextTab<T extends string>(tabs: DetailTabDef<T>[], active: T, delta: 1 | -1): T {
  const i = Math.max(0, tabs.findIndex((t) => t.id === active));
  return tabs[(i + delta + tabs.length) % tabs.length].id;
}

export interface DetailPageProps<T extends string> {
  avatar: ReactNode;
  title: string;
  status: StatusKind;
  meta?: ReactNode;
  actions?: ReactNode;
  tabs: DetailTabDef<T>[];
  activeTab: T;
  onTab: (id: T) => void;
  /** Rendered at the top of the body (needs-you strip, load errors). */
  banners?: ReactNode;
  children: ReactNode;
}

/** Header + ARIA tab bar + body (spec §4.2). The body remounts per tab so the
 *  cross-fade in detail-page.css runs on every switch. */
export function DetailPage<T extends string>(p: DetailPageProps<T>) {
  function onTabsKey(e: KeyboardEvent<HTMLDivElement>) {
    if (e.key === "ArrowRight") p.onTab(nextTab(p.tabs, p.activeTab, 1));
    else if (e.key === "ArrowLeft") p.onTab(nextTab(p.tabs, p.activeTab, -1));
  }
  return (
    <article className="detail-page">
      <header className="detail-page__head">
        <span className="detail-page__avatar">{p.avatar}</span>
        <div className="detail-page__ident">
          <h1 className="detail-page__title">
            {p.title} <StatusPill kind={p.status} />
          </h1>
          {p.meta && <div className="detail-page__meta">{p.meta}</div>}
        </div>
        {p.actions && <div className="detail-page__actions">{p.actions}</div>}
      </header>
      <div className="detail-page__tabs" role="tablist" onKeyDown={onTabsKey}>
        {p.tabs.map((t) => (
          <button
            key={t.id}
            type="button"
            role="tab"
            id={`tab-${t.id}`}
            aria-selected={t.id === p.activeTab}
            aria-controls={`panel-${t.id}`}
            tabIndex={t.id === p.activeTab ? 0 : -1}
            className={`detail-page__tab${t.id === p.activeTab ? " detail-page__tab--on" : ""}`}
            onClick={() => p.onTab(t.id)}
          >
            {t.label}
          </button>
        ))}
      </div>
      <div key={p.activeTab} className="detail-page__body" role="tabpanel" id={`panel-${p.activeTab}`} aria-labelledby={`tab-${p.activeTab}`}>
        {p.banners}
        {p.children}
      </div>
    </article>
  );
}
```

- [ ] Create `src/components/shell/dirty.tsx`:

```tsx
import { createContext, useCallback, useContext, useEffect, useMemo, useState, type ReactNode } from "react";
import { confirm } from "@tauri-apps/plugin-dialog";

interface DirtyCtx {
  dirty: ReadonlySet<string>;
  mark: (id: string, isDirty: boolean) => void;
}

const Ctx = createContext<DirtyCtx>({ dirty: new Set(), mark: () => {} });

/** Wrap one master–detail page. Sections report unsaved edits with
 *  useMarkDirty; the list and tab bar ask useDirtyGuard before leaving. */
export function DirtyProvider({ children }: { children: ReactNode }) {
  const [dirty, setDirty] = useState<Set<string>>(() => new Set());
  const mark = useCallback((id: string, isDirty: boolean) => {
    setDirty((prev) => {
      if (prev.has(id) === isDirty) return prev;
      const next = new Set(prev);
      if (isDirty) next.add(id);
      else next.delete(id);
      return next;
    });
  }, []);
  const value = useMemo(() => ({ dirty, mark }), [dirty, mark]);
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

/** `useMarkDirty("persona", form differs from saved)`. Clears on unmount. */
export function useMarkDirty(id: string, isDirty: boolean): void {
  const { mark } = useContext(Ctx);
  useEffect(() => {
    mark(id, isDirty);
  }, [id, isDirty, mark]);
  useEffect(() => () => mark(id, false), [id, mark]);
}

export function shouldConfirmLeave(dirty: ReadonlySet<string>): boolean {
  return dirty.size > 0;
}

export function useDirtyGuard(): {
  isDirty: boolean;
  /** Resolves true when leaving is fine: nothing dirty, or the user chose to discard. */
  confirmLeave: (message: string, title: string) => Promise<boolean>;
} {
  const { dirty } = useContext(Ctx);
  const confirmLeave = useCallback(
    async (message: string, title: string) => {
      if (!shouldConfirmLeave(dirty)) return true;
      return confirm(message, { title, kind: "warning" });
    },
    [dirty],
  );
  return { isDirty: shouldConfirmLeave(dirty), confirmLeave };
}
```

- [ ] Create `src/styles/components/detail-page.css` and import it after `source-list.css`:

```css
/* DetailPage (spec §4.2). */
.detail-page { display: flex; flex-direction: column; min-width: 0; height: 100%; overflow: auto; background: var(--surface-detail); }
.detail-page__head { display: flex; align-items: flex-start; gap: var(--space-6); padding: var(--space-7) var(--space-8) 0; }
.detail-page__avatar { display: flex; width: 48px; height: 48px; flex: none; }
.detail-page__ident { min-width: 0; flex: 1; }
.detail-page__title { margin: 0; font-size: var(--text-xl); font-weight: var(--fw-semi); letter-spacing: -.015em; display: flex; align-items: center; gap: 10px; }
.detail-page__meta { margin-top: 3px; font-size: var(--text-sm); color: var(--text-secondary); display: flex; flex-wrap: wrap; gap: 4px 6px; align-items: center; }
.detail-page__meta .sep { color: var(--text-tertiary); }
.detail-page__meta .mono { font-family: var(--font-mono); font-size: var(--text-sm); }
.detail-page__actions { margin-left: auto; display: flex; gap: var(--space-4); flex: none; }
.detail-page__tabs { display: flex; gap: 2px; padding: var(--space-6) var(--space-8) 0; border-bottom: 1px solid var(--border-line); }
.detail-page__tab {
  padding: 6px 10px 9px; font: inherit; font-size: 13px; color: var(--text-secondary); background: none; border: 0;
  border-bottom: 2px solid transparent; margin-bottom: -1px; white-space: nowrap; cursor: pointer;
}
.detail-page__tab--on { color: var(--text-primary); border-color: var(--color-brand); font-weight: 500; }
.detail-page__body { padding: var(--space-7) var(--space-8) var(--space-9); display: flex; flex-direction: column; gap: var(--space-6); }
@media (prefers-reduced-motion: no-preference) {
  .detail-page__body { animation: detail-fade var(--dur-fast) var(--ease-out); }
}
@keyframes detail-fade { from { opacity: 0; } to { opacity: 1; } }
/* Cards inside the body: hairline, no shadow (spec §5.2). */
.detail-card { background: var(--surface-card); border: 1px solid var(--border-line); border-radius: var(--radius-lg); padding: 14px 16px; min-width: 0; }
.detail-card__eyebrow { font-size: var(--text-xs); font-weight: var(--fw-semi); letter-spacing: .06em; text-transform: uppercase; color: var(--text-tertiary); margin-bottom: 8px; }
.detail-section { scroll-margin-top: var(--space-6); }
.detail-section + .detail-section { margin-top: var(--space-8); padding-top: var(--space-8); border-top: 1px solid var(--border-line-subtle); }
.detail-section__title { margin: 0 0 var(--space-5); font-size: var(--text-md); font-weight: var(--fw-semi); }
.detail-attn { display: flex; align-items: center; gap: 12px; padding: 10px 14px; border-radius: var(--radius-md); background: color-mix(in srgb, var(--status-attention) 14%, transparent); }
.detail-attn__text { flex: 1; min-width: 0; font-size: var(--text-sm); }
.detail-kv { display: grid; grid-template-columns: 100px 1fr auto; gap: 10px; align-items: center; padding: 7px 0; border-top: 1px solid var(--border-line-subtle); font-size: var(--text-sm); }
.detail-kv:first-of-type { border-top: 0; }
.detail-kv > :first-child { color: var(--text-secondary); }
.detail-kv button { font: inherit; font-size: var(--text-sm); color: var(--color-brand-strong); background: none; border: 0; cursor: pointer; }
.detail-stats { display: grid; grid-template-columns: repeat(4, 1fr); padding: 0; overflow: hidden; font-variant-numeric: tabular-nums; }
.detail-stats > div { padding: 12px 16px; border-left: 1px solid var(--border-line-subtle); }
.detail-stats > div:first-child { border-left: 0; }
.detail-stats b { display: block; font-size: var(--text-lg); font-weight: var(--fw-semi); }
.detail-stats span { font-size: var(--text-xs); color: var(--text-secondary); }
.detail-two { display: grid; grid-template-columns: 1.25fr 1fr; gap: var(--space-6); }
.master-detail--compact .detail-two { grid-template-columns: 1fr; }
```

- [ ] Tests pass; `npm run build`. Commit: `feat(hub): DetailPage with ARIA tabs, and a dirty guard for unsaved edits`

**Interfaces — Produces:** `DetailTabDef<T>`, `nextTab`, `<DetailPage avatar title status meta? actions? tabs activeTab onTab banners?>`, `DirtyProvider`, `useMarkDirty(id, isDirty)`, `useDirtyGuard(): { isDirty, confirmLeave(message, title) }`, `shouldConfirmLeave`; CSS `.detail-page*`, `.detail-card`, `.detail-card__eyebrow`, `.detail-section`, `.detail-attn`, `.detail-kv`, `.detail-stats`, `.detail-two`.

### Task 3.3 — `SplitButton` and `OverflowMenu`

- [ ] Create `src/components/shell/useMenu.ts`:

```ts
import { useEffect, useRef, useState, type RefObject } from "react";

/** Open/close state for a small popup menu: closes on outside click or Escape. */
export function useMenu(): { open: boolean; setOpen: (v: boolean) => void; rootRef: RefObject<HTMLDivElement> } {
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    function onDoc(e: MouseEvent) {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) setOpen(false);
    }
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") setOpen(false);
    }
    document.addEventListener("mousedown", onDoc);
    window.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      window.removeEventListener("keydown", onKey);
    };
  }, [open]);
  return { open, setOpen, rootRef };
}
```

- [ ] Create `src/components/shell/SplitButton.tsx`:

```tsx
import type { ReactNode } from "react";
import { useMenu } from "./useMenu";

export interface MenuItemDef {
  id: string;
  label: string;
  onSelect: () => void;
  disabled?: boolean;
  /** Renders with the danger colour (Delete). */
  danger?: boolean;
}

export interface SplitButtonProps {
  label: ReactNode;
  onPrimary: () => void;
  items: MenuItemDef[];
  disabled?: boolean;
  menuLabel: string;
}

export function MenuList({ items, onPick }: { items: MenuItemDef[]; onPick: () => void }) {
  return (
    <ul className="menu" role="menu">
      {items.map((it) => (
        <li key={it.id} role="none">
          <button
            type="button"
            role="menuitem"
            className={`menu__item${it.danger ? " menu__item--danger" : ""}`}
            disabled={it.disabled}
            onClick={() => {
              onPick();
              it.onSelect();
            }}
          >
            {it.label}
          </button>
        </li>
      ))}
    </ul>
  );
}

export function SplitButton({ label, onPrimary, items, disabled, menuLabel }: SplitButtonProps) {
  const { open, setOpen, rootRef } = useMenu();
  return (
    <div className="split" ref={rootRef}>
      <button type="button" className="btn btn--primary split__main" onClick={onPrimary} disabled={disabled}>
        {label}
      </button>
      <button
        type="button"
        className="btn btn--primary split__more"
        aria-haspopup="menu"
        aria-expanded={open}
        aria-label={menuLabel}
        onClick={() => setOpen(!open)}
        disabled={disabled}
      >
        ▾
      </button>
      {open && <MenuList items={items} onPick={() => setOpen(false)} />}
    </div>
  );
}
```

- [ ] Create `src/components/shell/OverflowMenu.tsx`:

```tsx
import { MenuList, type MenuItemDef } from "./SplitButton";
import { useMenu } from "./useMenu";

export function OverflowMenu({ items, label }: { items: MenuItemDef[]; label: string }) {
  const { open, setOpen, rootRef } = useMenu();
  return (
    <div className="split" ref={rootRef}>
      <button type="button" className="btn btn--secondary btn--icon" aria-haspopup="menu" aria-expanded={open} aria-label={label} title={label} onClick={() => setOpen(!open)}>
        ⋯
      </button>
      {open && <MenuList items={items} onPick={() => setOpen(false)} />}
    </div>
  );
}
```

- [ ] Write `src/components/shell/SplitButton.test.tsx`:

```tsx
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { SplitButton } from "./SplitButton";
import { OverflowMenu } from "./OverflowMenu";

describe("menus at rest", () => {
  it("split button renders the primary label and a closed menu trigger", () => {
    const html = renderToStaticMarkup(<SplitButton label="Run" onPrimary={() => {}} items={[]} menuLabel="More run options" />);
    expect(html).toContain(">Run<");
    expect(html).toContain('aria-haspopup="menu"');
    expect(html).toContain('aria-expanded="false"');
    expect(html).not.toContain('role="menu"');
  });
  it("overflow menu is an icon button", () => {
    expect(renderToStaticMarkup(<OverflowMenu items={[]} label="More" />)).toContain('aria-label="More"');
  });
});
```

- [ ] Create `src/styles/components/menus.css` (import after `detail-page.css`):

```css
.split { position: relative; display: inline-flex; }
.split__main { border-radius: var(--radius-sm) 0 0 var(--radius-sm); }
.split__more { width: 26px; padding: 0; justify-content: center; border-radius: 0 var(--radius-sm) var(--radius-sm) 0; border-left: 1px solid color-mix(in srgb, var(--text-on-brand) 28%, transparent); }
.btn--icon { width: 28px; padding: 0; justify-content: center; }
.menu {
  position: absolute; top: calc(100% + 4px); right: 0; min-width: 180px; margin: 0; padding: 4px; list-style: none;
  background: var(--surface-card); border: 1px solid var(--border-line); border-radius: var(--radius-md); box-shadow: var(--shadow-pop); z-index: 20;
}
.menu__item { display: block; width: 100%; text-align: left; padding: 7px 10px; border: 0; background: none; border-radius: var(--radius-sm); font: inherit; font-size: 13px; color: var(--text-primary); cursor: pointer; }
.menu__item:hover:not(:disabled) { background: var(--surface-hover); }
.menu__item:disabled { color: var(--text-tertiary); cursor: default; }
.menu__item--danger { color: var(--status-failed); }
```
  Also set `.btn { height: 28px; padding: 0 12px; font-size: var(--text-sm); font-weight: 500; display: inline-flex; align-items: center; gap: 6px; border-radius: var(--radius-sm); }` in `styles/components/primitives.css` line 2 (replace the padding/size/weight declarations; keep the rest).
- [ ] Test → pass. Commit: `feat(hub): SplitButton and OverflowMenu`

**Interfaces — Produces:** `MenuItemDef`, `<SplitButton label onPrimary items disabled? menuLabel>`, `<OverflowMenu items label>`, `useMenu`; CSS `.split*`, `.menu*`, `.btn--icon`.

### Task 3.4 — `detailTabs.ts`

- [ ] Write `src/components/shell/detailTabs.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { ALL_DETAIL_TABS } from "../../types";
import { AGENT_TABS, FLEET_TABS, detailGroupOf } from "./detailTabs";

describe("detailGroupOf", () => {
  it("maps all 11 legacy ids into the 6 groups", () => {
    for (const legacy of ALL_DETAIL_TABS) {
      const g = detailGroupOf(legacy);
      expect(AGENT_TABS).toContain(g.tab);
      expect(g.anchor).toBe(legacy);
    }
    expect(detailGroupOf("persona").tab).toBe("identity");
    expect(detailGroupOf("permissions").tab).toBe("capabilities");
    expect(detailGroupOf("mobile").tab).toBe("channels");
    expect(detailGroupOf("schedule").tab).toBe("automation");
  });
  it("unknown or empty falls back to Overview", () => {
    expect(detailGroupOf(null)).toEqual({ tab: "overview", anchor: null });
    expect(detailGroupOf("nope")).toEqual({ tab: "overview", anchor: null });
  });
  it("tab orders match the spec", () => {
    expect(AGENT_TABS).toEqual(["overview", "identity", "capabilities", "memory", "automation", "channels"]);
    expect(FLEET_TABS).toEqual(["overview", "members", "jobs", "settings"]);
  });
});
```

- [ ] Create `src/components/shell/detailTabs.ts`:

```ts
import type { TranslationKey } from "../../i18n/types";
import { ALL_DETAIL_TABS, type DetailTab } from "../../types";

// Agent detail: 11 legacy tabs → 6 groups (spec §4.3).
export type AgentTabId = "overview" | "identity" | "capabilities" | "memory" | "automation" | "channels";
export const AGENT_TABS: AgentTabId[] = ["overview", "identity", "capabilities", "memory", "automation", "channels"];
export const AGENT_TAB_LABEL_KEY: Record<AgentTabId, TranslationKey> = {
  overview: "detail.tab.overview",
  identity: "detail.tab.identity",
  capabilities: "detail.tab.capabilities",
  memory: "detail.tab.memory",
  automation: "detail.tab.automation",
  channels: "detail.tab.channels",
};

const LEGACY_GROUP: Record<DetailTab, AgentTabId> = {
  persona: "identity",
  style: "identity",
  behavior: "identity",
  skills: "capabilities",
  mcp: "capabilities",
  plugins: "capabilities",
  permissions: "capabilities",
  memory: "memory",
  schedule: "automation",
  inbox: "channels",
  mobile: "channels",
};

/** `desiredDetailTab` still speaks the legacy id; this resolves the new tab
 *  and the in-tab anchor (`<section id="agent-<anchor>">`). */
export function detailGroupOf(legacy: string | null): { tab: AgentTabId; anchor: DetailTab | null } {
  if (legacy && (ALL_DETAIL_TABS as readonly string[]).includes(legacy)) {
    const id = legacy as DetailTab;
    return { tab: LEGACY_GROUP[id], anchor: id };
  }
  return { tab: "overview", anchor: null };
}

// Fleet detail (spec §4.4).
export type FleetTabId = "overview" | "members" | "jobs" | "settings";
export const FLEET_TABS: FleetTabId[] = ["overview", "members", "jobs", "settings"];
export const FLEET_TAB_LABEL_KEY: Record<FleetTabId, TranslationKey> = {
  overview: "fleet.tab.overview",
  members: "fleet.tab.members",
  jobs: "fleet.tab.jobs",
  settings: "fleet.tab.settings",
};
```

- [ ] i18n (both tables): `detail.tab.overview` "Overview"/"總覽", `detail.tab.identity` "Identity"/"身分", `detail.tab.capabilities` "Capabilities"/"能力", `detail.tab.memory` "Memory"/"記憶", `detail.tab.automation` "Automation"/"自動化", `detail.tab.channels` "Channels"/"頻道", `fleet.tab.overview` "Overview"/"總覽", `fleet.tab.members` "Members"/"成員", `fleet.tab.jobs` "Jobs"/"任務", `fleet.tab.settings` "Settings"/"設定".
- [ ] Test → pass. Commit: `feat(hub): agent and fleet tab ids with the legacy 11→6 mapping`

**Interfaces — Produces:** `AgentTabId`, `AGENT_TABS`, `AGENT_TAB_LABEL_KEY`, `detailGroupOf`, `FleetTabId`, `FLEET_TABS`, `FLEET_TAB_LABEL_KEY`.

### Task 3.5 — Command palette (⌘K)

- [ ] Write `src/components/shell/palette.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { rankPalette, type PaletteItem } from "./palette";

const noop = () => {};
const items: PaletteItem[] = [
  { id: "page:agents", kind: "page", label: "Agents", run: noop },
  { id: "agent:aura", kind: "agent", label: "AURA", run: noop },
  { id: "agent:auditor", kind: "agent", label: "Auditor", run: noop },
  { id: "action:stop", kind: "action", label: "Stop AURA", run: noop },
  { id: "fleet:builder", kind: "fleet", label: "builder", run: noop },
];

describe("rankPalette", () => {
  it("prefix beats substring, then kind order, then label", () => {
    expect(rankPalette(items, "au").map((i) => i.id)).toEqual(["agent:auditor", "agent:aura", "action:stop"]);
  });
  it("empty query lists everything in kind order, capped", () => {
    expect(rankPalette(items, "", 3).map((i) => i.kind)).toEqual(["page", "action", "agent"]);
  });
});
```

- [ ] Create `src/components/shell/palette.ts`:

```ts
export type PaletteKind = "page" | "action" | "agent" | "fleet";

export interface PaletteItem {
  id: string;
  kind: PaletteKind;
  label: string;
  hint?: string;
  run: () => void;
}

const KIND_ORDER: Record<PaletteKind, number> = { page: 0, action: 1, agent: 2, fleet: 3 };
export const PALETTE_LIMIT = 12;

export function rankPalette(items: PaletteItem[], query: string, limit = PALETTE_LIMIT): PaletteItem[] {
  const q = query.trim().toLowerCase();
  const scored = items
    .map((it) => {
      const l = it.label.toLowerCase();
      const score = !q ? 1 : l.startsWith(q) ? 3 : l.includes(q) ? 2 : 0;
      return { it, score };
    })
    .filter((x) => x.score > 0);
  scored.sort(
    (a, b) => b.score - a.score || KIND_ORDER[a.it.kind] - KIND_ORDER[b.it.kind] || a.it.label.localeCompare(b.it.label),
  );
  return scored.slice(0, limit).map((x) => x.it);
}
```

- [ ] Create `src/components/shell/CommandPalette.tsx`:

```tsx
import { useEffect, useState, type KeyboardEvent } from "react";
import { useT } from "../../i18n";
import { rankPalette, type PaletteItem } from "./palette";

export function isPaletteShortcut(e: globalThis.KeyboardEvent): boolean {
  return (e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && e.key.toLowerCase() === "k";
}

export function CommandPalette({ open, items, onClose }: { open: boolean; items: PaletteItem[]; onClose: () => void }) {
  const { t } = useT();
  const [query, setQuery] = useState("");
  const [cursor, setCursor] = useState(0);
  const visible = rankPalette(items, query);

  useEffect(() => {
    if (open) {
      setQuery("");
      setCursor(0);
    }
  }, [open]);

  if (!open) return null;

  function pick(it: PaletteItem | undefined) {
    if (!it) return;
    onClose();
    it.run();
  }
  function onKey(e: KeyboardEvent<HTMLInputElement>) {
    if (e.key === "ArrowDown") { e.preventDefault(); setCursor((c) => Math.min(visible.length - 1, c + 1)); }
    else if (e.key === "ArrowUp") { e.preventDefault(); setCursor((c) => Math.max(0, c - 1)); }
    else if (e.key === "Enter") { e.preventDefault(); pick(visible[cursor]); }
    else if (e.key === "Escape") { e.preventDefault(); onClose(); }
  }

  return (
    <div className="palette-backdrop" onMouseDown={onClose}>
      <div className="palette" role="dialog" aria-label={t("palette.title")} onMouseDown={(e) => e.stopPropagation()}>
        <input
          autoFocus
          className="palette__input"
          placeholder={t("palette.placeholder")}
          value={query}
          onChange={(e) => { setQuery(e.target.value); setCursor(0); }}
          onKeyDown={onKey}
        />
        <ul className="palette__list" role="listbox">
          {visible.length === 0 && <li className="palette__empty">{t("palette.empty")}</li>}
          {visible.map((it, i) => (
            <li
              key={it.id}
              role="option"
              aria-selected={i === cursor}
              className={`palette__item${i === cursor ? " palette__item--on" : ""}`}
              onMouseEnter={() => setCursor(i)}
              onClick={() => pick(it)}
            >
              <span className="palette__kind">{t(`palette.kind.${it.kind}`)}</span>
              <span className="palette__label">{it.label}</span>
              {it.hint && <span className="palette__hint">{it.hint}</span>}
            </li>
          ))}
        </ul>
      </div>
    </div>
  );
}
```
  (`t(\`palette.kind.${it.kind}\`)` needs a cast: `as TranslationKey` from `../../i18n/types` — add the import and the four keys.)

- [ ] i18n (both): `palette.title` "Search or run a command"/"搜尋或執行指令", `palette.placeholder` "Jump to an agent, fleet or page… or run an action"/"跳到 agent、機群或頁面… 或執行動作", `palette.empty` "Nothing matches"/"沒有符合的項目", `palette.kind.page` "Page"/"頁面", `palette.kind.action` "Action"/"動作", `palette.kind.agent` "Agent"/"Agent", `palette.kind.fleet` "Fleet"/"機群", `palette.action.start` "Start {name}"/"啟動 {name}", `palette.action.stop` "Stop {name}"/"停止 {name}", `palette.action.newChat` "New chat"/"新對話", `palette.action.newAgent` "New agent"/"新增 Agent", `palette.action.settings` "Open Settings"/"開啟設定", `palette.action.refresh` "Refresh"/"重新整理", `shell.search` "Search or run a command"/"搜尋或執行指令".
- [ ] Create `src/styles/components/palette.css` (import after `menus.css`):

```css
.palette-backdrop { position: fixed; inset: 0; background: color-mix(in srgb, var(--text-primary) 28%, transparent); display: flex; justify-content: center; align-items: flex-start; padding-top: 12vh; z-index: 40; }
.palette { width: min(560px, 92vw); background: var(--surface-card); border: 1px solid var(--border-line); border-radius: var(--radius-lg); box-shadow: var(--shadow-pop); overflow: hidden; }
.palette__input { width: 100%; height: 44px; padding: 0 16px; border: 0; border-bottom: 1px solid var(--border-line); background: transparent; color: var(--text-primary); font: inherit; font-size: var(--text-md); outline: none; }
.palette__list { list-style: none; margin: 0; padding: 6px; max-height: 50vh; overflow: auto; }
.palette__item { display: grid; grid-template-columns: 60px 1fr auto; gap: 10px; align-items: center; padding: 8px 10px; border-radius: var(--radius-sm); cursor: pointer; font-size: 13px; }
.palette__item--on { background: var(--color-brand-soft); }
.palette__kind, .palette__hint, .palette__empty { font-size: var(--text-xs); color: var(--text-tertiary); }
.palette__empty { padding: 12px; }
```

- [ ] `src/components/shell/Sidebar.tsx`: add `onSearch: () => void` to props and, as the first child of `<nav>`, a search button:

```tsx
      <button type="button" className="shell-sidebar__search" onClick={onSearch} title={t("shell.search")}>
        <Ico><circle cx="11" cy="11" r="6" /><path d="M20 20l-4.5-4.5" /></Ico>
        <span className="shell-sidebar-item__label">{t("shell.search")}</span>
        <kbd className="shell-sidebar__kbd">⌘K</kbd>
      </button>
```
  CSS in `shell.css`: `.shell-sidebar__search { display:flex; align-items:center; gap:8px; height:28px; padding:0 8px; border-radius:var(--radius-sm); border:1px solid var(--border-line-subtle); background:var(--surface-secondary); color:var(--text-tertiary); font:inherit; font-size:12px; cursor:pointer; } .shell-sidebar__kbd { margin-left:auto; font:11px var(--font-mono); } .shell-sidebar--collapsed .shell-sidebar__search { justify-content:center; padding:0; } .shell-sidebar--collapsed .shell-sidebar__kbd { display:none; }`.
- [ ] `src/components/shell/Shell.tsx`: add `onSearch: () => void` to `ShellProps` and pass it to `<Sidebar onSearch={onSearch} …>`.
- [ ] `src/components/fleet/FleetView.tsx`: add `requestedName?: string | null` to the props and, after the "Load detail whenever selection changes" effect:

```tsx
  // The command palette can ask for a fleet by name (spec §6.6).
  useEffect(() => {
    if (requestedName) setSelectedName(requestedName);
  }, [requestedName]);
```
- [ ] `src/components/DashboardApp.tsx`:
  - state: `const [paletteOpen, setPaletteOpen] = useState(false); const [fleetRequest, setFleetRequest] = useState<string | null>(null); const [paletteFleets, setPaletteFleets] = useState<FleetSummary[]>([]);` (`import type { FleetSummary } from "./fleet/types"`).
  - replace the "⌘K focus search" effect (lines 308–317) with one that calls `setPaletteOpen(true)` when `isPaletteShortcut(e)`.
  - `useEffect(() => { if (!paletteOpen) return; invoke<FleetSummary[]>("fleet_list").then(setPaletteFleets).catch(() => setPaletteFleets([])); }, [paletteOpen]);`
  - build items (before `return`):

```tsx
  const selectedRuntime = selectedAgent ? runtimeMap.get(selectedAgent)?.state.state : undefined;
  const paletteItems: PaletteItem[] = [
    ...NAV_ITEMS.map((n) => ({ id: `page:${n.id}`, kind: "page" as const, label: t(n.labelKey), run: () => setPage(n.id) })),
    { id: "action:newChat", kind: "action", label: t("palette.action.newChat"), run: () => setPage("chats") },
    { id: "action:newAgent", kind: "action", label: t("palette.action.newAgent"), run: () => setWizardOpen(true) },
    { id: "action:settings", kind: "action", label: t("palette.action.settings"), run: () => setSettingsOpen(true) },
    { id: "action:refresh", kind: "action", label: t("palette.action.refresh"), run: () => { invoke("list_agents").catch(console.error); refreshInbox(); } },
    ...(selectedAgent
      ? [selectedRuntime === "running"
          ? { id: "action:stop", kind: "action" as const, label: t("palette.action.stop", { name: selectedAgent }), run: () => { invoke("stop_agent", { name: selectedAgent }).catch(console.error); } }
          : { id: "action:start", kind: "action" as const, label: t("palette.action.start", { name: selectedAgent }), run: () => { invoke("start_agent", { name: selectedAgent }).catch(console.error); } }]
      : []),
    ...agents.map((a) => ({ id: `agent:${a.name}`, kind: "agent" as const, label: a.display_name, hint: a.role ?? undefined, run: () => { setPage("agents"); setSelected(a.name); } })),
    ...paletteFleets.map((f) => ({ id: `fleet:${f.name}`, kind: "fleet" as const, label: f.display_name, run: () => { setPage("fleets"); setFleetRequest(f.name); } })),
  ];
```
  - render `<CommandPalette open={paletteOpen} items={paletteItems} onClose={() => setPaletteOpen(false)} />` next to the modals; pass `onSearch={() => setPaletteOpen(true)}` to `<Shell>` and `requestedName={fleetRequest}` to `<FleetView>`.
  - the global bar's `<label className="field dashboard__bar-search">…` input stays for now (it still feeds `query` to Chats); ⌘R: add `if ((e.metaKey || e.ctrlKey) && e.key.toLowerCase() === "r") { e.preventDefault(); invoke("list_agents").catch(console.error); refreshInbox(); }` to the same keydown effect.
- [ ] Tests, build, lint. Preview: ⌘K opens the palette; typing "au" ranks agents; Enter jumps; a fleet result opens the Fleets page on that fleet; Esc closes.
- [ ] Commit: `feat(hub): ⌘K command palette — jump to pages, agents, fleets; start/stop, new chat, settings`

**Interfaces — Produces:** `PaletteItem`, `rankPalette`, `<CommandPalette open items onClose>`, `isPaletteShortcut`; `Shell`/`Sidebar` prop `onSearch`; `FleetView` prop `requestedName`.

**Manual acceptance PR 3:** palette works from every page; ⌘F does nothing yet (no SourceList mounted) and does not break the browser find; all existing pages unchanged.

---

## PR 4 — Agents page

Branch `feat/hub-2-agents`.

### Task 4.1 — `InboxItem.agent` and `needsYouCounts`

- [ ] `src/components/home/inbox.ts`: add `agent?: string; // set by the hitl and companion adapters` after `payload`.
- [ ] `src/components/home/useInbox.ts`: in `hitlToItem` add `agent,` to the returned object; change `companionToItem(raw: RawCompanionEvent, lang: string)` to `companionToItem(raw: RawCompanionEvent, lang: string, agent: string)` and add `agent,` to its return; in `refreshCompanion` capture `const names = agentNamesRef.current;` before `Promise.all(names.map(…))` and build items with `perAgent.flatMap((rows, i) => rows.map((r) => companionToItem(r, lang, names[i])))`.
- [ ] Extend `src/components/home/useInboxAdapters.test.ts`: in the existing hitl test assert `expect(item?.agent).toBe(raw.agent)`; in the companion test pass a third argument `"aura"` and assert `expect(item?.agent).toBe("aura")`. Run → fails until the edits above are in; then passes.
- [ ] Write `src/components/home/needsYou.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { needsYouCounts } from "./needsYou";
import type { InboxItem } from "./inbox";

const item = (kind: InboxItem["kind"], id: string, agent?: string): InboxItem =>
  ({ kind, id, ts: 1, title: "", subtitle: "", payload: null, agent });

describe("needsYouCounts", () => {
  it("counts per agent and ignores items without an agent", () => {
    const counts = needsYouCounts([item("hitl", "1", "aura"), item("companion", "2", "aura"), item("hitl", "3", "scout"), item("upgrade_blocked", "4")]);
    expect(counts).toEqual({ aura: 2, scout: 1 });
  });
});
```

- [ ] Create `src/components/home/needsYou.ts`:

```ts
import type { InboxItem } from "./inbox";

/** Per-agent "needs you" counts for the list badge (spec §6.3). Items with no
 *  agent (blocked skill upgrades, install requests) count toward Home only. */
export function needsYouCounts(items: InboxItem[]): Record<string, number> {
  const counts: Record<string, number> = {};
  for (const it of items) {
    if (!it.agent) continue;
    counts[it.agent] = (counts[it.agent] ?? 0) + 1;
  }
  return counts;
}
```

- [ ] Tests → pass. Commit: `feat(hub): inbox items carry their agent; per-agent needs-you counts`

**Interfaces — Produces:** `InboxItem.agent?`, `needsYouCounts(items): Record<string, number>`.

### Task 4.2 — `agentOverview.ts`

- [ ] Write `src/components/detail/agent/agentOverview.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { activityFor } from "./agentOverview";
import type { ChannelSummary } from "../../../work/types";

const ch = (id: string, agents: string[], state: string, updated_at: string): ChannelSummary =>
  ({ id, title: id, state, goal: "", created_at: updated_at, updated_at, participants: [], agents, turns: 1, preview: "" });

describe("activityFor", () => {
  it("now = newest non-terminal channel; recent = newest first, capped", () => {
    const channels = [
      ch("old", ["aura"], "completed", "2026-09-01T00:00:00Z"),
      ch("live", ["aura", "mur"], "running", "2026-09-06T10:00:00Z"),
      ch("other", ["scout"], "running", "2026-09-06T11:00:00Z"),
      ch("done", ["aura"], "completed", "2026-09-05T00:00:00Z"),
    ];
    const a = activityFor(channels, "aura", 2);
    expect(a.now?.id).toBe("live");
    expect(a.recent.map((c) => c.id)).toEqual(["live", "done"]);
    expect(activityFor(channels, "nobody").now).toBeNull();
  });
});
```

- [ ] Create `src/components/detail/agent/agentOverview.ts`:

```ts
import type { ChannelSummary } from "../../../work/types";
import { isRunningChannel } from "../../home/useChannels";

export const RECENT_LIMIT = 3;

export interface AgentActivity {
  now: ChannelSummary | null;
  recent: ChannelSummary[];
}

/** What this agent is doing, from the same `channel_list` data Home reads. */
export function activityFor(channels: ChannelSummary[], agent: string, limit = RECENT_LIMIT): AgentActivity {
  const mine = channels
    .filter((c) => c.agents.includes(agent))
    .sort((a, b) => Date.parse(b.updated_at) - Date.parse(a.updated_at));
  return { now: mine.find(isRunningChannel) ?? null, recent: mine.slice(0, limit) };
}
```

- [ ] Test → pass. Commit: `feat(hub): per-agent activity from channel summaries`

**Interfaces — Produces:** `activityFor(channels, agent, limit?): AgentActivity`.

### Task 4.3 — `components/detail/agent/*`

Relocate `AgentInspector` into a `DetailPage`. Line numbers refer to `src/components/inspector/AgentInspector.tsx`.

- [ ] Create `src/components/detail/agent/IdentityTab.tsx` — the model block (lines 280–314: `ModelCombobox`, fallback chain, smart routing, `ModelLibrary`) followed by `PersonaTab`, `StyleTab`, `BehaviorTab`, each wrapped:

```tsx
import type { AgentDetail } from "../../../types";
import type { ModelOption } from "../../modelPicker";
import { useT } from "../../../i18n";
import { ModelCombobox } from "../../ModelCombobox";
import { ModelLibrary } from "../../ModelLibrary";
import { FallbackChainEditor } from "../../settings/FallbackChainEditor";
import { PersonaTab } from "../../inspector/tabs/PersonaTab";
import { StyleTab } from "../../inspector/tabs/StyleTab";
import { BehaviorTab } from "../../inspector/tabs/BehaviorTab";

export interface IdentityTabProps {
  detail: AgentDetail;
  onSaved: (d: AgentDetail) => void;
  modelOptions: ModelOption[];
  agentChain: string[];
  chainErr: string | null;
  onChain: (next: string[]) => void;
  agentSmart: boolean | null;
  onSmart: (value: string) => void;
  libraryOpen: boolean;
  setLibraryOpen: (v: boolean) => void;
}

export function IdentityTab(p: IdentityTabProps) {
  const { t } = useT();
  return (
    <>
      <section className="detail-section" id="agent-model">
        <h3 className="detail-section__title">{t("detail.section.model")}</h3>
        {/* lines 280–313 of AgentInspector.tsx, verbatim, with `detail`,
            `handleSaved`→p.onSaved, `agentChain`→p.agentChain, `modelOptions`→p.modelOptions,
            `saveAgentChain`→p.onChain, `chainErr`→p.chainErr, `agentSmart`→p.agentSmart,
            `saveAgentSmart`→p.onSmart, `libraryOpen`/`setLibraryOpen`→p.* */}
      </section>
      <section className="detail-section" id="agent-persona">
        <h3 className="detail-section__title">{t("detail.persona")}</h3>
        <PersonaTab detail={p.detail} onSaved={p.onSaved} />
      </section>
      <section className="detail-section" id="agent-style">
        <h3 className="detail-section__title">{t("detail.style")}</h3>
        <StyleTab detail={p.detail} onSaved={p.onSaved} />
      </section>
      <section className="detail-section" id="agent-behavior">
        <h3 className="detail-section__title">{t("detail.behavior")}</h3>
        <BehaviorTab detail={p.detail} onSaved={p.onSaved} />
      </section>
    </>
  );
}
```

- [ ] Create `src/components/detail/agent/CapabilitiesTab.tsx` with the same shape: sections `agent-skills` (`SkillsTab`), `agent-mcp` (`McpTab`), `agent-plugins` (`PluginsTab`), `agent-permissions` (`PermissionsTab detail`), titles `detail.skills`, `detail.mcp`, `detail.plugins`, `detail.permissions`. Props `{ detail; onSaved }`.
- [ ] Create `src/components/detail/agent/ChannelsTab.tsx`: sections `agent-inbox` (`CompanionInbox agentName`) and `agent-mobile` (`MobileTab agentName`), titles `detail.inbox`, `detail.mobile`. Props `{ agentName }`.
- [ ] Create `src/components/detail/agent/OverviewTab.tsx`:

```tsx
import type { AgentDetail } from "../../../types";
import type { ChannelSummary } from "../../../work/types";
import type { AgentTabId } from "../../shell/detailTabs";
import { useT } from "../../../i18n";
import { NeedsYouBadge } from "../../shell/Status";
import { activityFor } from "./agentOverview";

export interface OverviewTabProps {
  detail: AgentDetail | null;
  channels: ChannelSummary[];
  agentName: string;
  needsYou: number;
  onGoTo: (tab: AgentTabId) => void;
  onOpenChat: () => void;
  onOpenHome: () => void;
}

const DASH = "—";

export function OverviewTab(p: OverviewTabProps) {
  const { t } = useT();
  const { now, recent } = activityFor(p.channels, p.agentName);
  return (
    <>
      {p.needsYou > 0 && (
        <div className="detail-attn" role="status">
          <NeedsYouBadge count={p.needsYou} />
          <span className="detail-attn__text">{t("status.needsYou", { count: p.needsYou })}</span>
          <button type="button" className="btn btn--secondary" onClick={p.onOpenHome}>{t("overview.review")}</button>
        </div>
      )}
      <div className="detail-card">
        <div className="detail-card__eyebrow">{t("overview.now")}</div>
        {now ? (
          <>
            <h4 className="overview-now__title">{now.title || now.goal}</h4>
            <p className="overview-now__sub">{now.preview}</p>
          </>
        ) : (
          <p className="overview-now__sub">{t("overview.nothingRunning")}</p>
        )}
        <button type="button" className="btn btn--link" onClick={p.onOpenChat}>{t("overview.openChat")}</button>
      </div>
      <div className="detail-card detail-stats">
        <div><b>{DASH}</b><span>{t("overview.costToday")}</span></div>
        <div><b>{DASH}</b><span>{t("overview.turnsToday")}</span></div>
        <div><b>{recent.reduce((n, c) => n + c.turns, 0)}</b><span>{t("overview.recentTurns")}</span></div>
        <div><b>{recent[0] ? new Date(recent[0].updated_at).toLocaleTimeString() : DASH}</b><span>{t("overview.lastActive")}</span></div>
      </div>
      <div className="detail-two">
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("overview.recent")}</div>
          {recent.length === 0 && <p className="overview-now__sub">{t("overview.noRecent")}</p>}
          {recent.map((c) => (
            <div key={c.id} className="detail-kv"><span>{new Date(c.updated_at).toLocaleDateString()}</span><span>{c.title || c.goal}</span><span /></div>
          ))}
        </div>
        <div className="detail-card">
          <div className="detail-card__eyebrow">{t("overview.glance")}</div>
          <div className="detail-kv"><span>{t("detail.section.model")}</span><span>{p.detail?.model_ref ?? DASH}</span><button type="button" onClick={() => p.onGoTo("identity")}>{t("detail.tab.identity")}</button></div>
          <div className="detail-kv"><span>{t("detail.skills")}</span><span>{p.detail ? t("overview.count", { count: p.detail.skills.length }) : DASH}</span><button type="button" onClick={() => p.onGoTo("capabilities")}>{t("detail.tab.capabilities")}</button></div>
          <div className="detail-kv"><span>{t("detail.mcp")}</span><span>{p.detail ? t("overview.count", { count: p.detail.mcp_servers.length }) : DASH}</span><button type="button" onClick={() => p.onGoTo("capabilities")}>{t("detail.tab.capabilities")}</button></div>
          <div className="detail-kv"><span>{t("detail.schedule")}</span><span>{DASH}</span><button type="button" onClick={() => p.onGoTo("automation")}>{t("detail.tab.automation")}</button></div>
          <div className="detail-kv"><span>{t("detail.memory")}</span><span>{DASH}</span><button type="button" onClick={() => p.onGoTo("memory")}>{t("detail.tab.memory")}</button></div>
        </div>
      </div>
    </>
  );
}
```
  Before writing this file run `command grep -n 'model_ref\|skills\|mcp_servers' src/types.ts` inside `interface AgentDetail` and use the real field names (the three above are the expected ones; if a field is absent, render `DASH` for that row instead of inventing a field). Cost today, turns today, schedule and memory counts have no Hub source: they render `—` by design (spec §6.4).
- [ ] i18n (both): `overview.now` "Now"/"現在", `overview.nothingRunning` "Nothing running right now."/"目前沒有進行中的工作。", `overview.openChat` "Open chat →"/"開啟對話 →", `overview.review` "Review"/"查看", `overview.costToday` "Cost today"/"今日花費", `overview.turnsToday` "Turns today"/"今日回合", `overview.recentTurns` "Turns in recent work"/"近期回合", `overview.lastActive` "Last active"/"上次活動", `overview.recent` "Recent work"/"近期工作", `overview.noRecent` "No recent work."/"沒有近期工作。", `overview.glance` "Setup at a glance"/"設定概覽", `overview.count` "{count}"/"{count}", `detail.section.model` "Model"/"模型" (`detail.mobile`, `detail.memory`, `detail.schedule`, `detail.plugins`, `detail.skills`, `detail.mcp`, `detail.permissions`, `detail.inbox` already exist — `AgentInspector`'s `TAB_LABEL_KEYS` uses them), `action.chat` "Chat"/"對話", `action.openChatWindow` "Open chat in a window"/"在新視窗開啟對話", `action.more` "More actions"/"更多動作".
- [ ] Create `src/components/detail/agent/AgentDetail.tsx`:

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import type { AgentEntry, AgentRuntimeStatus, AgentDetail as AgentDetailData } from "../../../types";
import type { ChannelSummary } from "../../../work/types";
import type { ModelOption } from "../../modelPicker";
import { useAgents } from "../../../context/AgentContext";
import { useT } from "../../../i18n";
import { CATEGORY_COLORS, avatarInitials, avatarPreset, familyOf } from "../../../utils";
import { PetFace } from "../../PetFace";
import { sanitizeChain } from "../../settings/modelSwitch";
import { DetailPage } from "../../shell/DetailPage";
import { OverflowMenu } from "../../shell/OverflowMenu";
import { statusOf } from "../../shell/Status";
import { AGENT_TABS, AGENT_TAB_LABEL_KEY, detailGroupOf, type AgentTabId } from "../../shell/detailTabs";
import { useDirtyGuard } from "../../shell/dirty";
import { MemoryTab } from "../../MemoryTab";
import { ScheduleTab } from "../../inspector/tabs/ScheduleTab";
import { IdentityTab } from "./IdentityTab";
import { CapabilitiesTab } from "./CapabilitiesTab";
import { ChannelsTab } from "./ChannelsTab";
import { OverviewTab } from "./OverviewTab";

export interface AgentDetailProps {
  agentName: string;
  entry: AgentEntry | undefined;
  runtime: AgentRuntimeStatus | undefined;
  channels: ChannelSummary[];
  needsYou: number;
  onOpenChat: (name: string) => void;
  onOpenHome: () => void;
}

function showToast(msg: string, durationMs = 2000) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

export function AgentDetail({ agentName, entry, runtime, channels, needsYou, onOpenChat, onOpenHome }: AgentDetailProps) {
  const { t } = useT();
  const { desiredDetailTab, setDesiredDetailTab } = useAgents();
  const { confirmLeave } = useDirtyGuard();
  const [detail, setDetail] = useState<AgentDetailData | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState<AgentTabId>("overview");
  const [libraryOpen, setLibraryOpen] = useState(false);
  const [modelOptions, setModelOptions] = useState<ModelOption[]>([]);
  const [agentChain, setAgentChain] = useState<string[]>([]);
  const [chainErr, setChainErr] = useState<string | null>(null);
  const [agentSmart, setAgentSmart] = useState<boolean | null>(null);

  // Data loads: copy the four `useEffect`s from AgentInspector.tsx that call
  // `get_agent_detail`, `agent_get_fallback`, `agent_get_smart` and `list_models`
  // (locate with `grep -n 'invoke<' src/components/inspector/AgentInspector.tsx`),
  // keyed on agentName, verbatim. Then copy `saveAgentChain` and `saveAgentSmart`
  // (`grep -n 'function saveAgent' …`) verbatim.

  // Deep link: legacy tab id → new tab + anchor scroll.
  useEffect(() => {
    if (!desiredDetailTab) return;
    const g = detailGroupOf(desiredDetailTab);
    setTab(g.tab);
    setDesiredDetailTab(null);
    if (g.anchor) {
      requestAnimationFrame(() => document.getElementById(`agent-${g.anchor}`)?.scrollIntoView({ block: "start" }));
    }
  }, [desiredDetailTab, setDesiredDetailTab]);

  useEffect(() => {
    setTab("overview");
  }, [agentName]);

  async function changeTab(next: AgentTabId) {
    if (next === tab) return;
    if (await confirmLeave(t("detail.discardBody"), t("detail.discardTitle"))) setTab(next);
  }

  const rt = runtime?.state.state;
  const isRunning = rt === "running" || rt === "restarting";
  const displayName = detail?.display_name ?? entry?.display_name ?? agentName;
  const preset = entry ? avatarPreset(entry) : null;
  const avatar = preset ? (
    <PetFace presetId={preset} family={familyOf(preset)} expression="idle" size={48} />
  ) : (
    <span className="detail-page__initials" style={{ background: CATEGORY_COLORS[entry?.category ?? "custom"] }}>{avatarInitials(displayName)}</span>
  );

  // handleRun / handleStop / handleExport: copy from AgentInspector.tsx
  // (`grep -n 'function handle' src/components/inspector/AgentInspector.tsx`), verbatim.

  const actions = (
    <>
      <button type="button" className="btn btn--primary" onClick={() => onOpenChat(agentName)}>{t("action.chat")}</button>
      {isRunning ? (
        <button type="button" className="btn btn--secondary" onClick={() => handleStop(agentName)}>{t("action.stop")}</button>
      ) : (
        <button type="button" className="btn btn--secondary" onClick={() => handleRun(agentName)}>{t("action.run")}</button>
      )}
      <OverflowMenu
        label={t("action.more")}
        items={[
          { id: "export", label: t("action.export"), onSelect: () => { void handleExport(agentName); } },
          { id: "chatWindow", label: t("action.openChatWindow"), onSelect: () => { invoke("open_chat_window", { agentName }).catch(console.error); } },
        ]}
      />
    </>
  );

  const meta = (
    <>
      {entry?.role && <span>{entry.role}</span>}
      {entry?.role && <span className="sep">·</span>}
      <span className="mono">{entry?.model_id ?? "—"}</span>
    </>
  );

  return (
    <DetailPage
      avatar={avatar}
      title={displayName}
      status={statusOf(runtime?.state)}
      meta={meta}
      actions={actions}
      tabs={AGENT_TABS.map((id) => ({ id, label: t(AGENT_TAB_LABEL_KEY[id]) }))}
      activeTab={tab}
      onTab={(id) => { void changeTab(id); }}
      banners={error ? <p className="detail-error">{t("detail.loadFailed", { error })}</p> : undefined}
    >
      {tab === "overview" && (
        <OverviewTab detail={detail} channels={channels} agentName={agentName} needsYou={needsYou} onGoTo={(id) => { void changeTab(id); }} onOpenChat={() => onOpenChat(agentName)} onOpenHome={onOpenHome} />
      )}
      {detail && tab === "identity" && (
        <IdentityTab detail={detail} onSaved={setDetail} modelOptions={modelOptions} agentChain={agentChain} chainErr={chainErr} onChain={saveAgentChain} agentSmart={agentSmart} onSmart={saveAgentSmart} libraryOpen={libraryOpen} setLibraryOpen={setLibraryOpen} />
      )}
      {detail && tab === "capabilities" && <CapabilitiesTab detail={detail} onSaved={setDetail} />}
      {tab === "memory" && <MemoryTab agentName={agentName} />}
      {tab === "automation" && <ScheduleTab agentName={agentName} />}
      {tab === "channels" && <ChannelsTab agentName={agentName} />}
      {!detail && !error && tab !== "overview" && <p className="detail-loading">{t("detail.loading")}</p>}
    </DetailPage>
  );
}
```
  Where a comment says "verbatim", paste the cited lines from `AgentInspector.tsx` and adjust only the identifiers named. `detail.discardTitle` "Unsaved changes"/"尚未儲存", `detail.discardBody` "Discard the changes in this tab?"/"要放棄這個分頁的變更嗎？" go in both tables. CSS: `.detail-page__initials { width:48px; height:48px; border-radius:var(--radius-lg); display:grid; place-items:center; color:#fff; font-weight:var(--fw-bold); font-size:18px; }` in `detail-page.css`; `.overview-now__title { margin:0 0 3px; font-size:var(--text-base); font-weight:500; } .overview-now__sub { margin:0; color:var(--text-secondary); font-size:var(--text-sm); }`.
- [ ] `npm run build` (each new file ≤ 800 lines; `wc -l src/components/detail/agent/*.tsx` to confirm). Commit: `feat(hub): AgentDetail — AgentInspector content in a DetailPage with six tabs`

**Interfaces — Consumes:** `DetailPage`, `OverflowMenu`, `statusOf`, `detailGroupOf`, `AGENT_TABS`, `useDirtyGuard`, `activityFor`. **Produces:** `<AgentDetail agentName entry runtime channels needsYou onOpenChat onOpenHome>`.

### Task 4.4 — Dirty tracking in `PersonaTab` and `StyleTab`

- [ ] `src/components/inspector/tabs/PersonaTab.tsx`: after the `useState` block (lines 44–52) add

```tsx
  useMarkDirty(
    "persona",
    role !== (detail.role ?? "") ||
      category !== detail.persona_category ||
      description !== detail.persona_description ||
      tone !== detail.persona_tone ||
      risk !== detail.persona_risk ||
      verbosity !== detail.persona_verbosity,
  );
```
  with `import { useMarkDirty } from "../../shell/dirty";`. Next to the Save button render `{dirty && <span className="field-muted">{t("detail.unsaved")}</span>}` where `dirty` is the same expression hoisted into a `const dirty = …;` (pass `dirty` to `useMarkDirty`).
- [ ] `src/components/inspector/tabs/StyleTab.tsx`: after line 15 add `const dirty = selected !== detail.style_preset; useMarkDirty("style", dirty);` and the same unsaved hint next to its Save button.
- [ ] i18n: `detail.unsaved` "Unsaved changes"/"尚未儲存".
- [ ] `npm run build`. Commit: `feat(hub): persona and style edits register with the dirty guard`

### Task 4.5 — The Agents page on `SourceList` + `AgentDetail`

- [ ] Create `src/components/agents/AgentsOverview.tsx`: move the hero (`dashboard__hero` block, lines 174–194 of `AgentsPage.tsx`), the empty state (lines 131–140) and the grid (lines 141–150) here, unchanged, as

```tsx
export interface AgentsOverviewProps {
  agents: AgentEntry[];
  visible: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  onNewAgent: () => void;
}
export function AgentsOverview({ agents, visible, runtimeMap, onNewAgent }: AgentsOverviewProps) { /* hero + (empty | grid) */ }
```
  The mascot/greeting computations (lines 149–171) move with it. The list view (`agent-list` block, lines 151–168) and `ListRow` are deleted.
- [ ] Rewrite `src/components/agents/AgentsPage.tsx`:

```tsx
import { useEffect, useMemo, useState } from "react";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ChannelSummary } from "../../work/types";
import { useAgents } from "../../context/AgentContext";
import { useT } from "../../i18n";
import { avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import { SourceList } from "../shell/SourceList";
import type { SourceFacet, SourceRowData } from "../shell/sourceList";
import { ListDivider } from "../shell/ListDivider";
import { useResizableColumn } from "../shell/useResizableColumn";
import { statusOf } from "../shell/Status";
import { DirtyProvider, useDirtyGuard } from "../shell/dirty";
import { listModeFor } from "../shell/breakpoints";
import { useWindowWidth } from "../shell/useWindowWidth";
import { readKey, writeKey } from "../shell/persist";
import { AgentDetail } from "../detail/agent/AgentDetail";
import { AgentsOverview } from "./AgentsOverview";

export const NO_ROLE = "__none__";
export const LAST_SELECTED_AGENT_KEY = "mur.agents.lastSelected";
export const AGENTS_LIST_WIDTH_KEY = "mur.agents.listWidth";
const LIST_DEFAULT = 300;
const LIST_MIN = 240;
const LIST_MAX = 400;

export interface AgentsPageProps {
  agents: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  channels: ChannelSummary[];
  needsYou: Record<string, number>;
  selectedAgent: string | null;
  onNewAgent: () => void;
  onOpenChat: (name: string) => void;
  onOpenHome: () => void;
}

export function roleFacets(agents: AgentEntry[], noRoleLabel: string): SourceFacet[] {
  const counts: Record<string, number> = {};
  let noRole = 0;
  for (const a of agents) {
    const r = a.role?.trim();
    if (r) counts[r] = (counts[r] ?? 0) + 1;
    else noRole++;
  }
  const facets = Object.keys(counts).sort((x, y) => x.localeCompare(y)).map((r) => ({ id: r, label: r, count: counts[r] }));
  if (noRole > 0) facets.push({ id: NO_ROLE, label: noRoleLabel, count: noRole });
  return facets;
}

export function AgentsPage(props: AgentsPageProps) {
  return (
    <DirtyProvider>
      <AgentsPageInner {...props} />
    </DirtyProvider>
  );
}

function AgentsPageInner({ agents, runtimeMap, channels, needsYou, selectedAgent, onNewAgent, onOpenChat, onOpenHome }: AgentsPageProps) {
  const { t } = useT();
  const { setSelected } = useAgents();
  const { confirmLeave } = useDirtyGuard();
  const [filter, setFilter] = useState("");
  const [facet, setFacet] = useState<string | null>(null);
  const [listShown, setListShown] = useState(false); // overlay mode only
  const column = useResizableColumn(AGENTS_LIST_WIDTH_KEY, LIST_DEFAULT, LIST_MIN, LIST_MAX);
  const listMode = listModeFor(useWindowWidth());

  // Restore the last selection once agents are known (spec §6.1).
  useEffect(() => {
    if (selectedAgent !== null || agents.length === 0) return;
    const last = readKey(LAST_SELECTED_AGENT_KEY);
    if (last && agents.some((a) => a.name === last)) setSelected(last);
  }, [agents, selectedAgent, setSelected]);
  useEffect(() => {
    writeKey(LAST_SELECTED_AGENT_KEY, selectedAgent);
  }, [selectedAgent]);

  async function select(name: string | null) {
    if (name === selectedAgent) return;
    if (await confirmLeave(t("detail.discardBody"), t("detail.discardTitle"))) {
      setSelected(name);
      setListShown(false);
    }
  }

  const rows: SourceRowData[] = useMemo(
    () =>
      agents.map((a) => {
        const preset = avatarPreset(a);
        return {
          id: a.name,
          name: a.display_name,
          subtitle: [a.role?.trim(), a.model_id].filter(Boolean).join(" · "),
          status: statusOf(runtimeMap.get(a.name)?.state),
          needsYou: needsYou[a.name] ?? 0,
          avatar: <PetFace presetId={preset} family={familyOf(preset)} expression="idle" size={28} />,
          facets: [a.role?.trim() || NO_ROLE],
        };
      }),
    [agents, runtimeMap, needsYou],
  );

  const entry = agents.find((a) => a.name === selectedAgent);
  const cls = `master-detail master-detail--${listMode}${listShown ? " master-detail--list-shown" : ""}`;

  return (
    <div className={cls} style={{ ["--md-list-width" as string]: `${column.width}px` }}>
      <SourceList
        title={t("nav.agents")}
        count={agents.length}
        rows={rows}
        facets={roleFacets(agents, t("dashboard.noRole"))}
        allLabel={t("dashboard.all")}
        activeFacet={facet}
        onFacet={setFacet}
        filter={filter}
        onFilter={setFilter}
        filterPlaceholder={t("agents.filter")}
        selectedId={selectedAgent}
        onSelect={(id) => { void select(id); }}
        onCreate={onNewAgent}
        createLabel={t("app.newAgent")}
        emptyState={<p className="source-list__empty">{t("agents.noMatch")}</p>}
      />
      <ListDivider column={column} label={t("shell.resizeList")} />
      <div className="master-detail__detail">
        {listMode === "overlay" && (
          <button type="button" className="btn btn--secondary master-detail__show-list" onClick={() => setListShown((v) => !v)}>
            {t("shell.showList")}
          </button>
        )}
        {selectedAgent && entry ? (
          <AgentDetail
            key={selectedAgent}
            agentName={selectedAgent}
            entry={entry}
            runtime={runtimeMap.get(selectedAgent)}
            channels={channels}
            needsYou={needsYou[selectedAgent] ?? 0}
            onOpenChat={onOpenChat}
            onOpenHome={onOpenHome}
          />
        ) : (
          <AgentsOverview agents={agents} visible={agents} runtimeMap={runtimeMap} onNewAgent={onNewAgent} />
        )}
      </div>
    </div>
  );
}
```
  Note: `key={selectedAgent}` remounts the detail (and the dirty set) per agent — that is the cross-fade trigger from spec §5.3.
- [ ] Write `src/components/agents/agentsPage.test.ts` for `roleFacets` (two roles + no-role bucket; empty input → `[]`).
- [ ] CSS (`shell.css`): `.master-detail__detail { position: relative; min-width: 0; overflow: auto; } .master-detail--overlay { grid-template-columns: 0 0 1fr; } .master-detail--overlay .source-list, .master-detail--overlay .list-divider { display: none; } .master-detail--overlay.master-detail--list-shown .source-list { display: flex; position: absolute; inset: 0 auto 0 0; width: var(--shell-list-width-compact); z-index: 10; box-shadow: var(--shadow-pop); } .master-detail__show-list { position: absolute; top: var(--space-6); left: var(--space-6); z-index: 5; } .source-list__empty { padding: var(--space-6); color: var(--text-tertiary); font-size: var(--text-sm); }`. The `.master-detail` root needs `position: relative`.
- [ ] `src/components/chats/ChatsPage.tsx`: add `initialAgent?: string | null` to `Props`; after the `selected` state add `useEffect(() => { if (initialAgent) setSelected(initialAgent); }, [initialAgent]);`; add a local filter input at the top of `<nav className="chats-view__list">`: `<input className="source-list__filter" type="search" value={localQuery} onChange={(e) => setLocalQuery(e.target.value)} placeholder={t("chats.filter")} />` with `const [localQuery, setLocalQuery] = useState("")` and `buildChatList(agents, attention, localQuery || query)`.
- [ ] `src/components/shell/Inspector.tsx`: in `hasInspector` change `if (page === "agents") return sel.agent !== null;` to `if (page === "agents") return false;` and delete the `page === "agents"` branch in `Inspector` plus the `AgentInspector` import.
- [ ] `src/components/DashboardApp.tsx`:
  - delete the whole `<div className="dashboard__bar">…</div>` block and the `viewMode`, `query`, `searchRef` state; delete the `PlaceholderPage` function if unused (`grep -n PlaceholderPage`).
  - add `const [chatInitial, setChatInitial] = useState<string | null>(null);` and `function openChatWith(name: string) { setChatInitial(name); setPage("chats"); }`.
  - `const { channels } = useChannels();` (import from `./home/useChannels`) and `const needsYou = needsYouCounts(visibleInbox);`.
  - `<AgentsPage agents={agents} runtimeMap={runtimeMap} channels={channels} needsYou={needsYou} selectedAgent={selectedAgent} onNewAgent={() => setWizardOpen(true)} onOpenChat={openChatWith} onOpenHome={() => setPage("home")} />`; `<ChatsPage agents={agents} initialAgent={chatInitial} onActiveChange={onChatActive} />`; `<FleetView onSelect={onFleetSelect} requestedName={fleetRequest} />` (drop `query=`).
  - the Esc handler (lines 320–333): keep, but skip when `document.activeElement?.getAttribute("role") === "listbox"` (SourceList handles its own Esc).
- [ ] Delete `src/components/inspector/AgentInspector.tsx`. Delete from `dashboard.css`: `.agents-view*`, `.dashboard__bar*`, `.dashboard__brand`, `.view-toggle*`, `.agent-list*`, `.list-row*`, `.list-avatar`, `.list-name`, `.list-category`, `.list-model`; move `.dashboard__hero*`, `.dashboard__stats*`, `.stat*`, `.agent-grid`, `.grid-card*` into a new `src/styles/components/agents.css` (import after `detail-page.css`) so `dashboard.css` shrinks to the root/toolbar-btn rules still used elsewhere. Delete `.detail-panel--inspector`, `.detail-panel-tabs`, `.detail-tab*` from `detail-panel.css` only if `command grep -rn 'detail-panel-tabs\|detail-tab\b' src` shows no remaining user (the Chat/Library inspectors may still use `.detail-panel`). Keep `.sidebar*` rules: `SettingsModal` uses them.
- [ ] i18n (both): `agents.filter` "Filter agents"/"篩選 agent", `agents.noMatch` "No agents match."/"沒有符合的 agent。", `chats.filter` "Filter chats"/"篩選對話", `shell.showList` "Show list"/"顯示清單". Delete `view.grid`/`view.list` from both tables (and any other key `tsc` now reports unused is fine to keep).
- [ ] `npm test`, `npm run build`, `npm run lint`; `wc -l src/components/agents/*.tsx src/components/DashboardApp.tsx` all ≤ 800.
- [ ] Commit: `feat(hub): Agents page is master–detail — source list, full-width AgentDetail, no inspector`

**Interfaces — Consumes:** everything from PR 2/3, `needsYouCounts`, `AgentDetail`. **Produces:** `AgentsPage` props (above), `roleFacets`, `openChatWith`, `ChatsPage.initialAgent`, keys `mur.agents.lastSelected`, `mur.agents.listWidth`; classes `.master-detail--wide|compact|overlay`, `.master-detail__detail`.

**Manual acceptance PR 4** (light + dark, 1200 and 960): list keeps its width when a row is selected; window does not resize; Overview lands first; all six tabs render their content; ↑↓ move selection; ⌘F focuses the filter; Esc clears selection; chips filter by role; a pending HITL shows an amber badge on that agent's row and the strip on its Overview; editing Persona then clicking another agent asks to discard; Chat opens the Chats page on that agent; ⋯ → Export saves a `.muragent`; nothing selected → the greeting + card grid; grid cards are their full size; at 900–959 px the list is an overlay behind "Show list"; relaunch restores the last agent.

---

## PR 5 — Fleets page

Branch `feat/hub-2-fleets`. Line numbers refer to `src/components/fleet/FleetDetail.tsx`.

### Task 5.1 — `fleetActions.ts`

- [ ] Create `src/components/detail/fleet/fleetActions.ts`:

```ts
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export function showToast(msg: string, durationMs = 2500) {
  const el = document.createElement("div");
  el.className = "toast";
  el.textContent = msg;
  document.body.appendChild(el);
  setTimeout(() => el.remove(), durationMs);
}

/** The `busy` + `call()` pair every fleet tab used from FleetDetail.tsx (lines 70, 208–218). */
export function useFleetCall(onRefresh: () => void): {
  busy: string | null;
  call: (cmd: string, args: Record<string, unknown>) => Promise<void>;
} {
  const [busy, setBusy] = useState<string | null>(null);
  async function call(cmd: string, args: Record<string, unknown>) {
    setBusy(cmd);
    try {
      await invoke(cmd, args);
      onRefresh();
    } catch (err) {
      showToast(String(err), 4000);
    } finally {
      setBusy(null);
    }
  }
  return { busy, call };
}
```
- [ ] Commit: `refactor(hub): shared fleet call helper`

### Task 5.2 — Split `FleetDetail.tsx` into tabs

Every new component receives `detail: Detail` (`FleetDetail` type from `../../fleet/types`) and `onRefresh: () => void`, and takes its state, handlers and JSX from the cited line ranges **verbatim**, only re-pointing `call`/`busy` at `useFleetCall(onRefresh)` and `showToast` at `./fleetActions`.

- [ ] `src/components/detail/fleet/FleetHeader.tsx` — props `{ detail; running: boolean; onRefresh; onDelete: () => void }`. State/handlers: lines 220–250 (worktree, loop panel, `handleRunLoop`, `handleRun`), 251–265 (`handleSend`, plus `const [sendInput, setSendInput] = useState("")` from line 71 and a new `const [sendOpen, setSendOpen] = useState(false)`), 282–329 (`handleExport`, `handleImport`, `handleDelete`). Renders (used by `FleetView` as the `DetailPage` `actions`):

```tsx
  <>
    <SplitButton
      label={<>▶ {t("fleet.run")}</>}
      onPrimary={() => { void handleRun(); }}
      disabled={busy !== null || detail.stopped}
      menuLabel={t("fleet.runOptions")}
      items={[
        { id: "loop", label: t("fleet.run.loop"), onSelect: () => setLoopOpen(true) },
        { id: "send", label: t("fleet.send"), onSelect: () => setSendOpen(true) },
        ...(detail.parallel_summary ? [{ id: "worktree", label: worktree ? t("fleet.run.worktreeOn") : t("fleet.run.worktreeOff"), onSelect: () => setWorktree((v) => !v) }] : []),
      ]}
    />
    {detail.stopped ? (
      <button type="button" className="btn btn--primary" onClick={() => call("fleet_start", { name: detail.name })} disabled={busy !== null}>{t("fleet.start")}</button>
    ) : (
      <button type="button" className="btn btn--secondary" onClick={() => call("fleet_stop", { name: detail.name })} disabled={busy !== null}>{t("fleet.stop")}</button>
    )}
    <OverflowMenu label={t("action.more")} items={[
      { id: "export", label: t("fleet.export"), onSelect: () => { void handleExport(); } },
      { id: "import", label: t("fleet.import"), onSelect: () => { void handleImport(); } },
      { id: "delete", label: t("fleet.delete"), danger: true, onSelect: () => { void handleDelete(); } },
    ]} />
    {loopOpen && (/* the loop-row inputs + Go button, lines 403–431, inside a <div className="fleet-popover"> with a Cancel button */)}
    {sendOpen && (/* the send-job input + button, lines 700–712 (handleSend lines 251–265), inside a <div className="fleet-popover"> */)}
  </>
```
  `FleetHeader` also exports `fleetMeta(detail, t)`: the meta line `Router <router> · <n> members · <channel_id mono> · <mode badge>` (lines 372–378 content).
- [ ] `src/components/detail/fleet/FleetOverview.tsx` — props `{ detail; jobs: JobRow[]; agentMap; onGoTo: (tab: FleetTabId) => void }`. Renders: goal card (`.detail-card` + `.goal`), `.detail-stats` row (`loop_cfg.last_run ?? t("fleetInspector.never")`, `loop_cfg.max_iterations`, `$loop_cfg.budget_usd`, `loop_cfg.done_when || "router"`; all `—` when `loop_cfg` is null), a `.detail-two` with members chips (router first, from `detail.members`, using `PetFace` via `agentMap`) and the first three `jobs` as `.detail-kv` rows with `StatusPill`-free text status (`t(\`fleet.status.${job.status}\`)`), and a loop card (`trigger`, `deadline`, `done_when`) whose buttons call `onGoTo("settings")`.
- [ ] `src/components/detail/fleet/FleetMembers.tsx` — props `{ detail; agentMap; labels: LabelView[]; fleetLabels: string[]; onRefresh }`. Lines 173–207 (`saveLabels`, `createLabel`), 267–281 (`handleAddMember`), 449–568 (labels block + members block JSX).
- [ ] `src/components/detail/fleet/FleetJobs.tsx` — props `{ detail; jobs: JobRow[]; onRefresh }`. Lines 74–75 (`showAll`, `allJobs`), 330–360 (`handleCancelJob`, `handleShowAll`), 692–740 minus the send-job input (moved to the header).
- [ ] `src/components/detail/fleet/FleetSettings.tsx` — props `{ detail; onRefresh; onDelete }`. Lines 78–146 (trigger/cron/loop/done state + effects), 147–172 (`handleSaveSettings`), 361–366 (`applyCronShape`), 570–690 (form JSX), and the Danger zone (lines 741–750) last, wired to the same `handleDelete` as the header (import it from `FleetHeader` or duplicate the 12-line function — do not add a third variant).
- [ ] i18n (both): `fleet.runOptions` "More run options"/"更多執行方式", `fleet.run.worktreeOn` "Worktree per track: on"/"每軌 worktree：開", `fleet.run.worktreeOff` "Worktree per track: off"/"每軌 worktree：關", `fleet.filter` "Filter fleets"/"篩選機群", `fleet.noMatch` "No fleets match."/"沒有符合的機群。", `fleet.cancel` "Cancel"/"取消". Reuse existing `fleet.*` keys everywhere else.
- [ ] CSS `src/styles/components/fleet.css`: add `.fleet-popover { position:absolute; top:calc(100% + 6px); right:0; z-index:20; background:var(--surface-card); border:1px solid var(--border-line); border-radius:var(--radius-md); box-shadow:var(--shadow-pop); padding:var(--space-5); display:flex; gap:8px; }` and `.detail-page__actions { position: relative; }`; `.goal { margin:0; font-size:13.5px; line-height:1.55; max-width:62ch; }`.
- [ ] `npm run build`; `wc -l src/components/detail/fleet/*.tsx` ≤ 800 each. Commit: `refactor(hub): FleetDetail split into Header, Overview, Members, Jobs, Settings`

**Interfaces — Produces:** `<FleetHeader detail running onRefresh onDelete>`, `fleetMeta`, `<FleetOverview detail jobs agentMap onGoTo>`, `<FleetMembers detail agentMap labels fleetLabels onRefresh>`, `<FleetJobs detail jobs onRefresh>`, `<FleetSettings detail onRefresh onDelete>`.

### Task 5.3 — `FleetView` on `SourceList`

- [ ] Rewrite the render of `src/components/fleet/FleetView.tsx` (keep every data function: `loadList`, `loadLabels`, `loadDetail`, the effects, `handleRefresh`, `handleDelete`, `handleCreated`, the `fleet:run_done` listener, the `LABEL_FILTER_KEY` persistence — now storing at most one id):

```tsx
  const [tab, setTab] = useState<FleetTabId>("overview");
  const [filter, setFilter] = useState("");
  const column = useResizableColumn(FLEETS_LIST_WIDTH_KEY, LIST_DEFAULT, LIST_MIN, LIST_MAX);
  const listMode = listModeFor(useWindowWidth());
  const [listShown, setListShown] = useState(false);
  const activeLabel = selectedLabels[0] ?? null;

  useEffect(() => { setTab("overview"); }, [selectedName]);

  const rows: SourceRowData[] = fleets.map((f) => ({
    id: f.name,
    name: f.display_name,
    subtitle: t("fleet.rowSubtitle", { count: f.member_count }),
    status: fleetStatusOf(f),
    needsYou: f.active_jobs,
    avatar: <span className="fleet-avatar" aria-hidden="true"><Ico><path d="M12 4l9 4.5-9 4.5-9-4.5z" /><path d="M3 13l9 4.5 9-4.5" /></Ico></span>,
    facets: f.labels.length > 0 ? f.labels : [UNGROUPED],
  }));
  const facets: SourceFacet[] = [
    ...labels.map((l) => ({ id: l.id, label: l.display || l.id, count: l.fleet_count })),
    ...(fleets.some((f) => f.labels.length === 0) ? [{ id: UNGROUPED, label: t("fleet.labelUngrouped"), count: fleets.filter((f) => f.labels.length === 0).length }] : []),
  ];
  const summary = fleets.find((f) => f.name === detail?.name);

  return (
    <div className={`master-detail master-detail--${listMode}${listShown ? " master-detail--list-shown" : ""}`} style={{ ["--md-list-width" as string]: `${column.width}px` }}>
      <SourceList
        title={t("nav.fleets")} count={fleets.length} rows={rows} facets={facets} allLabel={t("fleet.labelAll")}
        activeFacet={activeLabel} onFacet={(id) => setSelectedLabels(id ? [id] : [])}
        filter={filter} onFilter={setFilter} filterPlaceholder={t("fleet.filter")}
        selectedId={selectedName} onSelect={(id) => { setSelectedName(id); setListShown(false); }}
        onCreate={() => setShowCreate(true)} createLabel={t("fleet.new")}
        emptyState={<p className="source-list__empty">{fleets.length === 0 ? t("fleet.empty") : t("fleet.noMatch")}</p>}
      />
      <ListDivider column={column} label={t("shell.resizeList")} />
      <div className="master-detail__detail">
        {listMode === "overlay" && (<button type="button" className="btn btn--secondary master-detail__show-list" onClick={() => setListShown((v) => !v)}>{t("shell.showList")}</button>)}
        {detail && summary ? (
          <DetailPage
            key={detail.name}
            avatar={<span className="fleet-avatar fleet-avatar--lg" aria-hidden="true"><Ico><path d="M12 4l9 4.5-9 4.5-9-4.5z" /><path d="M3 13l9 4.5 9-4.5" /></Ico></span>}
            title={detail.display_name}
            status={fleetStatusOf(summary)}
            meta={fleetMeta(detail, t)}
            actions={<FleetHeader detail={detail} running={summary.running} onRefresh={handleRefresh} onDelete={handleDelete} />}
            tabs={FLEET_TABS.map((id) => ({ id, label: t(FLEET_TAB_LABEL_KEY[id]) }))}
            activeTab={tab}
            onTab={setTab}
          >
            {tab === "overview" && <FleetOverview detail={detail} jobs={jobs} agentMap={agentMap} onGoTo={setTab} />}
            {tab === "members" && <FleetMembers detail={detail} agentMap={agentMap} labels={labels} fleetLabels={summary.labels} onRefresh={handleRefresh} />}
            {tab === "jobs" && <FleetJobs detail={detail} jobs={jobs} onRefresh={handleRefresh} />}
            {tab === "settings" && <FleetSettings detail={detail} onRefresh={handleRefresh} onDelete={handleDelete} />}
          </DetailPage>
        ) : (
          <div className="fleet-view__empty"><p>{t("fleet.empty")}</p></div>
        )}
      </div>
      {showCreate && <FleetCreateModal onCreated={handleCreated} onClose={() => setShowCreate(false)} />}
    </div>
  );
```
  Constants: `export const FLEETS_LIST_WIDTH_KEY = "mur.fleets.listWidth"; export const LAST_SELECTED_FLEET_KEY = "mur.fleets.lastSelected";` with `LIST_DEFAULT/MIN/MAX` as in `AgentsPage`. Last-selection: in `loadList`, replace the "Auto-select first" branch with `const last = readKey(LAST_SELECTED_FLEET_KEY); setSelectedName(last && rows.some((r) => r.name === last) ? last : rows[0]?.name ?? null);` and add `useEffect(() => { writeKey(LAST_SELECTED_FLEET_KEY, selectedName); }, [selectedName]);`. `Ico` is imported from `../agents/GridCard`. `filterByLabels` from `./fleetLabels` is no longer needed by the view (the facet does it); leave the helper and its tests in place.
- [ ] i18n (both): `fleet.rowSubtitle` "{count} members"/"{count} 位成員".
- [ ] CSS `fleet.css`: delete `.fleet-view`, `.fleet-view__main`, all `.fleet-rail*`, `.fleet-chip*`, `.fleet-detail__header`, `__title-row`, `__title`, `__goal`, `__router`, `__run*`, `__mgmt`, `__danger*` rules that no file references (`command grep -rn '<class>' src` per class before deleting); add `.fleet-avatar { display:grid; place-items:center; width:28px; height:28px; border-radius:var(--radius-md); background:var(--surface-secondary); color:var(--text-secondary); } .fleet-avatar--lg { width:48px; height:48px; border-radius:var(--radius-lg); }`.
- [ ] Delete `src/components/fleet/FleetDetail.tsx`, `src/components/fleet/FleetRail.tsx`, `src/components/inspector/FleetInspector.tsx`; in `src/components/shell/Inspector.tsx` make `hasInspector` return `false` for `fleets` and remove the fleets branch + import; in `DashboardApp.tsx` drop the `fleetName` state and `onFleetSelect` if nothing else reads them (`grep -n fleetName`). Remove the `fleetInspector.*` keys from both i18n tables **except** `fleetInspector.never` (used by `FleetOverview`) — or move that string to `fleet.never`.
- [ ] `npm test`, `npm run build`, `npm run lint`; `wc -l src/components/fleet/FleetView.tsx` ≤ 800.
- [ ] Commit: `feat(hub): Fleets page is master–detail — label chips, four tabs, Run split button, no duplicate inspector`

**Manual acceptance PR 5** (light + dark, 1200 and 960): only one copy of goal/members/loop on screen; Run ▾ offers Run as loop / Send job / worktree toggle; loop inputs validate as before; a `.stopped` fleet shows Stopped in red with Start as primary and Run disabled; label chips filter and the choice survives relaunch; Members add/remove and label edits still save; Jobs cancel and show-all work; Settings save + cron preview work; Delete asks to confirm; last fleet restored on relaunch; palette jump to a fleet lands on it.

---

## Spec coverage

| Spec § | Task |
|---|---|
| 3.1 panes, breakpoints, ⌘\, divider, no auto-resize, window size | 2.1, 2.2, 2.3, 2.4, 4.5, 5.3 |
| 3.2 title bar | 2.3 |
| 3.3 global toolbar retired | 2.2 (Settings), 3.5 (search), 4.5 (bar removed, New Agent → list "+", ⌘R) |
| 3.4 inspector retired; overview when nothing selected | 4.5, 5.3 |
| 4.1 SourceList | 3.1 |
| 4.2 DetailPage + dirty guard | 3.2, 4.4 |
| 4.3 agent tabs 11→6, deep links, header actions | 3.4, 4.3 |
| 4.4 fleet tabs, split button, stopped state | 5.2, 5.3 |
| 4.5 status vocabulary | 1.1, 1.3 |
| 4.6 SplitButton | 3.3 |
| 5.1–5.3 tokens, type, motion, a11y | 1.2, 3.1–3.3 CSS, 2.2 (reduced motion) |
| 5.4 global recolor side effect | 1.2 |
| 6.1 selection persistence | 4.5, 5.3 |
| 6.2 deep links | 4.3 |
| 6.3 needs-you | 4.1, 4.5 |
| 6.4 overview data, "—" where no source | 4.2, 4.3, 5.2 |
| 6.5 dirty guard | 3.2, 4.4, 4.5 |
| 6.6 keyboard map | 3.1 (⌘F, ↑↓, Esc), 3.5 (⌘K, ⌘R), 2.2 (⌘\); ⌘1–9 → **not in Phase 1** (see below) |
| 7 errors and empty states | 4.3 (banners), 4.5, 5.3 (empty states) |
| 8 PR order and file limits | this plan's structure |
| 9 tests | every task's test step |

Deliberate gaps, agreed as follow-ups: ⌘1–9 page switching (the palette covers jumping; add when a page-order shortcut is asked for); agent Duplicate/Delete in the ⋯ menu (no Tauri command exists — `delete_agent`/`duplicate_agent` are not in `src-tauri`); fleet iterations-used / budget-spent (no Hub source; spec §4.4).
