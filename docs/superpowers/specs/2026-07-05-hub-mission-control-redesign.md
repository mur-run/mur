# MUR Hub Mission-Control Redesign — Design

**Date:** 2026-07-05
**Status:** Approved (brainstormed with David)

## Problem

The Hub grew by accretion: one 959-line `DashboardApp`, four top-level
views (Conversations, Chats, Work, Fleet), a full-page DetailPanel, and
**eleven modals** (Wizard, PresetImport, MuragentImport, Settings,
ModelSetupWizard, ModelPicker, InstallInbox, SkillAddUrl, SkillRegistry,
McpAddRemote, McpDiscover). New capabilities (install inbox, skill
upgrade pipeline, role packs, model library) have nowhere natural to
live. Things that need the user's decision (HITL approvals, install
consents, companion messages, blocked skill upgrades) are scattered
across four surfaces.

## Decisions (brainstorm outcomes)

- Scope: **full redesign** — information architecture + visual, phased.
- Center of gravity: **mission control** (status + pending decisions
  first; chat is one entry among several).
- Visual: **macOS-native** feel, NOT aligned to the app.mur.run web
  dashboard (desktop = local tool, web = cloud account; deliberate
  contrast).
- Chosen approach: **new three-pane native shell** (sidebar / content /
  inspector), old views ported page by page. Rejected: incremental
  reskin (mission control + unified inbox don't fit the old top-level
  structure); full rewrite including state management (React context
  layer isn't broken; no user-visible payoff).

## Design

### 1. Shell: three-pane layout

```
┌──────────┬────────────────────────┬───────────┐
│ sidebar  │      content area      │ inspector │
│ (vibrancy│                        │ (⌘⌥I,     │
│  source  │                        │  auto-hide│
│  list)   │                        │  when no  │
│          │                        │  selection│
└──────────┴────────────────────────┴───────────┘
```

Sidebar groups:

```
WORKSPACE                LIBRARY
Home (badge)             Skills
Chats                    Workflows
Agents                   MCP Servers
Fleets                   Models
                         Plugins
```

### 2. Home = mission control + unified Inbox

Top-to-bottom:

1. **Needs You (unified Inbox)** — shown only when non-empty. Four
   sources merged into one time-sorted list, each item an in-place
   actionable card:
   - HITL approvals (reuse `HitlCard`)
   - Install requests (relay one-click install; card opens the consent
     dialog)
   - Companion messages (reuse `CompanionInbox` data)
   - Blocked skill upgrades (`BlockedModified` from
     `mur skill upgrade --check --json`) with keep / overwrite / diff
     actions on the card
   Sidebar Home item and the Dock icon carry the total badge.
2. **Now Running** — running agents (state + one-line current task) and
   active fleet loops (iteration, budget spend, convergence; sourced
   from fleet `.last_run` + channel tail). Empty state: mascot + quick
   actions (New chat / Run fleet / Create agent).
3. **Recent Activity** — recent cross-agent channel events, each row
   jumps to the matching Chat.

Data layer: nothing new. Inbox is a frontend aggregator (`useInbox()`
hook) over the four existing event/jsonl mechanisms; actions call the
existing tauri commands; read/processed state stays with each source's
own mechanism (HITL approve, install `.done`, etc.) — no new read-state
store.

### 3. Page inventory (old → new)

| Current | Destination |
|---|---|
| ConversationsView + ChatsView (two parallel chat UIs) | **one Chats page**: conversation list (cross-agent, ConversationRail folded in) + thread pane |
| DashboardApp agent grid | **Agents page** (grid kept); card click opens inspector instead of full-page DetailPanel |
| FleetView | **Fleets page** — ported as-is (recently redesigned), new container + skin only |
| WorkView | folded into Home (Now Running + Recent Activity); **tab deleted** |
| SkillRegistryModal + SkillAddUrlModal | **Skills page**: installed list (origin stamp / upgrade-status column) + registry browse + URL install |
| McpDiscoverModal + McpAddRemoteModal | **MCP Servers page**: same pattern |
| ModelLibrary/Panels + ModelPickerModal | **Models page** (library content becomes the page); ModelPicker stays a dialog (it's a chooser, not a browser) |
| plugin import/discover | **Plugins page** |
| — | **Workflows page** (new, small): `~/.mur/workflows/` list + run history; receives Dashboard workflow installs |
| WizardModal, ModelSetupWizard, import modals, install consent, Settings | stay dialogs (task flows); Settings on ⌘, per macOS convention |

All five Library pages share one component family:
`InstalledList + DiscoverGrid + install-source buttons`. Every
installed list shows upgrade status from
`mur skill upgrade --check --json`.

The only behavior change is the Chats merge; everything else is
relocation + reskin.

### 4. Inspector

Replaces the full-page DetailPanel; contextual:

- Agents selection → agent detail (existing tabs: status / skills /
  MCP / permissions / Memory / Mobile — `MemoryTab`, `MobileTab`
  reused directly)
- Chats selection → quick agent panel (model, tokens, channel info)
- Fleets selection → members + loop state
- Library selection → manifest detail, origin/version, README
- No selection → auto-collapse

### 5. Visual language (macOS-native)

- System font stack (`-apple-system`), 13px base; sidebar vibrancy
  (Tauri transparent window + CSS backdrop-filter; fall back to solid
  if WKWebView misbehaves — validate in phase 1, non-blocking)
- Monochrome SF-Symbols-style icons (extend the existing currentColor
  SVG pattern); emoji only for mascot/pet
- Light/dark follows the system; palette collapses to system accent +
  semantic colors (running=green, blocked=orange, error=red) + neutral
  grays; CATEGORY_COLORS desaturated to fit
- 8px radii, compact lists (Sonoma conventions); motion limited to
  sidebar transitions and inbox card enter/leave
- Brand copy: uppercase "MUR" everywhere user-visible

### 6. Migration order (each phase shippable, own PR)

1. **Shell**: three-pane layout + sidebar + routing; old views dropped
   into content unchanged; begin `DashboardApp` decomposition
   (shell / Home / GridCard as separate files, ≤800 lines each)
2. **Home + unified Inbox**; delete WorkView tab
3. **Library pages** (shared template; one page per PR:
   Skills → MCP → Models → Plugins → Workflows)
4. **Chats merge** + DetailPanel → inspector
5. **Visual polish**: vibrancy, dark/light tuning, icon unification

### 7. Testing

- Existing vitest pattern per phase (helpers/logic tests)
- Required: inbox merge-sort + badge count tests
- Visual acceptance via the verified local .app build recipe
  (sidecars from installed app, npx tauri build, ad-hoc sign)

## Out of scope

- Pet / Popover windows (unchanged)
- mur-server dashboard visuals (deliberately distinct)
- State-management rewrite
- Onboarding wizard content (container reskin only)
