# Hub Redesign Phase 2: Home + Unified Inbox Implementation Plan (2/5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Home page = Needs You (unified inbox over HITL / install requests / companion / blocked upgrades) + Now Running + Recent Activity; WorkView tab deleted.

**Architecture:** Frontend aggregator `useInbox()` over four sources; no new read-state store — actions call each source's existing tauri command. Two new backend commands fill verified gaps: `skill_upgrade_status()` (in-process `mur-core::skill_upgrade::upgrade_all`, check mode) and `hitl_pending_list()` (folds unresolved HITL channel gates — no global source existed). Spec §2. **Depends on Phase 1 shell.**

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

**REVISED after code inspection:** `mur-core::cmd::skill_upgrade::upgrade_all(mur_home, registry_dir, apply)` is a lib function and `mur-hub-gui/src-tauri` already depends on `mur-core` (path dep). Call it IN-PROCESS — do NOT shell out to the CLI (no subprocess, no CLI-path resolution, no JSON reparse). Report types: `UpgradeReport { items: Vec<UpgradeItem> }`, `UpgradeItem { name, dir, status }`, `UpgradeStatus::BlockedModified { local, latest }` (see `mur-core/src/cmd/skill_upgrade.rs`).

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/mcp_skills.rs` (or new `upgrade_status.rs` if near 800 lines; register in `lib.rs` invoke_handler)
- Test: `#[cfg(test)]` for the status→BlockedItem mapping

**Interfaces (produces):** tauri command `skill_upgrade_status() -> Result<Vec<BlockedItem>, String>` where `BlockedItem { name, dir, local_version, latest_version }` — calls `upgrade_all(&mur_home, &registry_cache_dir, false)` (apply=false, check mode; `registry_cache_dir` via `mur_core::cmd::skill_registry::registry_cache_dir(&mur_home)` — use the CACHED dir, no network fetch in this status hot path), maps only `UpgradeStatus::BlockedModified{local,latest}` items to `BlockedItem` (local_version/latest_version from the variant). Registry cache absent / any error → `Ok(vec![])` + `tracing::warn` (fail-open per constraint).

- [ ] **Step 1: Failing test** — build a small `UpgradeReport` in-test (one Upgraded, one BlockedModified, one UpToDate) and assert the pure mapping fn `blocked_items(&UpgradeReport) -> Vec<BlockedItem>` returns exactly the one blocked item with correct local/latest. (Keep the mapping a pure fn so it's testable without a real registry.)
- [ ] **Step 2:** fail. **Step 3:** implement pure mapper + thin command wrapper. **Step 4:** `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml upgrade_status` PASS (needs `ui/dist` stub + `MUR_WEB_DIST`/`ORT_STRATEGY` env — known gotchas). **Step 5:** commit `feat(hub): skill_upgrade_status command (in-process)`.

### Task 2b: HITL pending-list tauri command

**Discovered gap:** there is NO global pending-HITL source. HITL requests arrive via a `listen("hitl-approval-needed")` event accumulated in each `ChatTab`'s LOCAL state (`ChatTab.tsx:198`); `hitl.rs` only exposes `agent_hitl_respond`/`channel_hitl_respond`. A unified Home inbox needs to list pending HITL across all agents. HITL requests are persisted as channel events (v3c: `HitlRequest` channel event, resolved by a later `HitlResponse`), so the source of truth is on disk.

**Files:**
- Modify: `mur-hub-gui/src-tauri/src/hitl.rs` (+ register in `lib.rs`)
- Test: `#[cfg(test)]` over a temp channel dir

**Interfaces (produces):** tauri command `hitl_pending_list() -> Result<Vec<HitlRequestView>, String>` — scans channels for `HitlRequest` events with no matching `HitlResponse` (unresolved gates), returns them with `{ channel_id, hitl_id, agent, summary, risk, ts }`. First check `mur-core` for an existing "list pending HITL" helper (grep `hitl` in `mur-core/src/` — `mur channel approve` must enumerate them somewhere); reuse it if present, else add a thin `mur-core` fn that folds channel events and returns unresolved gates. Any read error → `Ok(vec![])` + warn (fail-open).

- [ ] **Step 1: Failing test** — temp channel with one HitlRequest (unresolved) + one HitlRequest later resolved by a HitlResponse → list returns exactly the unresolved one.
- [ ] **Step 2:** fail. **Step 3:** implement (reuse mur-core fold if it exists). **Step 4:** `cargo nextest run --manifest-path mur-hub-gui/src-tauri/Cargo.toml hitl_pending` PASS. **Step 5:** commit `feat(hub): hitl_pending_list command`.

### Task 3: `useInbox()` hook wiring four sources

**Files:**
- Create: `mur-hub-gui/ui/src/components/home/useInbox.ts`
- Test: `mur-hub-gui/ui/src/components/home/useInboxAdapters.test.ts`

**Interfaces:**
- Consumes (verified sources): `hitl_pending_list()` (Task 2b); `install_inbox_list()` (exists, `install_inbox.rs:105` → `Vec<InstallRequestView>`); companion per-agent `companion_bridge_pending({agent})` → `BridgeEvent[]` (`CompanionInbox.tsx:35` — aggregate across agents from the AgentContext list); Task 2's `skill_upgrade_status()`.
- Produces: `useInbox(): { items: InboxItem[]; refresh(): void }` — per-source adapter functions `hitlToItem/installToItem/companionToItem/blockedToItem` (exported for tests), each mapping one raw record to an `InboxItem`. Update mechanism per source: HITL also live-updates via the existing `listen("hitl-approval-needed")` event (refresh on fire); companion aggregates over the agent list; install + upgrade via 30s poll + `refresh()`.

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
