# MUR Hub Settings Sidebar Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Turn the Hub's flat Settings modal into a two-pane panel with a left navigation sidebar (General · Models · Updates & CLI · Import/Export · About).

**Architecture:** `SettingsModal.tsx` becomes a shell that renders a left nav (reusing the dashboard's `.sidebar`/`.sidebar-item` classes) plus a content pane routed by an `activeSection` state. Each section is a small self-contained component under `ui/src/components/settings/`, each owning its own Tauri calls. Two new capabilities: a theme toggle (UI over the existing `data-theme` CSS tokens) and a "replay onboarding" action (new Rust command clearing the first-launch marker). Everything else relocates existing functionality. Global settings only — per-agent settings stay in `DetailPanel`.

**Tech Stack:** Tauri 2, React + TypeScript (Vite), vitest, Rust (`mur-hub-gui` crate under `mur-hub-gui/src-tauri`).

## Global Constraints

- User-facing brand string is uppercase **MUR** (never "Mur"/"MuR"). Applies to every label and copy string added.
- No hardcoded values: URLs go in named constants (`DOCS_URL = "https://app.mur.run/docs/core"`, `REPO_URL = "https://github.com/mur-run/mur"`).
- Every new i18n key MUST be added to BOTH `ui/src/i18n/en.ts` and `ui/src/i18n/zh-TW.ts` — `zh-TW.ts` is typed `Record<TranslationKey, string>`, so a missing key fails `tsc -b`.
- Reuse existing CSS classes (`.sidebar`, `.sidebar-item`, `.sidebar-item--active`, `.settings-section`, `.settings-row`, `.toolbar-btn`) — do not reinvent them.
- Single source file ≤ 800 lines.
- External links open via `import { open as openExternal } from "@tauri-apps/plugin-shell"` (the pattern already used in `Markdown.tsx`).
- **Hub compile prerequisite:** `cargo` builds of `mur-hub-gui` run `generate_context!`, which panics without `ui/dist/index.html`. Before any `cargo test` here, either run `npm run build` in `ui/`, or create a stub `ui/dist/index.html` (do not commit the stub). If a release-style build complains about `ggml -mcpu=native`, set `GGML_NATIVE=OFF`.

---

### Task 1: Rust `replay_onboarding` command

Clears the first-launch marker so onboarding runs again next launch.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/onboarding/first_launch.rs`
- Modify: `mur-hub-gui/src-tauri/src/onboarding/mod.rs:3`
- Modify: `mur-hub-gui/src-tauri/src/lib.rs:558`

**Interfaces:**
- Produces: `#[tauri::command] pub fn replay_onboarding()` (no args, no return) and testable seam `pub fn clear_marker(mur_home: &Path)`. Frontend invokes it as `invoke("replay_onboarding")`.

- [ ] **Step 1: Write the failing test**

Add to the existing `#[cfg(test)] mod tests` block in `first_launch.rs`:

```rust
    #[test]
    fn clear_marker_removes_existing_marker() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(marker_path(tmp.path()), "").unwrap();
        assert!(marker_path(tmp.path()).exists());
        clear_marker(tmp.path());
        assert!(!marker_path(tmp.path()).exists());
    }

    #[test]
    fn clear_marker_is_noop_when_absent() {
        let tmp = TempDir::new().unwrap();
        clear_marker(tmp.path()); // must not panic
        assert!(!marker_path(tmp.path()).exists());
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml clear_marker`
Expected: FAIL — `cannot find function clear_marker in this scope`.

- [ ] **Step 3: Write minimal implementation**

In `first_launch.rs`, after the `mark_first_launch_done` command (around line 44), add:

```rust
/// Remove the first-launch marker so onboarding runs again on next launch.
pub fn clear_marker(mur_home: &Path) {
    let _ = std::fs::remove_file(marker_path(mur_home));
}

/// Reset onboarding: clears the marker. Next launch behaves as first launch.
#[tauri::command]
pub fn replay_onboarding() {
    clear_marker(&mur_home_path());
}
```

In `mod.rs:3`, extend the re-export:

```rust
pub use first_launch::{check_first_launch, mark_first_launch_done, replay_onboarding};
```

In `lib.rs`, after line 558 (`onboarding::first_launch::mark_first_launch_done,`) add inside the `generate_handler![` list:

```rust
            onboarding::first_launch::replay_onboarding,
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test --manifest-path mur-hub-gui/src-tauri/Cargo.toml clear_marker`
Expected: PASS (2 tests). (If it fails to compile on `generate_context!`, ensure `ui/dist/index.html` exists per Global Constraints.)

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/src-tauri/src/onboarding/first_launch.rs mur-hub-gui/src-tauri/src/onboarding/mod.rs mur-hub-gui/src-tauri/src/lib.rs
git commit -m "feat(hub): replay_onboarding command clears first-launch marker"
```

---

### Task 2: Theme helper + startup apply

Pure theme-choice logic + DOM/localStorage application, applied before first paint.

**Files:**
- Create: `mur-hub-gui/ui/src/theme.ts`
- Create: `mur-hub-gui/ui/src/theme.test.ts`
- Modify: `mur-hub-gui/ui/src/main.tsx`

**Interfaces:**
- Produces:
  - `type ThemeChoice = "system" | "light" | "dark"`
  - `themeAttr(c: ThemeChoice): "light" | "dark" | null` (pure)
  - `getStoredTheme(): ThemeChoice`
  - `applyTheme(c: ThemeChoice): void` (persists + sets/removes `document.documentElement` `data-theme`)

- [ ] **Step 1: Write the failing test**

`ui/src/theme.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import { themeAttr } from "./theme";

describe("themeAttr", () => {
  it("maps light/dark to the data-theme attribute value", () => {
    expect(themeAttr("light")).toBe("light");
    expect(themeAttr("dark")).toBe("dark");
  });
  it("maps system to null so prefers-color-scheme applies", () => {
    expect(themeAttr("system")).toBe(null);
  });
});
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cd mur-hub-gui/ui && npm run test -- theme`
Expected: FAIL — cannot resolve `./theme`.

- [ ] **Step 3: Write minimal implementation**

`ui/src/theme.ts`:

```ts
// Theme is applied via the data-theme attribute on <html>; CSS tokens live in
// styles/tokens/semantic.css ([data-theme="dark"|"light"], else prefers-color-scheme).
export type ThemeChoice = "system" | "light" | "dark";

const STORAGE_KEY = "mur.hub.theme";

/** Attribute value for a choice, or null when the choice defers to the OS. */
export function themeAttr(c: ThemeChoice): "light" | "dark" | null {
  return c === "system" ? null : c;
}

export function getStoredTheme(): ThemeChoice {
  const v = localStorage.getItem(STORAGE_KEY);
  return v === "light" || v === "dark" || v === "system" ? v : "system";
}

export function applyTheme(c: ThemeChoice): void {
  localStorage.setItem(STORAGE_KEY, c);
  const attr = themeAttr(c);
  if (attr) document.documentElement.setAttribute("data-theme", attr);
  else document.documentElement.removeAttribute("data-theme");
}
```

In `main.tsx`, add the import and apply before `createRoot` (next to the existing pet-window tag, ~line 12):

```ts
import { applyTheme, getStoredTheme } from "./theme";
```
```ts
// Apply the saved theme before first render to avoid a flash of the wrong palette.
applyTheme(getStoredTheme());
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cd mur-hub-gui/ui && npm run test -- theme`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/theme.ts mur-hub-gui/ui/src/theme.test.ts mur-hub-gui/ui/src/main.tsx
git commit -m "feat(hub): theme helper + apply saved theme on startup"
```

---

### Task 3: i18n keys for the settings sidebar

Add all new keys to both tables. (Existing keys reused: `settings.title`, `settings.language`, `settings.defaultBrain`, `settings.noBrain`, `settings.modelsHint`, `app.importAgent`, `app.importPreset`, `app.importAgentTooltip`, `app.importPresetTooltip`, `dashboard.cliSkew`.)

**Files:**
- Modify: `mur-hub-gui/ui/src/i18n/en.ts`
- Modify: `mur-hub-gui/ui/src/i18n/zh-TW.ts`

**Interfaces:**
- Produces translation keys consumed by Tasks 4–7: `settings.nav.*`, `settings.theme*`, `settings.openLibrary`, `settings.hubVersion`, `settings.cli.*`, `settings.about.*`.

- [ ] **Step 1: Add keys to `en.ts`**

Add near the existing `settings.*` keys:

```ts
  "settings.nav.general": "General",
  "settings.nav.models": "Models",
  "settings.nav.updates": "Updates & CLI",
  "settings.nav.data": "Import / Export",
  "settings.nav.about": "About",
  "settings.theme": "Theme",
  "settings.theme.system": "System",
  "settings.theme.light": "Light",
  "settings.theme.dark": "Dark",
  "settings.openLibrary": "Open Model Library",
  "settings.hubVersion": "MUR Hub version",
  "settings.cli.install": "Install command-line tools",
  "settings.cli.installed": "Installed to {path}",
  "settings.cli.installFailed": "Install failed: {error}",
  "settings.cli.inSync": "Command-line tools are up to date.",
  "settings.about.docs": "Documentation",
  "settings.about.github": "GitHub",
  "settings.about.replay": "Replay onboarding",
  "settings.about.replayDone": "Onboarding reset — it will run next time you open MUR Hub.",
```

- [ ] **Step 2: Add the same keys to `zh-TW.ts`**

```ts
  "settings.nav.general": "一般",
  "settings.nav.models": "模型",
  "settings.nav.updates": "更新與工具",
  "settings.nav.data": "匯入 / 匯出",
  "settings.nav.about": "關於",
  "settings.theme": "主題",
  "settings.theme.system": "跟隨系統",
  "settings.theme.light": "淺色",
  "settings.theme.dark": "深色",
  "settings.openLibrary": "開啟模型庫",
  "settings.hubVersion": "MUR Hub 版本",
  "settings.cli.install": "安裝命令列工具",
  "settings.cli.installed": "已安裝至 {path}",
  "settings.cli.installFailed": "安裝失敗：{error}",
  "settings.cli.inSync": "命令列工具已是最新版本。",
  "settings.about.docs": "說明文件",
  "settings.about.github": "GitHub",
  "settings.about.replay": "重新執行新手引導",
  "settings.about.replayDone": "已重設新手引導，下次開啟 MUR Hub 時會再次顯示。",
```

- [ ] **Step 3: Verify type-check passes (key parity)**

Run: `cd mur-hub-gui/ui && npx tsc -b`
Expected: no errors. (A key in `en.ts` missing from `zh-TW.ts` would error here.)

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/i18n/en.ts mur-hub-gui/ui/src/i18n/zh-TW.ts
git commit -m "i18n(hub): settings sidebar keys (en + zh-TW)"
```

---

### Task 4: GeneralSettings component (language + theme)

**Files:**
- Create: `mur-hub-gui/ui/src/components/settings/GeneralSettings.tsx`

**Interfaces:**
- Consumes: `useT()` from `../../i18n`; `ThemeChoice`, `getStoredTheme`, `applyTheme` from `../../theme` (Task 2); keys from Task 3.
- Produces: `export function GeneralSettings(): JSX.Element` (no props).

- [ ] **Step 1: Write the component**

```tsx
import { useState } from "react";
import { useT } from "../../i18n";
import { applyTheme, getStoredTheme, type ThemeChoice } from "../../theme";

const THEMES: ThemeChoice[] = ["system", "light", "dark"];

export function GeneralSettings() {
  const { t, lang, setLang } = useT();
  const [theme, setTheme] = useState<ThemeChoice>(getStoredTheme);

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.general")}</h3>

      <div className="settings-row">
        <label className="settings-row__label" htmlFor="settings-lang">
          {t("settings.language")}
        </label>
        <select
          id="settings-lang"
          className="input"
          value={lang}
          onChange={(e) => setLang(e.target.value as typeof lang)}
        >
          <option value="en">English</option>
          <option value="zh-TW">繁體中文</option>
        </select>
      </div>

      <div className="settings-row">
        <label className="settings-row__label" htmlFor="settings-theme">
          {t("settings.theme")}
        </label>
        <select
          id="settings-theme"
          className="input"
          value={theme}
          onChange={(e) => {
            const next = e.target.value as ThemeChoice;
            setTheme(next);
            applyTheme(next);
          }}
        >
          {THEMES.map((c) => (
            <option key={c} value={c}>
              {t(`settings.theme.${c}` as Parameters<typeof t>[0])}
            </option>
          ))}
        </select>
      </div>
    </section>
  );
}
```

- [ ] **Step 2: Verify type-check passes**

Run: `cd mur-hub-gui/ui && npx tsc -b`
Expected: no errors. (Theme application logic is already unit-tested in Task 2; this component wires it.)

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/ui/src/components/settings/GeneralSettings.tsx
git commit -m "feat(hub): GeneralSettings (language + theme)"
```

---

### Task 5: ModelsSettings + DataSettings components

**Files:**
- Create: `mur-hub-gui/ui/src/components/settings/ModelsSettings.tsx`
- Create: `mur-hub-gui/ui/src/components/settings/DataSettings.tsx`

**Interfaces:**
- Consumes: `useT`; `invoke` from `@tauri-apps/api/core`; `ModelLibrary` from `../ModelLibrary` (signature: `<ModelLibrary open={boolean} onClose={() => void} />`); `nudge_status` returns `[boolean, string | null]`.
- Produces:
  - `export function ModelsSettings(): JSX.Element` (no props).
  - `export function DataSettings(props: { onImportAgent: () => void; onImportPreset: () => void; onClose: () => void }): JSX.Element`.

- [ ] **Step 1: Write `ModelsSettings.tsx`**

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";
import { ModelLibrary } from "../ModelLibrary";

export function ModelsSettings() {
  const { t } = useT();
  const [model, setModel] = useState<string | null>(null);
  const [libraryOpen, setLibraryOpen] = useState(false);

  useEffect(() => {
    invoke<[boolean, string | null]>("nudge_status")
      .then(([, m]) => setModel(m))
      .catch(() => {});
  }, []);

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.models")}</h3>
      <div className="settings-row">
        <span className="settings-row__label">{t("settings.defaultBrain")}</span>
        <span className="settings-row__value">
          {model ? `🧠 ${model}` : t("settings.noBrain")}
        </span>
      </div>
      <div className="settings-row">
        <button className="toolbar-btn" onClick={() => setLibraryOpen(true)}>
          {t("settings.openLibrary")}
        </button>
      </div>
      <p className="settings-section__hint">{t("settings.modelsHint")}</p>
      <ModelLibrary open={libraryOpen} onClose={() => setLibraryOpen(false)} />
    </section>
  );
}
```

(If `settings.section__hint` / `settings.modelsHint` styling differs in the current modal, reuse whatever element the old Models section used — check `SettingsModal.tsx` git history for the exact markup before deleting it in Task 7.)

- [ ] **Step 2: Write `DataSettings.tsx`**

```tsx
import { useT } from "../../i18n";

interface Props {
  onImportAgent: () => void;
  onImportPreset: () => void;
  onClose: () => void;
}

export function DataSettings({ onImportAgent, onImportPreset, onClose }: Props) {
  const { t } = useT();
  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.data")}</h3>
      <div className="settings-row">
        <button
          className="toolbar-btn"
          onClick={() => {
            onClose();
            onImportAgent();
          }}
          title={t("app.importAgentTooltip")}
        >
          {t("app.importAgent")}
        </button>
        <button
          className="toolbar-btn"
          onClick={() => {
            onClose();
            onImportPreset();
          }}
          title={t("app.importPresetTooltip")}
        >
          {t("app.importPreset")}
        </button>
      </div>
    </section>
  );
}
```

- [ ] **Step 3: Verify type-check passes**

Run: `cd mur-hub-gui/ui && npx tsc -b`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/settings/ModelsSettings.tsx mur-hub-gui/ui/src/components/settings/DataSettings.tsx
git commit -m "feat(hub): ModelsSettings + DataSettings sections"
```

---

### Task 6: UpdatesSettings + AboutSettings components

**Files:**
- Create: `mur-hub-gui/ui/src/components/settings/UpdatesSettings.tsx`
- Create: `mur-hub-gui/ui/src/components/settings/AboutSettings.tsx`

**Interfaces:**
- Consumes: `useT`; `invoke`; `getVersion` from `@tauri-apps/api/app`; `open as openExternal` from `@tauri-apps/plugin-shell`; `replay_onboarding` (Task 1); `cli_version_skew` → `{ cli: string; hub: string } | null`; `install_cli_tools` → `string` (install path) or throws.
- Produces: `export function UpdatesSettings(): JSX.Element`, `export function AboutSettings(): JSX.Element` (no props).

- [ ] **Step 1: Write `UpdatesSettings.tsx`**

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useT } from "../../i18n";

export function UpdatesSettings() {
  const { t } = useT();
  const [skew, setSkew] = useState<{ cli: string; hub: string } | null>(null);
  const [installMsg, setInstallMsg] = useState<string | null>(null);

  useEffect(() => {
    invoke<{ cli: string; hub: string } | null>("cli_version_skew")
      .then(setSkew)
      .catch(() => {});
  }, []);

  async function install() {
    try {
      const path = await invoke<string>("install_cli_tools");
      setInstallMsg(t("settings.cli.installed", { path }));
    } catch (e) {
      setInstallMsg(t("settings.cli.installFailed", { error: String(e) }));
    }
  }

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.updates")}</h3>
      <div className="settings-row">
        <span className="settings-row__value">
          {skew ? t("dashboard.cliSkew", skew) : t("settings.cli.inSync")}
        </span>
      </div>
      <div className="settings-row">
        <button className="toolbar-btn" onClick={install}>
          {t("settings.cli.install")}
        </button>
      </div>
      {installMsg && (
        <div className="settings-row">
          <span className="settings-row__value">{installMsg}</span>
        </div>
      )}
    </section>
  );
}
```

- [ ] **Step 2: Write `AboutSettings.tsx`**

```tsx
import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { getVersion } from "@tauri-apps/api/app";
import { open as openExternal } from "@tauri-apps/plugin-shell";
import { useT } from "../../i18n";

const DOCS_URL = "https://app.mur.run/docs/core";
const REPO_URL = "https://github.com/mur-run/mur";

export function AboutSettings() {
  const { t } = useT();
  const [version, setVersion] = useState<string>("");
  const [replayed, setReplayed] = useState(false);

  useEffect(() => {
    getVersion().then(setVersion).catch(() => {});
  }, []);

  return (
    <section className="settings-section">
      <h3 className="settings-section__title">{t("settings.nav.about")}</h3>
      <div className="settings-row">
        <span className="settings-row__label">{t("settings.hubVersion")}</span>
        <span className="settings-row__value">MUR Hub {version}</span>
      </div>
      <div className="settings-row">
        <button className="toolbar-btn" onClick={() => openExternal(DOCS_URL)}>
          {t("settings.about.docs")}
        </button>
        <button className="toolbar-btn" onClick={() => openExternal(REPO_URL)}>
          {t("settings.about.github")}
        </button>
      </div>
      <div className="settings-row">
        <button
          className="toolbar-btn"
          onClick={() => {
            invoke("replay_onboarding").catch(() => {});
            setReplayed(true);
          }}
        >
          {t("settings.about.replay")}
        </button>
      </div>
      {replayed && (
        <div className="settings-row">
          <span className="settings-row__value">{t("settings.about.replayDone")}</span>
        </div>
      )}
    </section>
  );
}
```

- [ ] **Step 3: Verify type-check passes**

Run: `cd mur-hub-gui/ui && npx tsc -b`
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/settings/UpdatesSettings.tsx mur-hub-gui/ui/src/components/settings/AboutSettings.tsx
git commit -m "feat(hub): UpdatesSettings + AboutSettings sections"
```

---

### Task 7: SettingsModal shell (two-pane sidebar + routing) + CSS

Rewrite `SettingsModal.tsx` to compose the five sections behind a left nav. Add the two-pane layout CSS.

**Files:**
- Modify: `mur-hub-gui/ui/src/components/SettingsModal.tsx` (full rewrite of the body)
- Modify: `mur-hub-gui/ui/src/styles/components/dashboard.css`

**Interfaces:**
- Consumes: the five section components from Tasks 4–6; existing `Props { isOpen, onClose, onImportAgent, onImportPreset }` (unchanged, so `DashboardApp.tsx`'s call site needs no edit).
- Produces: nothing new (same export, same Props).

- [ ] **Step 1: Rewrite `SettingsModal.tsx`**

```tsx
import { useState } from "react";
import { useT } from "../i18n";
import { GeneralSettings } from "./settings/GeneralSettings";
import { ModelsSettings } from "./settings/ModelsSettings";
import { UpdatesSettings } from "./settings/UpdatesSettings";
import { DataSettings } from "./settings/DataSettings";
import { AboutSettings } from "./settings/AboutSettings";

interface Props {
  isOpen: boolean;
  onClose: () => void;
  onImportAgent: () => void;
  onImportPreset: () => void;
}

type SectionId = "general" | "models" | "updates" | "data" | "about";

const NAV: { id: SectionId; labelKey: string; icon: string }[] = [
  { id: "general", labelKey: "settings.nav.general", icon: "⚙️" },
  { id: "models", labelKey: "settings.nav.models", icon: "🧠" },
  { id: "updates", labelKey: "settings.nav.updates", icon: "⬆️" },
  { id: "data", labelKey: "settings.nav.data", icon: "📦" },
  { id: "about", labelKey: "settings.nav.about", icon: "ℹ️" },
];

export function SettingsModal({ isOpen, onClose, onImportAgent, onImportPreset }: Props) {
  const { t } = useT();
  const [active, setActive] = useState<SectionId>("general");

  if (!isOpen) return null;

  return (
    <div className="modal__overlay" onClick={onClose}>
      <div
        className="modal settings-modal settings-modal--paned"
        onClick={(e) => e.stopPropagation()}
      >
        <div className="modal__header">
          <h2 className="modal__title">{t("settings.title")}</h2>
          <button className="modal__close" onClick={onClose}>
            ×
          </button>
        </div>

        <div className="settings-modal__panes">
          <nav className="sidebar settings-nav">
            {NAV.map((n) => (
              <button
                key={n.id}
                className={`sidebar-item${active === n.id ? " sidebar-item--active" : ""}`}
                onClick={() => setActive(n.id)}
              >
                <span className="sidebar-item__icon">{n.icon}</span>
                <span>{t(n.labelKey as Parameters<typeof t>[0])}</span>
              </button>
            ))}
          </nav>

          <div className="modal__body settings-modal__content">
            {active === "general" && <GeneralSettings />}
            {active === "models" && <ModelsSettings />}
            {active === "updates" && <UpdatesSettings />}
            {active === "data" && (
              <DataSettings
                onImportAgent={onImportAgent}
                onImportPreset={onImportPreset}
                onClose={onClose}
              />
            )}
            {active === "about" && <AboutSettings />}
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Add the two-pane CSS**

Append to `ui/src/styles/components/dashboard.css` (after the existing `.settings-*` block, ~line 191):

```css
/* ── Settings two-pane layout ── */
.settings-modal--paned { width: 720px; max-width: 92vw; }
.settings-modal__panes {
  display: flex;
  min-height: 360px;
  max-height: 70vh;
}
.settings-nav {
  width: 180px;
  flex: none;
}
.settings-modal__content {
  flex: 1;
  overflow-y: auto;
  padding: 16px 20px;
}
```

- [ ] **Step 3: Build the UI**

Run: `cd mur-hub-gui/ui && npm run build`
Expected: build succeeds (tsc + vite), no type errors.

- [ ] **Step 4: Manual verify**

Build/run the Hub (`./build.sh` or the local Hub `.app` recipe). Open Settings:
- Sidebar shows five items top-to-bottom: General, Models, Updates & CLI, Import / Export, About.
- Clicking each swaps the right pane; active item is highlighted (`--color-brand`).
- General: switching Theme to Dark/Light changes the palette immediately and persists across reopen; System follows the OS.
- Models: shows the default brain + "Open Model Library" opens the library.
- Updates & CLI: shows sync status; "Install command-line tools" reports a result.
- Import / Export: both import buttons open their flows (and close settings first).
- About: shows the Hub version; Documentation/GitHub open in browser; Replay onboarding shows the reset confirmation.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/components/SettingsModal.tsx mur-hub-gui/ui/src/styles/components/dashboard.css
git commit -m "feat(hub): two-pane settings panel with left nav sidebar"
```

---

## Self-Review

**Spec coverage:**
- Two-pane layout + left nav reusing `.sidebar` → Task 7. ✓
- `activeSection` routing default `general` → Task 7 (`useState<SectionId>("general")`). ✓
- `settings/` folder, one file per section → Tasks 4–6. ✓
- Five sections (General/Models/Updates/Import-Export/About) → Tasks 4–7. ✓
- General: language (move) + theme (new) → Tasks 2, 4. ✓
- Models: badge + Open Model Library → Task 5. ✓
- Updates & CLI: install + version-skew → Task 6. ✓
- Import/Export: import agent + preset (export stays per-agent) → Task 5. ✓
- About: version + links + replay onboarding (new Rust command) → Tasks 1, 6. ✓
- i18n en + zh-TW, brand "MUR" → Task 3, used throughout. ✓
- Tests: Rust replay marker fs test (Task 1), vitest pure `themeAttr` test (Task 2). ✓
- Theme applied before first paint → Task 2 (main.tsx). ✓

**Placeholder scan:** No TBD/TODO; every code step has full code. ✓

**Type consistency:** `SectionId` union matches `NAV` ids and the render switch; `ThemeChoice` used identically in `theme.ts`, `GeneralSettings`; `DataSettings` Props (`onImportAgent`/`onImportPreset`/`onClose`) match `SettingsModal`'s call site; `cli_version_skew` shape `{cli,hub}` consistent across Task 6 and the reused `dashboard.cliSkew` key vars. ✓

**Notes / deliberate simplifications (ponytail):**
- Replay onboarding only clears the marker (effective next launch) + shows a confirmation — no live re-trigger. Add live re-trigger (callback up to `DashboardApp` to re-run `check_first_launch`) only if users want it immediate.
- Section components are verified via `tsc -b` (type-check) rather than DOM render tests — avoids pulling jsdom/testing-library for trivial markup. Add render tests if these sections grow real logic.
- The DashboardApp top-of-window `cliSkew` banner is left as-is; Updates & CLI is an additional home for the same info plus the install action, not a replacement.
