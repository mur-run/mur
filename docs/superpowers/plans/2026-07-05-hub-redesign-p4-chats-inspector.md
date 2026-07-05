# Hub Redesign Phase 4: Chats Merge + Inspector Implementation Plan (4/5)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** One Chats page replaces ConversationsView + ChatsView; DetailPanel becomes the contextual right-pane inspector.

**Architecture:** Chats = conversation list (ConversationRail folded in) + thread pane, backed by the existing ConversationContext (in-memory ConversationStore). Inspector content is selection-driven per page. Spec §3 (Chats row), §4. **Depends on Phase 1 (inspector slot), Phase 3 (library selection).** This is the only phase with real behavior change — the two chat UIs merge.

**Tech Stack:** React + TS + vitest.

## Global Constraints

- Same as Phase 1. Chat FUNCTIONALITY must not regress: streaming, image paste, suggested replies, autocomplete, HITL cards in-thread, per-connection stream isolation — all live in `components/chat/` and ConversationContext; the merge recomposes containers, it must not touch chat internals.

---

### Task 1: Inventory + merge decision table (read-only)

- [ ] Read `ConversationsView.tsx`, `ChatsView.tsx`, `ConversationRail.tsx`, `conversation/` context. Produce a short table IN THE PR DESCRIPTION: for each capability (list, create, resume, per-agent filter, unread, multi-window pop-out `#/chat/`), which of the two views implements it better → that one survives. Anything only ONE has survives by default.
- [ ] No commit (analysis feeds Task 2).

### Task 2: Unified ChatsPage

**Files:**
- Create: `mur-hub-gui/ui/src/components/chats/ChatsPage.tsx` (list pane + thread pane)
- Modify: `DashboardApp.tsx` (chats → ChatsPage)
- Delete: `ConversationsView.tsx`, `ChatsView.tsx`, `ConversationRail.tsx` (logic absorbed)
- Test: extract any nontrivial list logic (sort: latest-activity desc; unread grouping) to `chats/chatList.ts` with `chatList.test.ts`

- [ ] **Step 1: Failing tests** for `sortConversations` (desc by last activity, unread pinned rule if kept) and `groupByAgent`.
- [ ] **Step 2:** fail. **Step 3:** implement page per Task 1 table; thread pane reuses `components/chat/*` untouched; pop-out button keeps `#/chat/` window route.
- [ ] **Step 4:** `npm test` + build; .app smoke: stream a reply, paste an image, open two agent chats concurrently (stream isolation), pop-out window.
- [ ] **Step 5:** Commit `feat(hub-ui): unified Chats page (merges Conversations+Chats views)`.

### Task 3: DetailPanel → Inspector

**Files:**
- Create: `mur-hub-gui/ui/src/components/shell/Inspector.tsx` (thin switch)
- Modify: `DetailPanel.tsx` → rename/move to `inspector/AgentInspector.tsx`, restyle from full-page to 320px column (content unchanged: status/skills/MCP/perm tabs + `MemoryTab` + `MobileTab`)
- Create: `inspector/ChatInspector.tsx` (model, token usage, channel id — data already in ConversationContext/agent status), `inspector/FleetInspector.tsx` (members + loop state from FleetView's existing queries), `inspector/LibraryInspector.tsx` (manifest detail: name/version/origin/readme from the item's `meta`/detail command)
- Modify: `DashboardApp.tsx` — passes `inspector={...}` per page selection; no selection → `undefined` (Shell auto-hides); AgentsPage card click sets selection instead of navigating to full-page detail

- [ ] **Step 1:** AgentInspector conversion (CSS to column layout; tabs become a segmented control; verify ≤800 lines, split tabs into files if not).
- [ ] **Step 2:** Wire all four inspectors + selection state per page.
- [ ] **Step 3:** `npm test` + build; .app smoke: select agent → inspector; ⌘⌥I toggles; Esc/deselect hides; Library item select shows manifest.
- [ ] **Step 4:** Commit `feat(hub-ui): contextual inspector replaces full-page DetailPanel`.
