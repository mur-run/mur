# MuR Hub Visual Redesign + Lightweight i18n — Design

Date: 2026-06-04
Status: Approved (design); pending implementation plan
Scope: `mur-hub-gui/ui` (Tauri 2 + React 18 + Vite + TypeScript)

## 1. Goal

Restyle the MuR Hub desktop UI to a **modern, clean, brand-faithful** look that keeps MuR's
playful personality, and — because every component's JSX is rewritten during the restyle —
externalize all user-visible strings behind a **lightweight i18n layer** in the same pass
(touch each file once, not twice).

Non-goals: backend/runtime changes, new GUI features, Tauri command changes, the legacy
`mur-agent-gui` app. Pure presentation + string externalization.

## 2. Brand Direction

Derived from the official site (app.mur.run): the MuR identity is a friendly **blue starling
mascot** (murmuration), **coral CTA**, airy **light-blue** surfaces, bold **dark-navy**
headings. Professional but warm. **Not** indigo/violet (an earlier exploration that was rejected).

Personality lives in four places, applied with restraint so the UI stays professional:
1. **Mascot** — the starling appears in the dashboard hero, empty states, and the desktop Pet.
2. **Signature easter egg** — wherever the bird appears, hovering it raises its eyebrows and
   shifts its pupils toward the cursor ("it looks at you"). This is the house micro-interaction.
3. **Color** — vivid but soft category tints + coral/blue accents; no grey-on-grey.
4. **Copy** — light, murmuration-themed microcopy and emoji in empty/hero/toasts (e.g.
   "你的鳥群正在學習中"), never cold system language.

Motion is subtle: lift on hover, springy but small; no gratuitous rotation.

## 3. Architecture — Token-first vanilla CSS

Keep the existing stack (plain CSS + CSS custom properties). No Tailwind, no CSS Modules,
**zero new styling dependencies**. Rationale: a 9-component app does not justify a framework
migration that would churn every component and diverge from existing patterns; a framework is
not itself a design system. A two-tier token system is the actual best practice and the
lowest-risk path.

Split the current single 1184-line `styles.css` (over the 800-line/file rule) into:

```
ui/src/styles/
├── index.css              # barrel: @import everything (referenced from main entry)
├── tokens/
│   ├── primitives.css     # raw scales: color ramps, space, radius, shadow, type, motion
│   └── semantic.css       # semantic tokens + dark-mode overrides (components consume ONLY this)
├── base.css               # reset, html/body, font stack, focus-visible
├── motion.css             # keyframes + reusable motion utility classes
└── components/
    ├── dashboard.css      # grid + list views, GridCard, hero, view toggle
    ├── detail-panel.css
    ├── popover.css
    ├── pet.css
    ├── companion.css
    ├── wizard.css
    ├── modal.css          # preset / muragent import modals
    └── primitives.css     # button, badge, status-pill, toast, avatar, field, empty-state
```

Two-tier tokens: components reference **only** semantic tokens; semantic maps to primitives.
Theme/brand changes touch one file (`semantic.css`); components never change.

```
primitive            →  semantic                →  component usage
--blue-500 #4A90D9      --color-brand              color: var(--color-brand)
--coral-500 #FB6B53     --color-accent            background: var(--surface-card)
--space-3 12px          --surface-card            box-shadow: var(--shadow-card)
--radius-lg 16px        --text-primary
--ease-spring …         --shadow-card
```

Migration strategy: **pure code movement first** (relocate existing rules into the new files,
extract tokens), then visual value changes — committed separately for review/revert. Behavior
changes never mix with movement (CLAUDE.md rule 4).

## 4. Design Tokens (values)

### Color — primitives
- Brand blue: `--blue-700 #2F6FB0`, `--blue-500 #4A90D9`, `--blue-soft #EAF3FC`
- Accent coral: `--coral-600 #F0563D`, `--coral-500 #FB6B53`, `--coral-soft #FFF1EE`
- Category: research `#4A90D9`, automation `#10B981`, monitor `#F59E0B`, notify `#EF4444`,
  commerce `#8B5CF6`, custom `#64748B` (each with a ~12% soft tint for badges/avatars)
- Status: running `#22C55E`, idle `#94A3B8`, restarting `#F59E0B`, failed `#EF4444`
- Neutral light: bg `#F6FAFE`, secondary `#EAF3FC`, card `#FFFFFF`, hover `#EAF1F9`,
  line `#EAF0F7`, text `#16243B`, text-2 `#64758A`, text-3 `#9AA8B8`
- Neutral dark: bg `#0F1117`, secondary `#161922`, card `#1A1E29`, hover `#232838`,
  line `rgba(255,255,255,.08)`, text `#F1F5F9`, text-2 `#94A3B8`

### Semantic (examples)
`--color-brand`, `--color-accent`, `--surface-bg`, `--surface-card`, `--surface-hover`,
`--border-line`, `--text-primary/secondary/tertiary`, `--status-running/idle/failed`,
`--shadow-card/pop/focus`. Dark mode redefines these under `@media (prefers-color-scheme: dark)`;
a future manual switch only needs `[data-theme="dark"]` — components untouched.

### Scales
- Space (4px base): 2·4·6·8·12·16·20·24·32·40
- Radius: sm 6 · md 10 · lg 14/16 · xl 18/20 · full 9999
- Type (base 14, system font stack): xs 11 · sm 12.5 · base 14 · md 15 · lg 17/18 · xl 19/20 · 2xl 22/26; weights 400/600/700/800
- Shadow: `--shadow-sm 0 1px 2px rgba(16,40,80,.06)`; `--shadow-card 0 1px 3px rgba(16,40,80,.08)`;
  `--shadow-pop 0 16px 40px rgba(16,40,80,.16)`; `--shadow-focus 0 0 0 3px rgba(74,144,217,.18)`
- Motion: `--dur-fast 130ms / --dur-base 180ms / --dur-slow 280ms`;
  `--ease-out cubic-bezier(.2,.85,.25,1)`, `--ease-spring cubic-bezier(.34,1.56,.64,1)`

## 5. Surface specs

### 5.1 Dashboard
- Top bar: `MUR` wordmark + segmented tabs (Agents / Activity / Settings), right side: search,
  **view toggle (grid ▦ / list ☰)**, coral `+ New Agent`.
- **Hero** (visible in both views): floating starling mascot (with the eyebrow/eye easter egg),
  greeting + murmuration copy, and 3 stat chips (running / idle / unread).
- **View toggle** persists user choice (localStorage). Two modes:
  - **Grid**: cards with category-tinted hover glow shadow, a top accent line that grows from
    the left on hover, avatar pop, `Open →` arrow spread. Status as green/grey/red pill (running
    pulses). Category badge + footer divider.
  - **List**: aligned columns (Agent / Category / Status / Uptime / action). Row hover: slight
    translateX, category color bar fades in on the left edge, `Open →` appears.
- Drag-to-spawn (existing behavior) preserved.

### 5.2 DetailPanel
Right-side slide-in over a dimmed list. Header on a light-blue gradient: large rounded avatar,
name, status pill, `Stop` (coral-soft) / `Share` / `Export` (ghost). Sections: Category (badge),
Description, Persona (Tone chip, Risk/Verbosity as dot-meters), Style (selectable thumbnail
gallery with blue selected ring + render status), Capabilities (MCP Servers / Skills as colored-dot tags).

### 5.3 Menubar Popover
Compact 300px dropdown: search, agents grouped by Running / Idle, footer `Open Hub` +
coral `+ New`. Running dot pulses.

### 5.4 Empty state
Large floating + wing-flapping starling (with easter egg), murmuration copy
("鳥群還沒成形"), coral CTA to create the first agent.

### 5.5 Pet
Desktop pet keeps per-agent avatar image (initials fallback) + expression states; apply the
same hover easter egg and brand motion tokens.

## 6. Shared components (design language)
Button (primary=coral, blue, secondary/ghost, danger=coral-soft, link, sm, disabled);
badge (5 category tints); status pill (running/idle/error); field (with blue focus ring);
toast (success=green / error=coral left bar); wizard stepper (done=blue solid ✓, current=coral
ring); modal (import .muragent with blue dashed dropzone, Cancel/Import footer). All consume
semantic tokens only.

## 7. Lightweight i18n

No heavy dependency. Custom layer:
- `ui/src/i18n/` — `index.ts` (a `LanguageContext`, `useT()` hook returning `t(key, vars?)`),
  `en.ts`, `zh-TW.ts` (flat keyed string tables; supports `{var}` interpolation).
- Architecture allows adding `zh-CN` / `ja` later by dropping in a new table.
- **All user-visible strings externalized** to keys during the restyle (satisfies the
  "no hardcoded values" rule). Ship `en` + `zh-TW`.
- Language switcher in Settings; selection persisted to localStorage; default follows OS locale,
  falling back to `en`.
- Keys namespaced by surface, e.g. `dashboard.newAgent`, `status.running`, `modal.import.title`.

This is infrastructure + en/zh-TW content, done in the same component pass — not a separate
file-by-file rewrite later.

## 8. Implementation order
1. Token layer + file split (pure movement, then values).
2. Shared primitives (button/badge/pill/field/toast/empty-state) + motion + mascot CSS/easter egg.
3. i18n scaffold (`LanguageContext`, `t()`, `en`/`zh-TW`, Settings switcher).
4. Dashboard (grid + list + view toggle + hero), externalizing its strings as it is restyled.
5. DetailPanel → Popover → Wizard → Modals → Companion → Pet, each restyled + strings externalized.
6. Visual QA in light/dark; verify the easter egg on every mascot instance.

## 9. Risks / notes
- Mascot is currently a per-agent avatar image; the brand starling has no illustration asset.
  v1 uses a CSS-rendered starling (validated in the brainstorm mockups); a real illustration can
  replace it later without structural change.
- Dark-mode values are provisional and need a visual QA pass.
- i18n adds translation maintenance; mitigated by a single flat table per language and namespaced keys.
- Keep each new CSS/TS file under 800 lines (CLAUDE.md rule 4).
