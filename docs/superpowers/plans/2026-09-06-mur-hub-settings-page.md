# MUR Hub 2.0 — Phase 3(d) Settings as a page — implementation plan

> **Execute with `mur-executing-plans`.** Spec: `docs/superpowers/specs/2026-09-06-mur-hub-settings-page-design.md` (§ references below point there). One PR (**PR 13**), three tasks, each commit builds.

## Goal

Settings is a page on the shell — a section nav beside full-width section content — reached from the sidebar footer, ⌘,, ⌘K, and the model wizard; the modal is gone.

## Architecture

`PageId` gains `"settings"` (not a nav item). `SettingsPage` owns the current section (persisted, one-shot restore, a `requestedSection` prop wins) and renders the five existing section components unchanged inside its own two-column grid. `DashboardApp` routes every opener to `setPage("settings")` and drops `SettingsModal`.

## Tech stack

React 18 + TypeScript 5.5 + Vite 5, plain CSS on the two-tier tokens, Vitest 4 without jsdom, the lightweight i18n. No Rust (the app menu already emits `open-settings`).

## Global Constraints

Copied from the design and `CLAUDE.md`. Every task includes all of them.

1. Brand name is uppercase **MUR** in every user-visible string.
2. Single source file ≤ 800 lines.
3. Every new user-visible string lands in both `src/i18n/en.ts` and `src/i18n/zh-TW.ts` in the same commit (this plan adds none: `settings.title`, `settings.nav.*`, `app.settings` exist).
4. Components reference only semantic tokens; no raw hex in component CSS or TSX.
5. No hardcoded numbers or storage keys in TSX: named constants.
6. Never pair `Foo.tsx` with `foo.ts` in one directory (APFS is case-insensitive): the pure model is `settingsSections.ts`, the glyphs `settingsGlyphs.tsx`, the page `SettingsPage.tsx`.
7. Tests never touch the DOM: pure functions, or `renderToStaticMarkup` for markup (`SettingsPage` uses `useT`, so only the model is unit-tested).
8. Every commit is gated on the real exit code: `set -o pipefail; npm test 2>&1 | grep …` — never on grep's.
9. The five section components render unchanged; the page adds no header of its own.
10. Every PR leaves the app usable: `npm run build`, `npm test`, `npm run lint` green and the manual acceptance list passes.

## Working agreement

- Paths are relative to `mur-hub-gui/ui/`.
- Line numbers cite `main` at `108e1f98` (2026-09-06); re-check with `grep -n` before cutting.
- Commands from `mur-hub-gui/ui/`: `npm test -- <path>`, `npm test`, `npm run build`, `npm run lint`. `npm run lint` reports 6 pre-existing warnings in files this plan does not touch; 0 errors is the bar.
- Browser acceptance: `npm run dev -- --port 5174 --strictPort`, the stored-in-`sessionStorage` Tauri stub from the Phase 3(b) plan, `Try again` clicked by text; `open-settings` fired through the stub's stored listener id.
- Commit after every task with the message given.

## File structure

| File | Responsibility |
|---|---|
| `src/components/shell/nav.ts` (+ `nav.test.ts`) (modify) | `PageId` + `"settings"`, `isPageId` accepts it |
| `src/components/shell/Sidebar.tsx` (modify) | footer button active on the settings page; `GLYPHS.settings` |
| `src/components/settings/settingsSections.ts` (+ `.test.ts`) (new) | `SettingsSectionId`, `SETTINGS_SECTIONS`, `isSettingsSection`, `LAST_SECTION_KEY` |
| `src/components/settings/settingsGlyphs.tsx` (new) | `SETTINGS_GLYPHS` |
| `src/components/settings/SettingsPage.tsx` (new) | the page |
| `src/components/settings/DataSettings.tsx` (modify) | `onClose` removed |
| `src/styles/components/settings-page.css` (new), `src/styles/index.css`, `src/styles/tokens/primitives.css` (modify) | `.settings-page*`, `.settings-nav*`, `--settings-nav-width` |
| `src/components/DashboardApp.tsx` (modify) | openers → `setPage("settings")`, `settingsRequest`, the page switch; modal removed |
| `src/components/SettingsModal.tsx` (delete) | — |
| `src/styles/components/dashboard.css` (modify) | modal-era settings and legacy `.sidebar*` rules removed |

---

### Task 13.1 — `PageId` + sidebar

**Interfaces.** Produces `PageId` including `"settings"`, `isPageId("settings") === true`, `GLYPHS.settings`, and the footer's active state. 13.3 consumes `setPage("settings")`.

- [ ] `src/components/shell/nav.test.ts`: in the `isPageId` describe add `expect(isPageId("settings")).toBe(true);` after the `home` line. `npm test -- src/components/shell/nav.test.ts` → fails.
- [ ] `src/components/shell/nav.ts`: add `| "settings"` to the `PageId` union (after `"plugins"`), and replace `isPageId` with:
  ```ts
  /** Type guard for page ids arriving over events (`open-page`). Settings is a
   *  page without a nav item (spec 3(d) §3), so it is listed here explicitly. */
  export function isPageId(id: string): id is PageId {
    return id === "settings" || NAV_ITEMS.some((n) => n.id === id);
  }
  ```
- [ ] `src/components/shell/Sidebar.tsx`: `GLYPHS` is `Record<PageId, ReactNode>` and now needs a `settings` entry — move the `const GEAR = (…)` block above `GLYPHS` and add `settings: GEAR,` as its last entry. The footer button becomes:
  ```tsx
        <button
          type="button"
          className={`shell-sidebar-item${active === "settings" ? " shell-sidebar-item--active" : ""}`}
          onClick={onSettings}
          aria-current={active === "settings" ? "page" : undefined}
          title={t("app.settings")}
        >
  ```
- [ ] `npm test` (all pass), `npm run build`, `npm run lint`. (`DashboardApp`'s `page === … ? … : <PlaceholderPage id={page} />` chain still compiles: `"settings"` falls through to the placeholder until 13.3.)
- [ ] Commit: `feat(hub): settings is a PageId; the sidebar footer lights up on it`

### Task 13.2 — sections model, glyphs, `SettingsPage`, CSS, `DataSettings`

**Interfaces.** Produces `SettingsSectionId`, `SETTINGS_SECTIONS`, `isSettingsSection`, `LAST_SECTION_KEY`, `SETTINGS_GLYPHS`, `SettingsPage({ requestedSection?, onRequestHandled?, onImportAgent, onImportPreset })`, and `DataSettings` without `onClose`. 13.3 consumes `SettingsPage`.

- [ ] Create `src/components/settings/settingsSections.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { SETTINGS_SECTIONS, isSettingsSection } from "./settingsSections";

describe("SETTINGS_SECTIONS", () => {
  it("lists the five sections in order, ids unique", () => {
    const ids = SETTINGS_SECTIONS.map((s) => s.id);
    expect(ids).toEqual(["general", "models", "updates", "data", "about"]);
    expect(new Set(ids).size).toBe(ids.length);
  });
  it("isSettingsSection accepts each id and rejects strangers", () => {
    for (const s of SETTINGS_SECTIONS) expect(isSettingsSection(s.id)).toBe(true);
    expect(isSettingsSection("nope")).toBe(false);
  });
});
```
- [ ] `npm test -- src/components/settings/settingsSections.test.ts` → fails (module missing).
- [ ] Create `src/components/settings/settingsSections.ts`:

```ts
import type { TranslationKey } from "../../i18n/types";

/** The Settings page's sections (spec 3(d) §4), in nav order. */
export type SettingsSectionId = "general" | "models" | "updates" | "data" | "about";

export const SETTINGS_SECTIONS: { id: SettingsSectionId; labelKey: TranslationKey }[] = [
  { id: "general", labelKey: "settings.nav.general" },
  { id: "models", labelKey: "settings.nav.models" },
  { id: "updates", labelKey: "settings.nav.updates" },
  { id: "data", labelKey: "settings.nav.data" },
  { id: "about", labelKey: "settings.nav.about" },
];

export function isSettingsSection(id: string): id is SettingsSectionId {
  return SETTINGS_SECTIONS.some((s) => s.id === id);
}

export const LAST_SECTION_KEY = "mur.settings.lastSection";
```
- [ ] `npm test -- src/components/settings/settingsSections.test.ts` → 2 passed.
- [ ] Create `src/components/settings/settingsGlyphs.tsx`:

```tsx
import type { ReactNode } from "react";
import type { SettingsSectionId } from "./settingsSections";

/** 24-unit stroke paths for the section nav (rendered through `Ico`). */
export const SETTINGS_GLYPHS: Record<SettingsSectionId, ReactNode> = {
  general: (
    <>
      <circle cx="12" cy="12" r="3" />
      <path d="M12 2v3M12 19v3M2 12h3M19 12h3M4.9 4.9l2.1 2.1M17 17l2.1 2.1M19.1 4.9L17 7M7 17l-2.1 2.1" />
    </>
  ),
  models: <path d="M4 7a8 8 0 0 1 16 0v10a8 8 0 0 1-16 0Zm0 0h16" />,
  updates: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 16V8M8.5 11.5 12 8l3.5 3.5" />
    </>
  ),
  data: (
    <>
      <path d="M3 8l9-4 9 4-9 4Z" />
      <path d="M3 8v9l9 4 9-4V8M12 12v9" />
    </>
  ),
  about: (
    <>
      <circle cx="12" cy="12" r="9" />
      <path d="M12 11v5M12 8h.01" />
    </>
  ),
};
```
- [ ] Create `src/components/settings/SettingsPage.tsx`:

```tsx
import { useEffect, useRef, useState } from "react";
import { useT } from "../../i18n";
import { Ico } from "../agents/GridCard";
import { readKey, writeKey } from "../shell/persist";
import { GeneralSettings } from "./GeneralSettings";
import { ModelsSettings } from "./ModelsSettings";
import { UpdatesSettings } from "./UpdatesSettings";
import { DataSettings } from "./DataSettings";
import { AboutSettings } from "./AboutSettings";
import { SETTINGS_GLYPHS } from "./settingsGlyphs";
import {
  LAST_SECTION_KEY, SETTINGS_SECTIONS, isSettingsSection, type SettingsSectionId,
} from "./settingsSections";

const DEFAULT_SECTION: SettingsSectionId = "general";

export interface SettingsPageProps {
  /** A deep link (the model wizard's Customize → "models"); consumed once. */
  requestedSection?: SettingsSectionId | null;
  onRequestHandled?: () => void;
  onImportAgent: () => void;
  onImportPreset: () => void;
}

/** Settings on the shell (spec 3(d)): section nav | section content. */
export function SettingsPage({ requestedSection, onRequestHandled, onImportAgent, onImportPreset }: SettingsPageProps) {
  const { t } = useT();
  const [section, setSection] = useState<SettingsSectionId>(() => {
    const last = readKey(LAST_SECTION_KEY);
    return last && isSettingsSection(last) ? last : DEFAULT_SECTION;
  });
  // The mount render must not write the default over a stored section
  // before a request has had its say; write from the first change on.
  const mounted = useRef(false);
  useEffect(() => {
    if (mounted.current) writeKey(LAST_SECTION_KEY, section);
    mounted.current = true;
  }, [section]);

  useEffect(() => {
    if (!requestedSection) return;
    setSection(requestedSection);
    onRequestHandled?.();
  }, [requestedSection, onRequestHandled]);

  return (
    <div className="settings-page">
      <nav className="settings-nav" aria-label={t("settings.title")}>
        {SETTINGS_SECTIONS.map((s) => {
          const on = s.id === section;
          return (
            <button
              key={s.id}
              type="button"
              className={`settings-nav__item${on ? " settings-nav__item--active" : ""}`}
              aria-current={on ? "page" : undefined}
              onClick={() => setSection(s.id)}
            >
              <span className="settings-nav__icon"><Ico>{SETTINGS_GLYPHS[s.id]}</Ico></span>
              <span className="settings-nav__label">{t(s.labelKey)}</span>
            </button>
          );
        })}
      </nav>
      <div className="settings-page__content">
        {section === "general" && <GeneralSettings />}
        {section === "models" && <ModelsSettings />}
        {section === "updates" && <UpdatesSettings />}
        {section === "data" && <DataSettings onImportAgent={onImportAgent} onImportPreset={onImportPreset} />}
        {section === "about" && <AboutSettings />}
      </div>
    </div>
  );
}
```
- [ ] `src/components/settings/DataSettings.tsx`: remove `onClose: () => void;` from `Props`, remove `onClose` from the destructuring, and change the two handlers `onClick={() => { onClose(); onImportAgent(); }}` / `onClick={() => { onClose(); onImportPreset(); }}` to `onClick={onImportAgent}` / `onClick={onImportPreset}`. In `src/components/SettingsModal.tsx` (still present until 13.3) drop the `onClose={onClose}` line from its `<DataSettings …/>` so the commit compiles.
- [ ] Token and CSS. `src/styles/tokens/primitives.css` line 46: append ` --settings-nav-width:200px;` after `--shell-sidebar-collapsed-width:56px;`. Create `src/styles/components/settings-page.css` and add `@import "./components/settings-page.css";` after the `bulk.css` line in `src/styles/index.css`:

```css
/* Settings page (Phase 3(d) §5): section nav | section content. */
.settings-page { display: grid; grid-template-columns: var(--settings-nav-width) 1fr; height: 100%; min-height: 0; }
.settings-nav {
  display: flex; flex-direction: column; gap: 2px; padding: var(--space-6) var(--space-4);
  border-right: 1px solid var(--border-line); background: var(--surface-list);
}
.settings-nav__item {
  display: flex; align-items: center; gap: var(--space-4); padding: 7px 10px; border: 0; border-radius: var(--radius-md);
  background: transparent; color: var(--text-secondary); font: inherit; font-size: var(--text-sm); text-align: left; cursor: pointer;
}
.settings-nav__item:hover { background: var(--surface-hover); color: var(--text-primary); }
.settings-nav__item--active { background: var(--color-brand-soft); color: var(--text-primary); }
.settings-nav__icon { display: flex; width: 18px; height: 18px; }
.settings-page__content { min-width: 0; overflow: auto; padding: var(--space-7) var(--space-8) var(--space-9); background: var(--surface-detail); }
```
- [ ] `npm test` (all pass), `npm run build`, `npm run lint`.
- [ ] Commit: `feat(hub): SettingsPage — section nav beside the existing section components`

### Task 13.3 — `DashboardApp` wiring, modal deletion, CSS cleanup

**Interfaces.** Consumes 13.1 and 13.2. Produces the four openers on the page; `SettingsModal` gone.

- [ ] `src/components/DashboardApp.tsx`:
  - Replace `import { SettingsModal } from "./SettingsModal";` with `import { SettingsPage } from "./settings/SettingsPage";` and add `import type { SettingsSectionId } from "./settings/settingsSections";`.
  - Replace `const [settingsOpen, setSettingsOpen] = useState(false);` with:
    ```tsx
    // Settings is a page (spec 3(d)); the wizard can deep-link a section.
    const [settingsRequest, setSettingsRequest] = useState<SettingsSectionId | null>(null);
    const clearSettingsRequest = useCallback(() => setSettingsRequest(null), []);
    const openSettings = useCallback((section: SettingsSectionId | null = null) => {
      setSettingsRequest(section);
      setPage("settings");
    }, []);
    ```
  - `open-settings` listener: `listen("open-settings", () => setSettingsOpen(true))` → `listen("open-settings", () => openSettings())`, and add `openSettings` to that effect's dependency array.
  - Palette: `run: () => setSettingsOpen(true)` → `run: () => openSettings()`.
  - Shell: `onSettings={() => setSettingsOpen(true)}` → `onSettings={() => openSettings()}`.
  - Wizard: in `<ModelSetupWizard … onCustomize={() => { setShowModelWizard(false); setSettingsOpen(true); }} />` replace `setSettingsOpen(true)` with `openSettings("models")`.
  - Page switch: before `) : (\n            <PlaceholderPage id={page} />` insert
    ```tsx
          ) : page === "settings" ? (
            <SettingsPage
              requestedSection={settingsRequest}
              onRequestHandled={clearSettingsRequest}
              onImportAgent={() => setMuragentImportOpen(true)}
              onImportPreset={() => setPresetImportOpen(true)}
            />
    ```
  - Delete the `<SettingsModal … />` element (five lines).
  - `grep -n 'settingsOpen\|SettingsModal' src/components/DashboardApp.tsx` → none.
- [ ] Delete `src/components/SettingsModal.tsx`. `grep -rn 'SettingsModal\|settings-modal\|className="sidebar\|sidebar-item' src --include='*.tsx'` → none.
- [ ] `src/styles/components/dashboard.css`: delete the legacy sidebar block — the rules `.sidebar { … }`, `.sidebar-item { … }`, `.sidebar-item__icon { … }`, `.sidebar-item:hover { … }`, `.sidebar-item--active { … }`, and `.sidebar-item--active .badge { … }` (lines 16–50 and 60) — keeping `.badge { … }` only if `grep -rn 'className="badge\|"badge"' src --include='*.tsx'` finds a user (delete it too otherwise). Delete the modal-era settings block: the comment starting `/* ── Settings two-pane layout ── */` through `.settings-modal__content { … }` (lines 666–690). Keep `.settings-row--wrap*` (the chain editor uses it) and every `.settings-section*` / `.settings-row*` / `.settings-hint` / `.settings-actions` rule.
- [ ] `npm test`, `npm run build`, `npm run lint` (0 errors).
- [ ] Browser acceptance (stubbed bridge; the stub answers `get_fleet_autorun → false`, `nudge_status → null`, `model_slots_get` / `model_switch_get` / `list_models` / `probe_local_providers` with empty shapes, `cli_version_skew → null`): the sidebar footer button opens the page and gets the active style; ⌘K → 設定 opens it; a fired `open-settings` (stored listener) opens it; with the page open, the five nav items switch content and each shows its own `h3`; switch to Agents and back → the last section is restored; set `localStorage['mur.settings.lastSection']='nope'`, remount → General; the wizard path is exercised by calling the stub's stored `need-model` listener, clicking Customize → Settings on Models; Import agent opens the import modal and closing it leaves the Settings page; `document.querySelector('.settings-modal')` is null; ⌘1 still goes Home.
- [ ] Commit: `refactor(hub): Settings opens as a page; SettingsModal and the legacy sidebar CSS retired`

**Manual acceptance PR 13 (real build):** the app menu's Settings… (⌘,) lands on the page; the Models section's registry rows no longer wrap; light and dark; the 900px minimum window still shows nav + content.

## Spec coverage

| Spec § | Task |
|---|---|
| 3 routing, entry points, sidebar | 13.1, 13.3 |
| 4 sections model, glyphs, page, `DataSettings` | 13.2 |
| 5 CSS, token, cleanup | 13.2 (new), 13.3 (deletions) |
| 6 keyboard | nothing to build; 13.3 acceptance checks ⌘1 |
| 7 edge cases | 13.2 (`isSettingsSection` on restore), 13.1 (`isPageId`) |
| 8 tests | 13.1 (`nav.test`), 13.2 (`settingsSections.test`), 13.3 (browser) |
