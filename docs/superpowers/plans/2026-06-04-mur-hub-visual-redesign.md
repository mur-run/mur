# MuR Hub Visual Redesign + Lightweight i18n — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Restyle `mur-hub-gui/ui` to MuR's brand (blue starling + coral CTA + airy light-blue, dark-navy headings) via a two-tier vanilla-CSS token system, and externalize every user-visible string behind a lightweight custom i18n layer (en + zh-TW) in the same pass.

**Architecture:** Split the single `src/styles.css` (1184 lines) into a token layer (`primitives.css` → `semantic.css`) + `base.css` + `motion.css` + `components/*.css`; components consume only semantic tokens. A custom `LanguageContext` + `t()` helper + typed `en`/`zh-TW` string tables replace hardcoded strings. Zero new styling deps; `vitest` added only to unit-test the i18n helper.

**Tech Stack:** Tauri 2, React 18, Vite 5, TypeScript 5, plain CSS custom properties.

**Spec:** `docs/superpowers/specs/2026-06-04-mur-hub-visual-redesign-design.md`
**Work branch:** `design/mur-hub-visual-redesign`

---

## Conventions for every task

- **Build gate** (the primary verification for visual tasks): `npm run build` in `mur-hub-gui/ui` runs `tsc -b && vite build`. It must succeed (typecheck + bundle).
- **Lint gate:** `npm run lint` (eslint, includes react-hooks rules) must pass clean.
- **Dev preview** for visual checks: `npm run dev` then open the printed localhost URL in a browser. Tauri-only APIs (`invoke`) will no-op/throw in a plain browser — that is expected; check layout/visuals, not live data.
- Run all `npm` commands from `mur-hub-gui/ui`.
- Keep each new file < 800 lines (CLAUDE.md rule 4).
- Commit after every task with the message shown.

---

## File Structure (target)

```
ui/src/
├── main.tsx                      # MODIFY: import "./styles/index.css" + wrap App in <LanguageProvider>
├── styles/
│   ├── index.css                 # NEW barrel: @import all below in order
│   ├── tokens/primitives.css     # NEW raw scales
│   ├── tokens/semantic.css       # NEW semantic + dark-mode
│   ├── base.css                  # NEW reset/body/font/focus
│   ├── motion.css                # NEW keyframes + motion utilities
│   └── components/
│       ├── primitives.css        # NEW button/badge/pill/field/toast/avatar/empty-state
│       ├── dashboard.css         # MOVED+restyled
│       ├── detail-panel.css      # MOVED+restyled
│       ├── popover.css           # MOVED+restyled
│       ├── pet.css               # MOVED+restyled
│       ├── companion.css         # MOVED+restyled
│       ├── wizard.css            # MOVED+restyled
│       └── modal.css             # MOVED+restyled
├── components/Mascot.tsx         # NEW CSS starling component (hero/empty/pet)
├── i18n/
│   ├── types.ts                  # NEW TranslationKey union + Lang type
│   ├── en.ts                     # NEW English table
│   ├── zh-TW.ts                  # NEW zh-TW table
│   ├── index.ts                  # NEW LanguageContext, LanguageProvider, useT
│   └── t.test.ts                 # NEW vitest unit tests
└── styles.css                    # DELETE after split
```

---

## Task 0: Verify branch & baseline build

**Files:** none

- [ ] **Step 1: Confirm branch and clean baseline build**

Run:
```bash
cd /Volumes/Firecuda4tb/Projects/mur/mur-hub-gui/ui
git rev-parse --abbrev-ref HEAD   # expect: design/mur-hub-visual-redesign
npm install
npm run build
```
Expected: branch matches; build succeeds. If build already fails on baseline, stop and report — do not start on a red baseline.

- [ ] **Step 2: Commit nothing** (baseline only). Proceed.

---

## Task 1: Token foundation (primitives + semantic)

**Files:**
- Create: `ui/src/styles/tokens/primitives.css`
- Create: `ui/src/styles/tokens/semantic.css`
- Create: `ui/src/styles/index.css`
- Modify: `ui/src/main.tsx` (import path only)

- [ ] **Step 1: Create `primitives.css`**

```css
/* Raw scales. Never referenced directly by components — only by semantic.css. */
:root {
  /* brand blue */
  --blue-700:#2F6FB0; --blue-500:#4A90D9; --blue-soft:#EAF3FC;
  /* accent coral */
  --coral-600:#F0563D; --coral-500:#FB6B53; --coral-soft:#FFF1EE;
  /* category */
  --cat-research:#4A90D9; --cat-automation:#10B981; --cat-monitor:#F59E0B;
  --cat-notify:#EF4444; --cat-commerce:#8B5CF6; --cat-custom:#64748B;
  /* status */
  --green-500:#22C55E; --green-600:#16A34A; --amber-500:#F59E0B;
  --red-500:#EF4444; --red-600:#DC2626; --slate-400:#94A3B8;
  /* neutral light */
  --n-bg:#F6FAFE; --n-secondary:#EAF3FC; --n-card:#FFFFFF; --n-hover:#EAF1F9;
  --n-line:#EAF0F7; --n-text:#16243B; --n-text2:#64758A; --n-text3:#9AA8B8;
  /* neutral dark */
  --d-bg:#0F1117; --d-secondary:#161922; --d-card:#1A1E29; --d-hover:#232838;
  --d-line:rgba(255,255,255,.08); --d-text:#F1F5F9; --d-text2:#94A3B8; --d-text3:#6B7688;
  /* space (4px base) */
  --space-1:2px; --space-2:4px; --space-3:6px; --space-4:8px; --space-5:12px;
  --space-6:16px; --space-7:20px; --space-8:24px; --space-9:32px; --space-10:40px;
  /* radius */
  --radius-sm:6px; --radius-md:10px; --radius-lg:14px; --radius-xl:18px; --radius-2xl:20px; --radius-full:9999px;
  /* type */
  --font-sans:-apple-system,BlinkMacSystemFont,"Segoe UI",Roboto,"Helvetica Neue",Arial,"PingFang TC","Microsoft JhengHei",sans-serif;
  --text-xs:11px; --text-sm:12.5px; --text-base:14px; --text-md:15px;
  --text-lg:17px; --text-xl:19px; --text-2xl:22px;
  --fw-regular:400; --fw-semi:600; --fw-bold:700; --fw-heavy:800;
  /* shadow */
  --shadow-sm:0 1px 2px rgba(16,40,80,.06);
  --shadow-card:0 1px 3px rgba(16,40,80,.08),0 1px 2px rgba(16,40,80,.04);
  --shadow-pop:0 16px 40px rgba(16,40,80,.16);
  --shadow-focus:0 0 0 3px rgba(74,144,217,.18);
  /* motion */
  --dur-fast:130ms; --dur-base:180ms; --dur-slow:280ms;
  --ease-out:cubic-bezier(.2,.85,.25,1);
  --ease-spring:cubic-bezier(.34,1.56,.64,1);
}
```

- [ ] **Step 2: Create `semantic.css`** (components consume ONLY these)

```css
:root {
  --color-brand:var(--blue-500); --color-brand-strong:var(--blue-700);
  --color-accent:var(--coral-500); --color-accent-strong:var(--coral-600);
  --color-accent-soft:var(--coral-soft); --color-brand-soft:var(--blue-soft);
  --surface-bg:var(--n-bg); --surface-secondary:var(--n-secondary);
  --surface-card:var(--n-card); --surface-hover:var(--n-hover);
  --border-line:var(--n-line);
  --text-primary:var(--n-text); --text-secondary:var(--n-text2); --text-tertiary:var(--n-text3);
  --status-running:var(--green-600); --status-idle:var(--slate-400);
  --status-restarting:var(--amber-500); --status-failed:var(--red-600);
  --shadow-1:var(--shadow-card); --shadow-pop:var(--shadow-pop); --shadow-focus:var(--shadow-focus);
}
@media (prefers-color-scheme: dark) {
  :root {
    --surface-bg:var(--d-bg); --surface-secondary:var(--d-secondary);
    --surface-card:var(--d-card); --surface-hover:var(--d-hover);
    --border-line:var(--d-line);
    --text-primary:var(--d-text); --text-secondary:var(--d-text2); --text-tertiary:var(--d-text3);
  }
}
/* Manual override hook for future Settings toggle; identical to the media block. */
:root[data-theme="dark"] {
  --surface-bg:var(--d-bg); --surface-secondary:var(--d-secondary);
  --surface-card:var(--d-card); --surface-hover:var(--d-hover);
  --border-line:var(--d-line);
  --text-primary:var(--d-text); --text-secondary:var(--d-text2); --text-tertiary:var(--d-text3);
}
:root[data-theme="light"] {
  --surface-bg:var(--n-bg); --surface-secondary:var(--n-secondary);
  --surface-card:var(--n-card); --surface-hover:var(--n-hover);
  --border-line:var(--n-line);
  --text-primary:var(--n-text); --text-secondary:var(--n-text2); --text-tertiary:var(--n-text3);
}
```

- [ ] **Step 3: Create `index.css` barrel** (order matters)

```css
@import "./tokens/primitives.css";
@import "./tokens/semantic.css";
@import "./base.css";
@import "./motion.css";
@import "./components/primitives.css";
@import "./components/dashboard.css";
@import "./components/detail-panel.css";
@import "./components/popover.css";
@import "./components/pet.css";
@import "./components/companion.css";
@import "./components/wizard.css";
@import "./components/modal.css";
```

- [ ] **Step 4: Point the app at the barrel.** In `ui/src/main.tsx` change `import "./styles.css";` to `import "./styles/index.css";`. (Old `styles.css` still exists and is no longer imported; the empty `@import`-ed component files are created in Task 3. Until then the build will fail to resolve imports — so do Steps 1–3 of Task 3 before building. Defer the build gate to Task 3.)

- [ ] **Step 5: Commit**

```bash
git add ui/src/styles/tokens ui/src/styles/index.css ui/src/main.tsx
git commit -m "feat(hub-ui): add two-tier design token layer (primitives + semantic)"
```

---

## Task 2: base.css + motion.css

**Files:**
- Create: `ui/src/styles/base.css`
- Create: `ui/src/styles/motion.css`

- [ ] **Step 1: Create `base.css`**

```css
* , *::before, *::after { box-sizing: border-box; }
html, body, #root { height: 100%; }
body {
  margin: 0;
  font-family: var(--font-sans);
  font-size: var(--text-base);
  color: var(--text-primary);
  background: var(--surface-bg);
  -webkit-font-smoothing: antialiased;
  text-rendering: optimizeLegibility;
}
button { font-family: inherit; }
:focus-visible { outline: none; box-shadow: var(--shadow-focus); border-radius: var(--radius-md); }
::selection { background: var(--color-brand-soft); }
```

- [ ] **Step 2: Create `motion.css`** (reusable keyframes; the mascot easter-egg lives in its component CSS)

```css
@keyframes pulse-dot { 0%,100%{opacity:1} 50%{opacity:.35} }
@keyframes breathe-ring {
  0%,100%{ box-shadow:0 0 0 0 rgba(34,197,94,.5) }
  50%{ box-shadow:0 0 0 5px rgba(34,197,94,0) }
}
@keyframes float-y { 0%,100%{transform:translateY(0)} 50%{transform:translateY(-7px)} }
@keyframes toast-in { from{opacity:0; transform:translateY(8px)} to{opacity:1; transform:translateY(0)} }
.u-lift { transition: transform var(--dur-base) var(--ease-out), box-shadow var(--dur-base) var(--ease-out); }
.u-lift:hover { transform: translateY(-3px); box-shadow: var(--shadow-pop); }
@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { animation-duration:.001ms !important; animation-iteration-count:1 !important; transition-duration:.001ms !important; }
}
```

- [ ] **Step 3: Commit**

```bash
git add ui/src/styles/base.css ui/src/styles/motion.css
git commit -m "feat(hub-ui): add base reset and motion utilities"
```

---

## Task 3: Split styles.css into component files (PURE MOVEMENT, no value changes)

Goal: relocate the existing rules from `styles.css` into the seven `components/*.css` files so the app builds again with identical appearance. **Do not change any values yet** — visual restyle happens in later tasks. This isolates "did I break a selector" from "do I like the new look".

**Files:**
- Create: `ui/src/styles/components/{primitives,dashboard,detail-panel,popover,pet,companion,wizard,modal}.css`
- Delete: `ui/src/styles.css`

- [ ] **Step 1: Read the current stylesheet** to see section boundaries.

Run: `sed -n '1,1184p' ui/src/styles.css | less` (or open it). It is organized by feature (popover, agent-row, dashboard, detail panel, pet, companion, wizard, modal, toast, etc.).

- [ ] **Step 2: Create the eight component files and move rules by feature.**

Mapping (move each block verbatim):
- `components/primitives.css` ← `.toast`, generic `button`/badge/status-dot/avatar helpers, `:root` color vars that are NOT tokens (delete duplicates already covered by Task 1 tokens — e.g. old `--brand`, `--status-*`, `--bg*`, `--text*` definitions; components keep referencing them in Task 4 via the new semantic names, but for pure-movement keep old names working by leaving the old `:root` block here temporarily).
- `components/dashboard.css` ← `.dashboard*`, grid, `.grid-card*`, header, search, `.grid-view`/`.list-view` toggle rules, `No agents yet` / `Applications` styles.
- `components/popover.css` ← `.popover*`, `.agent-group`, `.group-header`, `.agent-row*`, `.agent-avatar`, `.agent-info`, `.agent-name`, `.agent-category`, `.popover-footer`.
- `components/detail-panel.css` ← `.detail-panel*`.
- `components/pet.css` ← `.pet-*`.
- `components/companion.css` ← `.companion*`, inbox/message styles.
- `components/wizard.css` ← `.wizard*`, step styles.
- `components/modal.css` ← preset/muragent modal styles, dropzone, overlay.

Keep the **old `:root` variable block** (the original `--brand`, `--bg`, etc.) at the top of `primitives.css` for now so moved rules still resolve. Task 4 removes it.

- [ ] **Step 3: Delete `ui/src/styles.css`.**

Run: `git rm ui/src/styles.css`

- [ ] **Step 4: Build gate (first real build of the new structure).**

Run: `npm run build`
Expected: PASS. Then `npm run dev` and eyeball the dashboard/popover — appearance should be **unchanged** from baseline (we only moved rules).

- [ ] **Step 5: Commit**

```bash
git add ui/src/styles
git commit -m "refactor(hub-ui): split styles.css into token-layered component files (pure movement)"
```

---

## Task 4: Restyle shared primitives + adopt semantic tokens

Now apply the new look to the shared atoms and switch them onto semantic tokens. Remove the legacy `:root` block from `primitives.css` and replace remaining references.

**Files:**
- Modify: `ui/src/styles/components/primitives.css`

- [ ] **Step 1: Replace `primitives.css` content** with token-driven atoms:

```css
/* Buttons */
.btn { font-size:var(--text-base); font-weight:var(--fw-bold); padding:9px 16px;
  border-radius:var(--radius-md); border:1px solid transparent; cursor:pointer;
  transition:transform var(--dur-fast) var(--ease-out), background var(--dur-fast), box-shadow var(--dur-fast); }
.btn--primary { background:var(--color-accent); color:#fff; box-shadow:0 3px 10px rgba(251,107,83,.32); }
.btn--primary:hover { background:var(--color-accent-strong); transform:translateY(-1px); }
.btn--blue { background:var(--color-brand); color:#fff; }
.btn--blue:hover { background:var(--color-brand-strong); }
.btn--secondary { background:var(--surface-card); color:var(--text-primary); border-color:var(--border-line); }
.btn--secondary:hover { background:var(--color-brand-soft); color:var(--color-brand-strong); }
.btn--danger { background:var(--color-accent-soft); color:var(--color-accent-strong); border-color:#FBD7CF; }
.btn--link { background:transparent; color:var(--color-brand); padding:9px 6px; }
.btn--sm { font-size:var(--text-sm); padding:6px 12px; }
.btn:disabled,.btn--disabled { background:#EEF1F5; color:#A8B3C0; box-shadow:none; cursor:not-allowed; }

/* Category badge + status pill */
.badge { font-size:var(--text-xs); font-weight:var(--fw-semi); padding:3px 9px; border-radius:var(--radius-sm); }
.pill { display:inline-flex; align-items:center; gap:6px; font-size:var(--text-xs);
  font-weight:var(--fw-bold); padding:4px 9px; border-radius:var(--radius-full); }
.pill__dot { width:6px; height:6px; border-radius:50%; background:currentColor; }
.pill--run { background:#E9F9EF; color:var(--status-running); }
.pill--run .pill__dot { animation:pulse-dot 2.4s ease-in-out infinite; }
.pill--idle { background:#F1F5F9; color:var(--text-secondary); }
.pill--fail { background:#FDECEA; color:var(--status-failed); }

/* Field */
.field { display:flex; align-items:center; gap:8px; background:var(--surface-card);
  border:1px solid var(--border-line); border-radius:var(--radius-md); padding:9px 12px;
  font-size:var(--text-base); color:var(--text-primary); box-shadow:var(--shadow-sm); }
.field:focus-within { border-color:var(--color-brand); box-shadow:var(--shadow-focus); }
.field input { border:none; outline:none; background:transparent; font:inherit; color:inherit; width:100%; }
.field input::placeholder { color:var(--text-tertiary); }

/* Toast */
.toast { display:flex; align-items:center; gap:10px; background:var(--surface-card);
  border:1px solid var(--border-line); border-left:4px solid var(--color-brand);
  border-radius:11px; padding:11px 15px; font-size:var(--text-base); color:var(--text-primary);
  box-shadow:var(--shadow-pop); animation:toast-in var(--dur-base) var(--ease-out); }
.toast--ok { border-left-color:var(--status-running); }
.toast--err { border-left-color:var(--color-accent); }

/* Avatar */
.avatar { display:grid; place-items:center; border-radius:var(--radius-lg);
  box-shadow:inset 0 0 0 1px rgba(16,40,80,.04); flex:none; }

/* Category color map (consumed via class on badge/avatar/accent) */
.cat-research { --cat:var(--cat-research); } .cat-automation { --cat:var(--cat-automation); }
.cat-monitor { --cat:var(--cat-monitor); } .cat-notify { --cat:var(--cat-notify); }
.cat-commerce { --cat:var(--cat-commerce); } .cat-custom { --cat:var(--cat-custom); }

/* Empty state */
.empty-state { text-align:center; padding:40px 30px 34px;
  background:linear-gradient(180deg,var(--color-brand-soft),var(--surface-bg));
  border:1px solid var(--border-line); border-radius:var(--radius-xl); }
.empty-state h3 { margin:18px 0 6px; font-size:var(--text-lg); font-weight:var(--fw-heavy); color:var(--text-primary); }
.empty-state p { margin:0 auto 20px; font-size:var(--text-base); line-height:1.6; color:var(--text-secondary); max-width:260px; }
```

- [ ] **Step 2: Remove the legacy `:root` block** that was parked in `primitives.css` during Task 3. Grep for any rule still using old names and repoint to semantic tokens:

Run: `grep -rnE "var\(--brand\)|var\(--bg\)|var\(--bg-secondary\)|var\(--text\)|var\(--text-inverse\)|var\(--status-stale\)" ui/src/styles`
For each hit, replace: `--brand`→`--color-brand`, `--bg`→`--surface-bg`, `--bg-secondary`→`--surface-secondary`, `--bg-hover`→`--surface-hover`, `--text`→`--text-primary`, `--text-secondary` stays (now semantic), `--text-inverse`→`#fff`, `--status-stale`→`--status-failed`, `--border`→`--border-line`, `--shadow`→`--shadow-1`.

- [ ] **Step 3: Build + lint gate**

Run: `npm run build && npm run lint`
Expected: PASS, no unresolved `var(--…)` (CSS won't error, so also `npm run dev` and confirm buttons/pills/toasts look like the v4/components mockup).

- [ ] **Step 4: Commit**

```bash
git add ui/src/styles/components/primitives.css ui/src/styles
git commit -m "feat(hub-ui): restyle shared primitives onto semantic tokens"
```

---

## Task 5: Mascot component + signature easter egg

**Files:**
- Create: `ui/src/components/Mascot.tsx`
- Modify: `ui/src/styles/components/primitives.css` (append `.bird` rules)

- [ ] **Step 1: Create `Mascot.tsx`** (CSS starling; `size` prop scales it; reused in hero/empty/pet)

```tsx
interface MascotProps { size?: number; floating?: boolean; className?: string; }

/** MuR starling. Hover raises brows + shifts pupils toward the viewer (house easter egg). */
export function Mascot({ size = 66, floating = false, className = "" }: MascotProps) {
  return (
    <div
      className={`bird ${floating ? "bird--float" : ""} ${className}`}
      style={{ width: size, height: size * 0.94 }}
      aria-hidden="true"
    >
      <div className="bird__feet" />
      <div className="bird__body" />
      <div className="bird__wing bird__wing--l" />
      <div className="bird__wing bird__wing--r" />
      <div className="bird__brow bird__brow--l" />
      <div className="bird__brow bird__brow--r" />
      <div className="bird__eye bird__eye--l" />
      <div className="bird__eye bird__eye--r" />
      <div className="bird__beak" />
    </div>
  );
}
```

- [ ] **Step 2: Append mascot CSS** to `primitives.css` (proportions are relative to a 66×62 box; scale via the wrapper `width/height`). Use the validated mockup values:

```css
.bird { position:relative; flex:none; cursor:pointer; }
.bird--float { animation:float-y 3.6s ease-in-out infinite; }
.bird__feet { position:absolute; bottom:0; left:0; right:0; height:18%; }
.bird__feet::before,.bird__feet::after { content:""; position:absolute; bottom:0; width:3%; min-width:2px; height:100%; background:#16243B; }
.bird__feet::before { left:36%; transform:rotate(8deg); }
.bird__feet::after  { right:36%; transform:rotate(-8deg); }
.bird__body { position:absolute; left:6%; right:6%; top:16%; bottom:13%;
  background:linear-gradient(160deg,#5BA0E0,#4187CF);
  border-radius:50% 50% 47% 47%/58% 58% 42% 42%; box-shadow:inset -5px -7px 14px rgba(0,0,0,.08); }
.bird__wing { position:absolute; top:48%; width:20%; height:29%; background:#3D7DC4; border-radius:50%; }
.bird__wing--l { left:4%; transform:rotate(18deg); }
.bird__wing--r { right:4%; transform:rotate(-18deg); }
.bird__brow { position:absolute; top:7%; width:40%; height:24%; background:#13203A; z-index:3;
  transition:transform var(--dur-base) var(--ease-out); }
.bird__brow--l { left:12%; border-radius:70% 30% 30% 0/100% 100% 30% 0; transform:rotate(-10deg); }
.bird__brow--r { right:12%; border-radius:30% 70% 0 30%/100% 100% 0 30%; transform:rotate(10deg); }
.bird__eye { position:absolute; top:32%; width:24%; height:27%; background:#FBF6E9;
  border-radius:50%; z-index:2; box-shadow:inset 0 -3px 4px rgba(0,0,0,.06); }
.bird__eye--l { left:24%; } .bird__eye--r { right:24%; }
.bird__eye::after { content:""; position:absolute; left:30%; top:30%; width:36%; height:38%;
  background:#13203A; border-radius:50%; transition:transform var(--dur-base) var(--ease-out); }
.bird__beak { position:absolute; top:50%; left:50%; transform:translateX(-50%); width:12%; height:11%;
  background:var(--color-brand-strong); border-radius:40% 40% 60% 60%; z-index:2; }
/* Easter egg */
.bird:hover .bird__brow--l { transform:rotate(-18deg) translateY(-2px); }
.bird:hover .bird__brow--r { transform:rotate(18deg) translateY(-2px); }
.bird:hover .bird__eye::after { transform:translate(18%,18%) scale(1.05); }
```

- [ ] **Step 3: Build gate**

Run: `npm run build`
Expected: PASS. (Visual verification of the mascot happens in Task 8 where it's mounted.)

- [ ] **Step 4: Commit**

```bash
git add ui/src/components/Mascot.tsx ui/src/styles/components/primitives.css
git commit -m "feat(hub-ui): add CSS starling Mascot with hover easter egg"
```

---

## Task 6: i18n core (TDD)

**Files:**
- Create: `ui/src/i18n/types.ts`, `en.ts`, `zh-TW.ts`, `index.ts`, `t.test.ts`
- Modify: `ui/package.json` (add vitest + test script)

- [ ] **Step 1: Add vitest** (only new runtime/dev dep; standard for Vite)

Run:
```bash
npm install -D vitest
npm pkg set scripts.test="vitest run"
```

- [ ] **Step 2: Define the string-table shape and seed keys** in `en.ts`. Start with the global/dashboard keys (more added per surface in later tasks):

```ts
export const en = {
  "app.newAgent": "New Agent",
  "app.importAgent": "Import Agent",
  "dashboard.heading": "Agents",
  "dashboard.search": "Search… (⌘K)",
  "dashboard.empty.title": "No flock yet",
  "dashboard.empty.body": "Create your first agent and let it start learning in your workflow.",
  "dashboard.empty.cta": "Create your first agent",
  "dashboard.greeting": "Good evening, {name} 👋",
  "dashboard.flockStatus": "Your flock is learning — {running} flying, {idle} resting",
  "view.grid": "Grid view",
  "view.list": "List view",
  "status.running": "running",
  "status.idle": "idle",
  "status.error": "error",
  "settings.language": "Language",
} as const;
```

- [ ] **Step 3: Define types** in `types.ts` (keys come from `en`, so `en` is the source of truth)

```ts
import { en } from "./en";
export type TranslationKey = keyof typeof en;
export type Lang = "en" | "zh-TW";
export type Table = Record<TranslationKey, string>;
```

- [ ] **Step 4: Create `zh-TW.ts`** typed as `Table` (compiler enforces completeness)

```ts
import type { Table } from "./types";
export const zhTW: Table = {
  "app.newAgent": "新增 Agent",
  "app.importAgent": "匯入 Agent",
  "dashboard.heading": "Agents",
  "dashboard.search": "搜尋… (⌘K)",
  "dashboard.empty.title": "鳥群還沒成形",
  "dashboard.empty.body": "建立你的第一隻 agent，讓牠開始在你的工作流中學習。",
  "dashboard.empty.cta": "＋ 建立第一隻 Agent",
  "dashboard.greeting": "晚安，{name} 👋",
  "dashboard.flockStatus": "你的鳥群正在學習中 — {running} 隻在飛，{idle} 隻在休息",
  "view.grid": "卡片檢視",
  "view.list": "列表檢視",
  "status.running": "執行中",
  "status.idle": "閒置",
  "status.error": "錯誤",
  "settings.language": "語言",
};
```

- [ ] **Step 5: Write the FAILING test** `t.test.ts`

```ts
import { describe, it, expect } from "vitest";
import { translate } from "./index";

describe("translate", () => {
  it("returns the English string for a key", () => {
    expect(translate("en", "app.newAgent")).toBe("New Agent");
  });
  it("returns the zh-TW string for a key", () => {
    expect(translate("zh-TW", "app.newAgent")).toBe("新增 Agent");
  });
  it("interpolates {vars}", () => {
    expect(translate("en", "dashboard.greeting", { name: "David" })).toBe("Good evening, David 👋");
  });
  it("falls back to en when a lang table is missing the key at runtime", () => {
    // @ts-expect-error force an unknown lang
    expect(translate("ja", "app.newAgent")).toBe("New Agent");
  });
});
```

- [ ] **Step 6: Run it, verify it fails**

Run: `npm test`
Expected: FAIL ("translate is not a function" / cannot find `./index`).

- [ ] **Step 7: Implement `index.ts`**

```tsx
import { createContext, useContext, useState, useEffect, type ReactNode } from "react";
import { en } from "./en";
import { zhTW } from "./zh-TW";
import type { Lang, TranslationKey, Table } from "./types";

const TABLES: Record<string, Table> = { en, "zh-TW": zhTW };
const STORAGE_KEY = "mur.hub.lang";

export function translate(lang: string, key: TranslationKey, vars?: Record<string, string | number>): string {
  const table = TABLES[lang] ?? en;
  let s = (table[key] ?? en[key]) as string;
  if (vars) for (const [k, v] of Object.entries(vars)) s = s.replace(new RegExp(`\\{${k}\\}`, "g"), String(v));
  return s;
}

function detectDefault(): Lang {
  const stored = localStorage.getItem(STORAGE_KEY);
  if (stored === "en" || stored === "zh-TW") return stored;
  return navigator.language.toLowerCase().startsWith("zh") ? "zh-TW" : "en";
}

interface Ctx { lang: Lang; setLang: (l: Lang) => void; t: (k: TranslationKey, vars?: Record<string, string | number>) => string; }
const LanguageContext = createContext<Ctx | null>(null);

export function LanguageProvider({ children }: { children: ReactNode }) {
  const [lang, setLangState] = useState<Lang>(detectDefault);
  useEffect(() => { localStorage.setItem(STORAGE_KEY, lang); }, [lang]);
  const setLang = (l: Lang) => setLangState(l);
  const t = (k: TranslationKey, vars?: Record<string, string | number>) => translate(lang, k, vars);
  return <LanguageContext.Provider value={{ lang, setLang, t }}>{children}</LanguageContext.Provider>;
}

export function useT() {
  const ctx = useContext(LanguageContext);
  if (!ctx) throw new Error("useT must be used within <LanguageProvider>");
  return ctx;
}
```

- [ ] **Step 8: Run tests, verify PASS**

Run: `npm test`
Expected: 4 passed.

- [ ] **Step 9: Commit**

```bash
git add ui/src/i18n ui/package.json ui/package-lock.json
git commit -m "feat(hub-ui): add lightweight i18n core (en + zh-TW) with tests"
```

---

## Task 7: Mount LanguageProvider + Settings language switcher

**Files:**
- Modify: `ui/src/main.tsx`
- Modify: `ui/src/App.tsx` (or wherever Settings lives; if no Settings surface exists, add a minimal switcher to the dashboard top bar)

- [ ] **Step 1: Wrap the app** in `main.tsx`:

```tsx
import { LanguageProvider } from "./i18n";
// ...
ReactDOM.createRoot(document.getElementById("root")!).render(
  <React.StrictMode>
    <LanguageProvider>
      <App />
    </LanguageProvider>
  </React.StrictMode>,
);
```

- [ ] **Step 2: Inspect App routing** to find where Settings/top-bar is.

Run: `cat ui/src/App.tsx`
If a Settings view exists, add the switcher there; otherwise add it to the dashboard top bar (Task 8) and skip a separate Settings task.

- [ ] **Step 3: Add a language `<select>`** wherever Settings renders:

```tsx
import { useT } from "../i18n";
// inside component:
const { lang, setLang, t } = useT();
// ...
<label className="lang-switch">
  {t("settings.language")}
  <select value={lang} onChange={(e) => setLang(e.target.value as "en" | "zh-TW")}>
    <option value="en">English</option>
    <option value="zh-TW">繁體中文</option>
  </select>
</label>
```

- [ ] **Step 4: Build gate + manual switch test**

Run: `npm run build && npm run dev`
Expected: PASS; toggling the select flips visible strings (verifiable after Task 8 wires dashboard strings); selection persists across reload (localStorage).

- [ ] **Step 5: Commit**

```bash
git add ui/src/main.tsx ui/src/App.tsx
git commit -m "feat(hub-ui): mount LanguageProvider and add language switcher"
```

---

## Task 8: Dashboard — restyle + grid/list views + hero + externalize strings

**Files:**
- Modify: `ui/src/components/DashboardApp.tsx`
- Modify: `ui/src/styles/components/dashboard.css`

- [ ] **Step 1: Read the component** to map current markup and the existing grid/list toggle.

Run: `cat ui/src/components/DashboardApp.tsx`
Note existing `title="Grid view"/"List view"` toggle and `CATEGORY_COLORS` map (lines ~15-22) — reuse them.

- [ ] **Step 2: Restyle `dashboard.css`** to the v5 mockup. Replace the dashboard rules with token-driven layout. Key specs (apply to the existing class names you saw in Step 1; rename only if cleaner):

```css
.dashboard { background:
  radial-gradient(900px 300px at 80% -120px, rgba(74,144,217,.12), transparent 70%), var(--surface-bg);
  min-height:100vh; }
.dashboard__bar { display:flex; align-items:center; justify-content:space-between; padding:18px 24px 14px; }
.dashboard__hero { display:flex; align-items:center; gap:18px; margin:4px 24px 8px; padding:18px 22px;
  background:linear-gradient(120deg,var(--surface-card),#F2F8FF); border:1px solid var(--border-line);
  border-radius:var(--radius-xl); box-shadow:var(--shadow-1); }
.dashboard__hero h3 { margin:0; font-size:var(--text-lg); font-weight:var(--fw-heavy); color:var(--text-primary); }
.dashboard__hero p { margin:4px 0 0; font-size:var(--text-base); color:var(--text-secondary); }
.dashboard__stats { margin-left:auto; display:flex; gap:8px; }
.stat { text-align:center; background:var(--surface-card); border:1px solid var(--border-line);
  border-radius:var(--radius-lg); padding:9px 15px; min-width:62px; }
.stat__n { font-size:var(--text-lg); font-weight:var(--fw-heavy); color:var(--text-primary); line-height:1; }
.stat__n--run { color:var(--status-running); } .stat__n--cor { color:var(--color-accent); }
.stat__l { font-size:var(--text-xs); color:var(--text-secondary); margin-top:4px; text-transform:uppercase; letter-spacing:.04em; }

.view-toggle { display:flex; background:var(--surface-card); border:1px solid var(--border-line);
  border-radius:var(--radius-md); padding:3px; box-shadow:var(--shadow-sm); }
.view-toggle button { width:32px; height:28px; display:grid; place-items:center; border:none;
  background:transparent; border-radius:var(--radius-sm); cursor:pointer; color:var(--text-secondary); }
.view-toggle button.is-active { background:var(--color-brand-soft); color:var(--color-brand-strong); }

/* GRID */
.agent-grid { display:grid; grid-template-columns:repeat(3,1fr); gap:14px; padding:6px 24px 18px; }
.grid-card { --cat:var(--cat-custom); position:relative; background:var(--surface-card);
  border:1px solid var(--border-line); border-radius:var(--radius-2xl); padding:16px; cursor:pointer; overflow:hidden;
  transition:transform var(--dur-slow) var(--ease-out), box-shadow var(--dur-slow) var(--ease-out), border-color var(--dur-base); }
.grid-card::before { content:""; position:absolute; left:0; right:0; top:0; height:3px; background:var(--cat);
  transform:scaleX(0); transform-origin:left; transition:transform var(--dur-slow) var(--ease-out); }
.grid-card:hover { transform:translateY(-6px) scale(1.012); border-color:transparent;
  box-shadow:0 18px 36px -10px color-mix(in srgb, var(--cat) 42%, transparent), 0 6px 14px rgba(16,40,80,.08); }
.grid-card:hover::before { transform:scaleX(1); }
.grid-card__avatar { width:44px; height:44px; border-radius:var(--radius-lg); display:grid; place-items:center;
  font-size:21px; flex:none; transition:transform var(--dur-base) var(--ease-out); }
.grid-card:hover .grid-card__avatar { transform:scale(1.12) translateY(-1px); }
.grid-card__foot { display:flex; align-items:center; justify-content:space-between; margin-top:15px;
  padding-top:13px; border-top:1px solid #F0F4F9; }
.grid-card__open { font-size:var(--text-sm); font-weight:var(--fw-bold); color:var(--color-brand);
  display:inline-flex; gap:4px; transition:gap var(--dur-fast), color var(--dur-fast); }
.grid-card:hover .grid-card__open { gap:9px; color:var(--color-brand-strong); }

/* LIST */
.agent-list { display:flex; flex-direction:column; gap:6px; padding:6px 24px 18px; }
.agent-list__head, .list-row { display:grid; grid-template-columns:2.4fr 1fr 1fr 1.2fr 70px; gap:14px; align-items:center; }
.agent-list__head { padding:0 16px 6px; font-size:var(--text-xs); font-weight:var(--fw-bold);
  letter-spacing:.06em; text-transform:uppercase; color:var(--text-tertiary); }
.list-row { --cat:var(--cat-custom); position:relative; background:var(--surface-card);
  border:1px solid var(--border-line); border-radius:13px; padding:12px 16px; cursor:pointer;
  transition:transform var(--dur-fast) var(--ease-out), box-shadow var(--dur-fast), border-color var(--dur-fast); }
.list-row::before { content:""; position:absolute; left:0; top:10px; bottom:10px; width:3px; border-radius:3px;
  background:var(--cat); opacity:0; transition:opacity var(--dur-fast); }
.list-row:hover { transform:translateX(3px); box-shadow:0 6px 16px rgba(16,40,80,.09); }
.list-row:hover::before { opacity:1; }
.list-row__open { justify-self:end; font-size:var(--text-sm); font-weight:var(--fw-bold);
  color:var(--color-brand); opacity:0; transition:opacity var(--dur-fast); }
.list-row:hover .list-row__open { opacity:1; }
```

- [ ] **Step 3: Wire category color** onto each card/row by setting `style={{ ["--cat" as any]: CATEGORY_COLORS[agent.category] }}` on the card/row element, and apply `.grid-card__open` / status `.pill` markup per the mockup. Mount `<Mascot floating />` in the hero and `.stat` chips for running/idle/unread counts.

- [ ] **Step 4: Externalize dashboard strings.** Add `import { useT } from "../i18n"`, get `const { t } = useT()`. Replace literals:
  - `New Agent` → `t("app.newAgent")`
  - `Import Agent` → `t("app.importAgent")`
  - `Applications`/heading → `t("dashboard.heading")`
  - `placeholder="Search… (⌘K)"` → `t("dashboard.search")`
  - `No agents yet` → empty-state using `t("dashboard.empty.title")` + `t("dashboard.empty.body")` + `t("dashboard.empty.cta")` with `<Mascot floating size={96} />`
  - `title="Grid view"`/`title="List view"` → `t("view.grid")`/`t("view.list")`
  - status labels → `t("status.running"|"status.idle"|"status.error")`
  - hero greeting/flock line → `t("dashboard.greeting",{name})` / `t("dashboard.flockStatus",{running,idle})`
  The pre-existing Chinese `title="目前的大腦 — 點此升級…"` → add key `dashboard.brainTooltip` to both tables and use `t("dashboard.brainTooltip")`.

- [ ] **Step 5: Build + lint + visual gate**

Run: `npm run build && npm run lint && npm run dev`
Expected: PASS; dashboard matches the v5 mockup (hero mascot with easter egg, grid hover glow + top line, list rows, view toggle switches, stats). Toggle the language switcher: dashboard strings flip.

- [ ] **Step 6: Commit**

```bash
git add ui/src/components/DashboardApp.tsx ui/src/styles/components/dashboard.css ui/src/i18n
git commit -m "feat(hub-ui): restyle dashboard (grid/list/hero/mascot) and externalize strings"
```

---

## Task 9: DetailPanel — restyle + externalize

**Files:**
- Modify: `ui/src/components/DetailPanel.tsx`, `ui/src/styles/components/detail-panel.css`
- Modify: `ui/src/i18n/{en,zh-TW}.ts` (add keys below)

- [ ] **Step 1: Add i18n keys** (append to both tables; zh-TW values shown):

| key | en | zh-TW |
|---|---|---|
| detail.category | Category | 分類 |
| detail.description | Description | 描述 |
| detail.descPlaceholder | What this agent does… | 這個 agent 的工作… |
| detail.persona | Persona | 人格 |
| detail.tone | Tone | 語氣 |
| detail.risk | Risk tolerance | 風險容忍度 |
| detail.verbosity | Verbosity | 詳盡度 |
| detail.quiet | Quiet | 精簡 |
| detail.normal | Normal | 一般 |
| detail.lively | Lively | 活潑 |
| detail.style | Style | 風格 |
| detail.currentStyle | Current style | 目前風格 |
| detail.renderStatus | Render status | 算圖狀態 |
| detail.notRendered | Not rendered yet | 尚未算圖 |
| detail.presetGallery | Preset gallery | 風格庫 |
| detail.capabilities | Capabilities | 能力 |
| detail.mcpServers | MCP Servers | MCP 伺服器 |
| detail.skills | Skills | 技能 |
| detail.noMcp | No MCP servers configured. | 尚未設定 MCP 伺服器。 |
| detail.noSkills | No skills installed. | 尚未安裝技能。 |
| detail.noCaps | No special capabilities declared. | 未宣告特殊能力。 |
| detail.save | Save Changes | 儲存變更 |
| detail.close | Close | 關閉 |
| action.stop | Stop | 停止 |
| action.share | Share | 分享 |
| action.export | Export | 匯出 |

- [ ] **Step 2: Restyle `detail-panel.css`** to the mockup:

```css
.detail-panel { width:380px; background:var(--surface-card); box-shadow:-12px 0 30px rgba(16,40,80,.10);
  display:flex; flex-direction:column; }
.detail-panel__header { position:relative; padding:20px 20px 16px;
  background:linear-gradient(135deg,var(--color-brand-soft),var(--surface-card)); border-bottom:1px solid var(--border-line); }
.detail-panel__avatar { width:52px; height:52px; border-radius:15px; display:grid; place-items:center;
  font-size:26px; background:var(--surface-card); box-shadow:0 2px 8px rgba(16,40,80,.1); flex:none; }
.detail-panel__name { font-size:var(--text-xl); font-weight:var(--fw-heavy); color:var(--text-primary); }
.detail-panel__actions { display:flex; gap:8px; margin-top:15px; }
.detail-panel__body { padding:18px 20px 24px; overflow:auto; }
.detail-section { margin-bottom:22px; }
.detail-section > h4 { margin:0 0 10px; font-size:var(--text-xs); font-weight:var(--fw-bold);
  letter-spacing:.07em; text-transform:uppercase; color:var(--text-tertiary); }
.meter { display:flex; gap:3px; }
.meter i { width:18px; height:6px; border-radius:3px; background:#E4EBF3; }
.meter i.is-on { background:var(--color-brand); }
.style-gallery { display:grid; grid-template-columns:repeat(3,1fr); gap:8px; }
.style-thumb { aspect-ratio:1/.7; border-radius:var(--radius-md); border:2px solid transparent;
  background:#F4F7FB; display:grid; place-items:center; font-size:20px; cursor:pointer; transition:.15s; }
.style-thumb.is-selected { border-color:var(--color-brand); box-shadow:0 0 0 3px rgba(74,144,217,.18); }
.cap-tag { font-size:var(--text-base); font-weight:var(--fw-semi); padding:5px 10px; border-radius:var(--radius-md);
  background:var(--surface-card); border:1px solid var(--border-line); color:var(--text-primary);
  display:inline-flex; gap:6px; align-items:center; }
.cap-tag .cap-dot { width:7px; height:7px; border-radius:50%; background:var(--cat,var(--color-brand)); }
```

- [ ] **Step 3: Apply classes + `t()`** in `DetailPanel.tsx`: header avatar/name/status pill + `action.stop/share/export` buttons (Stop = `.btn--danger`); sections use the new keys; Risk/Verbosity as `.meter`; Style as `.style-gallery`; MCP/Skills as `.cap-tag` (skill tags set `--cat` per category color). Replace all literals from the Step-1 table.

- [ ] **Step 4: Build + lint + visual gate**

Run: `npm run build && npm run lint && npm run dev`
Expected: PASS; panel matches mockup; strings flip with language.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/DetailPanel.tsx ui/src/styles/components/detail-panel.css ui/src/i18n
git commit -m "feat(hub-ui): restyle DetailPanel and externalize strings"
```

---

## Task 10: Popover + empty state — restyle + externalize

**Files:**
- Modify: `ui/src/components/PopoverApp.tsx`, `ui/src/components/AgentRow.tsx`, `ui/src/styles/components/popover.css`
- Modify: `ui/src/i18n/{en,zh-TW}.ts`

- [ ] **Step 1: Add keys**

| key | en | zh-TW |
|---|---|---|
| popover.search | Search agents… (⌘F) | 搜尋 agents… (⌘F) |
| popover.noneFound | No agents found. | 找不到 agents。 |
| popover.group.running | Running | 執行中 |
| popover.group.idle | Idle | 閒置 |
| popover.openHub | Open Hub | 開啟 Hub |
| common.new | New | 新增 |

- [ ] **Step 2: Restyle `popover.css`** to mockup (300px card, search `.field`, grouped `.agent-row` hover blue-soft, running dot `breathe-ring`, footer with `Open Hub` + coral `New`):

```css
.popover { width:300px; background:var(--surface-card); border:1px solid var(--border-line);
  border-radius:var(--radius-xl); box-shadow:var(--shadow-pop); overflow:hidden; }
.popover__search { margin:12px; }
.agent-group__header { font-size:var(--text-xs); font-weight:var(--fw-bold); letter-spacing:.08em;
  text-transform:uppercase; color:var(--text-tertiary); padding:8px 14px 4px; }
.agent-row { display:flex; align-items:center; gap:10px; padding:8px 12px; margin:0 6px;
  border-radius:var(--radius-md); cursor:pointer; transition:background var(--dur-fast); }
.agent-row:hover { background:var(--color-brand-soft); }
.agent-row__status { width:7px; height:7px; border-radius:50%; flex:none; }
.agent-row__status--run { background:var(--green-500); animation:breathe-ring 2.4s ease-in-out infinite; }
.agent-row__status--idle { background:var(--status-idle); }
.popover__footer { display:flex; gap:8px; padding:10px 12px; border-top:1px solid var(--border-line);
  background:var(--surface-secondary); }
```

- [ ] **Step 3: Apply `t()`** in `PopoverApp.tsx`/`AgentRow.tsx`: search placeholder, group headers (Running/Idle), `No agents found.`, footer buttons. Use `<Mascot>` empty-state in PopoverApp if it has a no-agents branch.

- [ ] **Step 4: Build + lint + visual gate** — `npm run build && npm run lint && npm run dev`. Expected PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/PopoverApp.tsx ui/src/components/AgentRow.tsx ui/src/styles/components/popover.css ui/src/i18n
git commit -m "feat(hub-ui): restyle popover/empty-state and externalize strings"
```

---

## Task 11: Wizard — restyle stepper + externalize

**Files:**
- Modify: `ui/src/components/wizard/WizardModal.tsx` + `ui/src/components/wizard/steps/Step{1..6}*.tsx`
- Modify: `ui/src/styles/components/wizard.css`, `ui/src/i18n/{en,zh-TW}.ts`

- [ ] **Step 1: Read each step** to collect its visible strings.

Run: `for f in ui/src/components/wizard/steps/*.tsx ui/src/components/wizard/WizardModal.tsx; do echo "== $f =="; grep -oE ">[A-Z][A-Za-z ./'&!?-]+<|placeholder=\"[^\"]+\"|\"[A-Z][a-z][A-Za-z ]+\"" "$f"; done`

- [ ] **Step 2: Add a `wizard.*` key per collected string** to `en.ts`/`zh-TW.ts` (namespace by step, e.g. `wizard.step.persona`, `wizard.next`, `wizard.back`, `wizard.finish`, plus each field label/placeholder found). Translate zh-TW for each.

- [ ] **Step 3: Restyle `wizard.css` stepper** to the components mockup:

```css
.wizard-stepper { display:flex; align-items:center; }
.wizard-step { display:flex; align-items:center; gap:9px; }
.wizard-step__circle { width:28px; height:28px; border-radius:50%; display:grid; place-items:center;
  font-size:var(--text-sm); font-weight:var(--fw-bold); background:var(--surface-card);
  border:2px solid var(--border-line); color:var(--text-secondary); }
.wizard-step.is-done .wizard-step__circle { background:var(--color-brand); border-color:var(--color-brand); color:#fff; }
.wizard-step.is-current .wizard-step__circle { border-color:var(--color-accent); color:var(--color-accent-strong);
  box-shadow:0 0 0 4px rgba(251,107,83,.15); }
.wizard-step__label { font-size:var(--text-sm); font-weight:var(--fw-semi); color:var(--text-secondary); }
.wizard-step.is-current .wizard-step__label { color:var(--text-primary); }
.wizard-step__line { width:46px; height:2px; background:var(--border-line); margin:0 12px; }
.wizard-step__line.is-done { background:var(--color-brand); }
.wizard__nav .btn--primary { /* Finish/Next uses coral primary */ }
```

- [ ] **Step 4: Apply classes + `t()`** across WizardModal + all six steps. Buttons use `.btn--primary`/`.btn--secondary`. Inputs use `.field`.

- [ ] **Step 5: Build + lint + visual gate** — `npm run build && npm run lint && npm run dev`, walk the wizard. Expected PASS.

- [ ] **Step 6: Commit**

```bash
git add ui/src/components/wizard ui/src/styles/components/wizard.css ui/src/i18n
git commit -m "feat(hub-ui): restyle wizard stepper and externalize strings"
```

---

## Task 12: Modals (muragent + preset) — restyle + externalize

**Files:**
- Modify: `ui/src/components/MuragentImportModal.tsx`, `ui/src/components/PresetImportModal.tsx`, `ui/src/styles/components/modal.css`, `ui/src/i18n/{en,zh-TW}.ts`

- [ ] **Step 1: Add keys** for every string found earlier (Import Agent, Import, Model, Provider, Set up a model, Spawn a pet window, Spawn MCP servers, Select a file first, Signature is invalid, Installed/Updated, Import Style Preset, Import from URL, the three placeholders, etc.) namespaced `modal.import.*` / `modal.preset.*`. Provide zh-TW for each.

- [ ] **Step 2: Restyle `modal.css`** to the components mockup:

```css
.modal { width:360px; background:var(--surface-card); border:1px solid var(--border-line);
  border-radius:var(--radius-xl); box-shadow:0 24px 60px rgba(16,40,80,.22); overflow:hidden; }
.modal__header { padding:18px 20px; border-bottom:1px solid var(--border-line);
  display:flex; align-items:center; justify-content:space-between; }
.modal__title { font-size:var(--text-md); font-weight:var(--fw-heavy); color:var(--text-primary); }
.modal__body { padding:20px; }
.modal__footer { display:flex; justify-content:flex-end; gap:8px; padding:14px 20px;
  border-top:1px solid var(--border-line); background:var(--surface-secondary); }
.dropzone { border:2px dashed #CFE0F1; border-radius:var(--radius-lg); background:var(--surface-bg);
  padding:26px; text-align:center; color:var(--text-secondary); font-size:var(--text-base); }
.dropzone b { color:var(--color-brand-strong); }
.modal__overlay { position:fixed; inset:0; background:rgba(16,40,80,.35); display:grid; place-items:center; }
```

- [ ] **Step 3: Apply classes + `t()`** in both modals. Footer buttons `.btn--secondary`/`.btn--primary`; dropzone `.dropzone`; inputs `.field`.

- [ ] **Step 4: Build + lint + visual gate** — `npm run build && npm run lint && npm run dev`. Expected PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/MuragentImportModal.tsx ui/src/components/PresetImportModal.tsx ui/src/styles/components/modal.css ui/src/i18n
git commit -m "feat(hub-ui): restyle import modals and externalize strings"
```

---

## Task 13: Companion inbox + Pet — restyle + externalize

**Files:**
- Modify: `ui/src/components/CompanionInbox.tsx`, `ui/src/components/PetApp.tsx`, `ui/src/styles/components/companion.css`, `ui/src/styles/components/pet.css`, `ui/src/i18n/{en,zh-TW}.ts`

- [ ] **Step 1: Add keys** for companion/pet strings (No messages yet., No thanks, Not now, Save it, Good/Bad/Dismiss titles, Return to Hub, Close). zh-TW translations.

- [ ] **Step 2: Restyle `companion.css`** (message cards on `--surface-card`, accent buttons `.btn--*`, empty state) and `pet.css` (apply brand motion tokens; the pet already uses `<img>`+initials fallback — keep, but route the context menu + bubble onto tokens). The Pet bubble and sprite keep existing behavior; only colors/radii/shadows move to tokens.

- [ ] **Step 3: Apply `t()`** in both components. Pet context menu `📥 Return to Hub` / `✕ Close` → `t("pet.returnToHub")` / `t("common.close")`.

- [ ] **Step 4: Build + lint + visual gate** — `npm run build && npm run lint && npm run dev`. Expected PASS.

- [ ] **Step 5: Commit**

```bash
git add ui/src/components/CompanionInbox.tsx ui/src/components/PetApp.tsx ui/src/styles/components/companion.css ui/src/styles/components/pet.css ui/src/i18n
git commit -m "feat(hub-ui): restyle companion inbox + pet and externalize strings"
```

---

## Task 14: Final QA — strings, dark mode, file sizes

**Files:** none (verification + small fixes)

- [ ] **Step 1: Hardcoded-string sweep.** Find any remaining user-visible literal not behind `t()`:

Run: `grep -rnE ">[A-Z][a-z]+ ?[A-Za-z]*<|placeholder=\"[A-Za-z]|title=\"[A-Za-z上-龥]" ui/src/components`
For each real hit, add a key + `t()`. (Ignore code identifiers, class names, `aria-hidden`.)

- [ ] **Step 2: Translation completeness.** `zh-TW.ts` is typed `Table`, so a missing key fails `tsc`. Confirm:

Run: `npm run build`
Expected: PASS (no TS2741 missing-property errors). Also `npm test` → i18n tests green.

- [ ] **Step 3: Dark-mode pass.** Toggle OS appearance to dark (or temporarily set `<html data-theme="dark">` in devtools) and `npm run dev`. Check each surface for contrast/legibility; fix offending `--d-*` values in `primitives.css` only.

- [ ] **Step 4: File-size rule.** Ensure no new file exceeds 800 lines:

Run: `find ui/src/styles ui/src/i18n -name '*.css' -o -name '*.ts' | xargs wc -l | sort -n | tail`
If `primitives.css` is large, split atoms into `components/buttons.css` etc. and add to the barrel.

- [ ] **Step 5: Lint clean.** `npm run lint` → no errors/warnings.

- [ ] **Step 6: Commit any fixes**

```bash
git add -A ui/src
git commit -m "chore(hub-ui): final QA — string sweep, dark-mode + file-size fixes"
```

---

## Self-Review notes (coverage)

- Spec §3 (token-first split) → Tasks 1–3. §4 (token values) → Task 1. §5.1 dashboard → Task 8; §5.2 detail → 9; §5.3 popover → 10; §5.4 empty → 8/10; §5.5 pet → 13. §6 shared components → Task 4 (+ wizard 11, modal 12). §2 mascot + easter egg → Task 5, mounted in 8/10. §7 i18n → Tasks 6–7, applied 8–13. §8 order → task ordering. §9 risks (CSS mascot, dark-mode QA, <800 lines) → Task 14.
- No `vitest`-style tests for pure-CSS tasks by design — verification is build + lint + manual visual, stated per task. The one logic unit (i18n `translate`) is TDD'd in Task 6.
