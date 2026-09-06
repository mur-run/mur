# MUR Hub 2.0 — Phase 3(d): Settings as a page

**Date:** 2026-09-06 · **Status:** Draft — awaiting review
**Follows:** `2026-09-06-mur-hub-master-detail-shell-design.md` (§3 shell, §10 Phase 3), `2026-09-06-mur-hub-multiselect-design.md` (Phase 3(c), #1181–#1182). Last item of Phase 3.
**Scope:** `mur-hub-gui/ui` only. No Rust change (the app menu's ⌘, already emits `open-settings`).

## 1. Problem

Settings is the last surface that is still a modal: a 70vh box with a 180px nav and a content pane whose width the modal fights over (the `.modal.settings-modal--paned` comment records the cascade battle). The model registry rows inside it were designed for a 900px-wide library and wrap in the box. Every other surface now lives on the shell; Settings should too.

## 2. Decisions

| Question | Decision | Rejected |
|---|---|---|
| Left column | **A plain section nav**: five buttons (SVG glyph + label), the sidebar item's selected look, no filter / count / chips. | `SourceList`: a filter field over five static items reads as decoration. |
| Entry / exit | **Settings is a `PageId`** (`"settings"`), not a `NAV_ITEMS` entry: the sidebar footer button is the entry and lights up on that page; ⌘, (`open-settings`), the ⌘K action, and the model wizard's Customize all navigate there. No Esc, no close button — it is a page. `SettingsModal` is deleted. | Settings in the nav list as a third group: a group of one, and it would take a ⌘-digit. |
| Section state | **The page owns it**, persisted (`mur.settings.lastSection`) and restored once; a `requestedSection` prop (wizard → Models) wins, the `FleetView.requestedName` pattern. | Section in `DashboardApp`: only the wizard needs to reach in. |
| Layout | **Its own two-column grid** (`--settings-nav-width: 200px` + content), no `ListDivider`, no overlay mode. At the shell's 960px breakpoint the sidebar collapses to its rail, which leaves the settings nav and content plenty of room down to the 900px window minimum. | The `master-detail` grid: a draggable divider and a persisted width for five static items. |
| Section content | **Unchanged components**: `GeneralSettings`, `ModelsSettings`, `UpdatesSettings`, `DataSettings`, `AboutSettings` render as they are (each already has its `h3` title), so the page adds no header of its own. | A `DetailHeader` above each section: it would repeat the section's own title. |
| `DataSettings.onClose` | **Removed.** Its two import buttons called `onClose()` so the modal got out of the import dialog's way; the dialog overlays the page fine. | — |

## 3. Routing and entry points

- `nav.ts`: `PageId` gains `"settings"`. `NAV_ITEMS` is unchanged. `isPageId` accepts `"settings"` in addition to the nav ids (so `open-page` events can target it).
- `Sidebar.tsx`: the footer button gets `shell-sidebar-item--active` and `aria-current="page"` when `active === "settings"`. `GLYPHS: Record<PageId, ReactNode>` gains `settings: GEAR` (the constant already there).
- `DashboardApp.tsx`: `settingsOpen` / `setSettingsOpen` / the `SettingsModal` render and import go away. The four openers become `setPage("settings")`: the Shell's `onSettings`, the palette's `action:settings`, the `open-settings` listener, and `ModelSetupWizard.onCustomize` — the last one also sets `settingsRequest = "models"`. The page switch gains `page === "settings" ? <SettingsPage requestedSection={settingsRequest} onRequestHandled={clearSettingsRequest} onImportAgent={…} onImportPreset={…} /> : …` before the `PlaceholderPage` fallback.

## 4. `SettingsPage`

- **`components/settings/settingsSections.ts`** (pure): `export type SettingsSectionId = "general" | "models" | "updates" | "data" | "about"`, `SETTINGS_SECTIONS: { id; labelKey: TranslationKey }[]` in that order (the modal's `NAV` minus the emoji), `isSettingsSection(id: string): id is SettingsSectionId`, `LAST_SECTION_KEY = "mur.settings.lastSection"`. Glyphs live beside the component (`settingsGlyphs.tsx`, `Record<SettingsSectionId, ReactNode>` of `Ico` paths: gear, the sidebar's models glyph, an up-arrow-in-circle for Updates, a box for Data, an i-in-circle for About) — TSX, so a separate file from the `.ts` model (constraint 6).
- **`components/settings/SettingsPage.tsx`**:
  ```
  props: { requestedSection?: SettingsSectionId | null; onRequestHandled?: () => void; onImportAgent: () => void; onImportPreset: () => void }
  state: section (restored once from LAST_SECTION_KEY, default "general"; written on change after the restore ran)
  effect: requestedSection → setSection(it); onRequestHandled()
  <div className="settings-page">
    <nav className="settings-nav" aria-label={t("settings.title")}>
      {SETTINGS_SECTIONS.map(s => <button type="button" className={`settings-nav__item${on ? " settings-nav__item--active" : ""}`} aria-current={on ? "page" : undefined} onClick={…}><span className="settings-nav__icon"><Ico>{glyph}</Ico></span><span className="settings-nav__label">{t(s.labelKey)}</span></button>)}
    </nav>
    <div className="settings-page__content">{the section component}</div>
  </div>
  ```
  The `data` section renders `<DataSettings onImportAgent onImportPreset />`.
- **`DataSettings`**: `onClose` removed from `Props` and from the two click handlers.

## 5. CSS

`styles/components/settings-page.css`:

```css
.settings-page { display: grid; grid-template-columns: var(--settings-nav-width) 1fr; height: 100%; min-height: 0; }
.settings-nav { display: flex; flex-direction: column; gap: 2px; padding: var(--space-6) var(--space-4); border-right: 1px solid var(--border-line); background: var(--surface-list); }
.settings-nav__item { display: flex; align-items: center; gap: var(--space-4); padding: 7px 10px; border: 0; border-radius: var(--radius-md); background: transparent; color: var(--text-secondary); font: inherit; font-size: var(--text-sm); text-align: left; cursor: pointer; }
.settings-nav__item:hover { background: var(--surface-hover); color: var(--text-primary); }
.settings-nav__item--active { background: var(--color-brand-soft); color: var(--text-primary); }
.settings-nav__icon { display: flex; width: 18px; height: 18px; }
.settings-page__content { min-width: 0; overflow: auto; padding: var(--space-7) var(--space-8) var(--space-9); background: var(--surface-detail); }
```

`--settings-nav-width: 200px` joins the shell widths in `tokens/primitives.css`. `dashboard.css` loses `.modal.settings-modal--paned`, `.settings-modal__panes`, `.settings-nav` (the old 180px one), `.settings-modal__content`, and the legacy `.sidebar` / `.sidebar-item*` rules the modal's nav was the last user of (`grep -rn 'className="sidebar\|sidebar-item' src --include='*.tsx'` → none after the modal is deleted). `.settings-section*`, `.settings-row*`, `.settings-hint`, `.settings-actions` stay: the sections use them.

## 6. Keyboard

No new shortcut. ⌘, is the app menu's (Rust) and lands on the page. Esc does nothing on the page. ⌘1–9 are untouched (`NAV_ITEMS` unchanged).

## 7. Errors and edge cases

- A stored section id that no longer exists → `isSettingsSection` rejects it and the page opens on General.
- `open-page` with `"settings"` → accepted by `isPageId` → the page.
- The import modals (`MuragentImportModal`, `PresetImportModal`) open over the page; closing them leaves the user on Settings → Import / Export, where they were.

## 8. Testing

- `settingsSections.test.ts`: the five ids in order, unique; `isSettingsSection` accepts each and rejects `"nope"`.
- `nav.test.ts`: `isPageId("settings")` is true and `NAV_ITEMS` still lists exactly the nine nav ids.
- Browser acceptance (stubbed bridge): the sidebar footer button opens the page and lights up; ⌘K → 設定 opens it; a fired `open-settings` event opens it; the wizard's Customize opens it on Models; the five sections switch and each renders its own title; reload (remount via another page and back) restores the last section; Import agent opens the import modal and closing it leaves Settings visible; no `.modal` for settings exists; ⌘1 still goes Home.

## 9. Implementation order

One PR (**PR 13**, branch `feat/hub-3d-settings-page`): (1) `PageId` + `isPageId` + Sidebar active + glyph; (2) `settingsSections` + glyphs + `SettingsPage` + CSS + token + `DataSettings` prop removal; (3) `DashboardApp` wiring, `SettingsModal` deletion, CSS cleanup.

## 10. Later

Phase 3 is complete with this PR. Candidates the redesign surfaced but did not schedule: a focus trap for `PeekPanel`, a generic channel viewer (so non-fleet multi-agent channels can peek), channel switching in `ChatTab` (which would bring the rail back), and bulk actions beyond Start / Stop once an agent delete command exists.
