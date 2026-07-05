# Hub Redesign Phase 2: Home + Unified Inbox Implementation Plan (2/5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Home page = Needs You (unified inbox over HITL / install requests / companion / blocked upgrades) + Now Running + Recent Activity; WorkView tab deleted.

**Architecture:** Frontend aggregator `useInbox()` over four existing sources; no new read-state store — actions call each source's existing tauri command. One new tauri command surfaces blocked upgrades by shelling `mur skill upgrade --check --json` (CLI from origin-pipeline Plan 4). Spec §2. **Depends on Phase 1 shell.**

**Tech Stack:** React + TS + vitest; one small Rust tauri command.

## Global Constraints

- Same as Phase 1; Rust side: nextest + clippy, `MUR_WEB_DIST`/`ORT_STRATEGY` env for mur-core-linked builds.
- Inbox is fail-open per source: one source erroring must not blank the others.

---

### Task 1: Inbox data model + merge logic

**Files:**
- Create: `mur-hub-gui/ui/src/components/home/inbox.ts`
- Test: `mur-hub-gui/ui/src/components/home/inbox.test.ts`

**Interfaces (produces):**

```ts
export type InboxKind = "hitl" | "install" | "companion" | "upgrade_blocked";
export interface InboxItem {
  kind: InboxKind; id: string;        // unique within kind
  ts: number;                          // unix seconds, sort key
  title: string; subtitle: string;
  payload: unknown;                    // kind-specific, cast at the card
}
export function mergeInbox(sources: InboxItem[][]): InboxItem[]; // desc by ts, stable, dedup by kind+id
export function inboxBadge(items: InboxItem[]): number;          // === items.length
```

- [ ] **Step 1: Failing tests** — merge sorts desc across sources; stable for equal ts; dedup `kind+id` keeps newest; empty sources → `[]`; badge counts all.
- [ ] **Step 2:** `npm test -- inbox` fail. **Step 3:** implement (pure functions). **Step 4:** PASS. **Step 5:** commit `feat(hub-ui): inbox merge model`.

### Task 2: Blocked-upgrades tauri command

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mcp_skills.rs` (or new `upgrade_status.rs` if near 800 lines; register in `lib.rs` invoke_handler)
- Test: `#[cfg(test)]` for the JSON mapping

**Interfaces (produces):** tauri command `skill_upgrade_status() -> Vec<BlockedItem>` where `BlockedItem { name, dir, local_version, latest_version }` — runs the CLI (`mur skill upgrade --check --json` via the same resolved-CLI-path helper other commands use — grep `cli_tools.rs`), parses the report, returns only `BlockedModified` items. CLI missing/old → `Ok(vec![])` + `tracing::warn` (fail-open, per constraint).

- [ ] **Step 1: Failing test** — parse a canned report JSON (one Upgraded, one BlockedModified, one UpToDate) → exactly the one blocked item mapped.
- [ ] **Step 2:** fail. **Step 3:** implement. **Step 4:** `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml upgrade_status` PASS (needs `ui/dist` stub if UI not built — known gotcha). **Step 5:** commit `feat(hub): skill_upgrade_status command`.

### Task 3: `useInbox()` hook wiring four sources

**Files:**
- Create: `mur-hub-gui/ui/src/components/home/useInbox.ts`
- Test: `mur-hub-gui/ui/src/components/home/useInboxAdapters.test.ts`

**Interfaces:**
- Consumes: existing HITL pending query (grep the tauri command `HitlCard`/`hitl.rs` uses), install-request inbox list (`install_inbox_list` from relay-install plan), companion unread (`useUnreadCount`/`CompanionInbox` data source), Task 2's `skill_upgrade_status`.
- Produces: `useInbox(): { items: InboxItem[]; refresh(): void }` — per-source adapter functions `hitlToItem/installToItem/companionToItem/blockedToItem` (exported for tests), polling/event-listen matching each source's existing update mechanism (listen() events where they exist, 30s poll for upgrade status).

- [ ] **Step 1: Failing tests** — each adapter maps a representative raw object to a correct InboxItem (kind, id, ts, title); an adapter given a malformed object returns null and is filtered (fail-open).
- [ ] **Step 2:** fail. **Step 3:** implement adapters + hook (hook body thin; logic in adapters). **Step 4:** PASS. **Step 5:** commit `feat(hub-ui): useInbox aggregator`.

### Task 4: Home page UI + WorkView removal

**Files:**
- Create: `mur-hub-gui/ui/src/components/home/HomePage.tsx`, `NeedsYou.tsx`, `NowRunning.tsx`, `RecentActivity.tsx`
- Create: `mur-hub-gui/ui/src/styles/components/home.css`
- Modify: `DashboardApp.tsx` — home placeholder → HomePage; default page = home; delete the work nav entry; badge prop = `inboxBadge(items)`; Dock badge via the tauri badge API mirroring the count
- Delete: `mur-hub-gui/ui/src/components/work/` (WorkView) — AFTER confirming Now Running/Recent Activity covers its data (WorkView's data hooks move, don't die: reuse its agent-status and activity sources)
- Test: existing suites green

- [ ] **Step 1:** NeedsYou renders card-per-item by kind: hitl→embed `HitlCard`; install→summary card, click opens the existing consent dialog; companion→message card reusing CompanionInbox styles; upgrade_blocked→card with keep (dismiss for this session) / overwrite (`invoke` a new thin `skill_upgrade_apply_one(name)` that re-runs upgrade for that skill with apply) / diff (opens read-only diff modal fetching local vs registry yaml). Section hidden when empty.
- [ ] **Step 2:** NowRunning + RecentActivity built on WorkView's existing data sources (move the hooks). Empty state: Mascot + three quick actions (New chat → chats page; Run fleet → fleets; Create agent → WizardModal).
- [ ] **Step 3:** Remove work from nav + delete component dir; `npm test` + `npm run build` green.
- [ ] **Step 4:** .app smoke: badge on sidebar + Dock; each card kind actionable end-to-end (fake one HITL via a channel workflow with `risk: write`; one install via Dashboard click; one blocked upgrade by editing an origin-stamped skill).
- [ ] **Step 5:** Commit `feat(hub-ui): mission-control Home with unified inbox; remove Work tab`.
