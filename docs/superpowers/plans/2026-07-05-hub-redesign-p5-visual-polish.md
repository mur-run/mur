# Hub Redesign Phase 5: Visual Polish Implementation Plan (5/5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Unify the visual language to the macOS-native spec: tokens, icons, dark/light, motion; retire dead CSS.

**Architecture:** Token-level sweep (`styles/tokens/`), then per-component-css alignment, then icon unification. No component logic changes. Spec §5. **Depends on Phases 1–4** (do it last so polish lands on final layouts).

## Global Constraints

- Same as Phase 1. No behavior changes — visual diffs only. `npm test` green at every step.
- Uppercase "MUR" audit is part of this phase.

---

### Task 1: Token consolidation

**Files:** `styles/tokens/primitives.css`, `styles/tokens/semantic.css`, all `styles/components/*.css`

- [ ] Define/verify semantic tokens: `--accent` (system accent via `accent-color`/fallback), `--status-running` (green), `--status-blocked` (orange), `--status-error` (red), neutral gray ramp, `--radius: 8px`, `--font-ui: -apple-system, …`, 13px base.
- [ ] Sweep component CSS: replace hardcoded colors/radii/fonts with tokens (grep hex values; each survivor needs a reason). Desaturate `CATEGORY_COLORS` (in `utils.ts`) to fit the neutral palette.
- [ ] Dark + light verified per page in the .app; commit `style(hub-ui): token consolidation`.

### Task 2: Icon unification

- [ ] Inventory every emoji/icon in nav, buttons, cards (grep for emoji ranges in tsx). Replace UI glyphs with the currentColor SVG set (extend the DashboardApp glyph pattern into `components/shell/icons.tsx`, one exported component per glyph). Emoji remain ONLY for mascot/pet surfaces.
- [ ] Commit `style(hub-ui): monochrome icon set`.

### Task 3: Motion + density pass

- [ ] Remove/disable animations except: sidebar page transition (150ms fade), inbox card enter/leave (150ms slide+fade). Respect `prefers-reduced-motion`.
- [ ] Compact list density (row heights per Sonoma: ~28px lists, 8px paddings); check `motion.css` for retired rules.
- [ ] Commit `style(hub-ui): motion + density pass`.

### Task 4: Cleanup + brand audit

- [ ] Delete now-unused CSS files (`work.css`; any modal css whose modal died in Phase 3 — verify no imports first) and dead components missed in earlier phases (`ConversationRail` etc. if still present).
- [ ] Brand audit: grep UI strings + i18n files for `Mur\b|MuR` → "MUR" (skip identifiers/paths per CLAUDE.md rule 7).
- [ ] Full `npm test` + `npm run build` + .app acceptance in both themes; screenshot set for the PR.
- [ ] Commit `chore(hub-ui): retire dead styles; MUR brand audit`.
