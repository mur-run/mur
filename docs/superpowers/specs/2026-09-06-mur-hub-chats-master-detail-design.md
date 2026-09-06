# MUR Hub 2.0 — Phase 3(a): the Chats page on the master–detail shell

**Date:** 2026-09-06 · **Status:** Draft — awaiting review
**Follows:** `2026-09-06-mur-hub-master-detail-shell-design.md` (§10 Phase 3), `2026-09-06-mur-hub-library-master-detail-design.md` (Phase 2(a), #1171–#1173), `2026-09-06-mur-hub-open-in-window-design.md` (Phase 2(b), #1174–#1176).
**Scope:** `mur-hub-gui/ui` only. No Rust change.

## 1. Problem

Chats is the last page not on the Phase 1 shell. It draws its own list (`chats-view__list`, `chats-item*`), auto-selects the first agent, and is the only remaining user of the right-hand inspector column: `ChatInspector` shows three facts (model, channel id, turn count) in a 320px column that squeezes the conversation. The standalone chat window (`AgentChatWindow`) shows a live tool-call pill above the compose box; the page does not.

Phase 3 per the shell spec is five independent pieces — Chats, Home side-peek + Quick Look, `SourceList` multi-select, Settings as a page — and this spec is the first: Chats. Finishing it removes the inspector concept from the app entirely (Shell prop, ⌘⌥I toggle, CSS track, token), so every page shares one shape.

## 2. Decisions

| Question | Decision | Rejected |
|---|---|---|
| List unit | **One row per agent** (the current mental model; `ChatTab` loads only the agent's primary channel). | One row per channel: `ChatTab` cannot be pointed at a channel, so a fleet-channel row would have no conversation to continue. |
| The inspector's three facts | **A `DetailPage`-style header above the conversation**, fed from data already on the page: model from `AgentEntry.model_id`, channel id and turns from the `ChannelSummary` the dashboard already loads. `ChatInspector` deleted; the Shell's inspector plumbing deleted with it. | Keep the column: a third pane for three lines, and the conversation loses 320px. |
| Conversation body | **`ChatTab` + `TaskPill`** above the compose box, matching the window. **No channel rail**: it switches nothing anywhere (`AgentChatWindow` passes `activeChannelId` only to the rail and its title, never to `ChatTab`), so on the page it would read as broken. | Rail for parity; "add channel switching" is its own feature. |
| Row content | Subtitle = the primary channel's `preview`; sort by its `updated_at` (this fills `ChatListItem.lastActivityMs`, which exists today and is never set). Badges: runtime `StatusDot`, HITL as the amber needs-you badge, unread as a brand-coloured dot. Chips: All / Needs you / Unread. | Name-only rows (today). |
| Selection | Persisted (`mur.chats.lastSelected`), restored once; **no auto-select of the first agent**; `initialAgent` (Agents → Chat) wins. | Auto-select (today): the page opens on whichever agent sorts first, which changes as unread state changes. |
| ⌘↩ / double-click on Chats | **Pop the chat out** (the existing `open_chat_window`). The "open in window" gesture from Phase 2(b) means the same thing here; the chat window is that window. | Nothing, or a second chat window kind. |
| Home / Settings / multi-select / Quick Look / side-peek | Not in this spec (Phase 3(b)–(e)). | — |

## 3. Layout

`ChatsPage` renders the same `master-detail` grid the Agents page does: `SourceList` · `ListDivider` (`useResizableColumn("mur.chats.listWidth", …)`) · the chat pane. Below 960px the list overlays (`listModeFor`, `master-detail--overlay`, the "Show list" button) exactly as on Agents.

- **SourceList config.** Title `nav.chats`, count = agents, filter placeholder `chats.filter`, `allLabel` = `dashboard.all`, facets from §4, `onSelect` → select (no dirty guard: the chat has no forms), `onOpen` → pop-out, no "+" (`createLabel` still required by the prop; pass `chats.new` … no: `onCreate` and `createItems` both absent hides "+", per PR 6). Empty state: `chats.empty` when there are no agents, `chats.noMatch` when the filter hides all.
- **Selection.** `selected` state; on first non-empty agent list restore `readKey("mur.chats.lastSelected")` once (guarded by a ref, the AgentsPage pattern: never re-fills after Esc); write on change after the restore ran. `initialAgent` sets the selection whenever it changes (today's effect, kept) and clears itself in `DashboardApp` after being applied — `DashboardApp` gains `clearChatInitial` like `clearFleetRequest`, so opening the same agent's chat twice from Agents works.
- **No selection.** `chats.selectHint` ("Select an agent to start chatting.") centred in the pane, the Fleets pattern.
- **`onActiveChange`** and the `query` prop are removed from `ChatsPage` (the inspector was their only consumer; the palette's search never reached this page).

## 4. Rows (`chatList.ts`)

`buildChatList(agents, attention, channels, query)` gains `channels: ChannelSummary[]`. For each agent the primary channel is `channels.find((c) => c.id === a.name)` (the id rule `ChatInspector` and `ChatChannelRail` already use). New / filled fields on `ChatListItem`:

```ts
export interface ChatListItem {
  name: string;
  displayName: string;
  agent: AgentEntry;
  unread: boolean;
  hitl: boolean;
  /** Epoch ms of the primary channel's updated_at; undefined without a channel. */
  lastActivityMs?: number;
  /** The primary channel's preview line; undefined without a channel. */
  preview?: string;
  /** The primary channel's id and turn count, for the header meta. */
  channelId?: string;
  turns?: number;
}
```

`sortConversations` is unchanged and now sorts by real activity. `chatRows(items, runtimeMap, nowMs, labels): SourceRowData[]` (new, pure) maps to rows: `subtitle` = `preview` with `relativeTime(updated_at, nowMs)` (`work/format.ts`) appended after ` · `, or `chats.noChannel` when there is none; `status` = `statusOf(runtimeMap.get(name)?.state)`; `needsYou` = `hitl ? 1 : 0`; `unread` = `unread`; `facets` = `["needsYou"]` when `hitl`, `["unread"]` when `unread` (both when both); avatar = `PetFace` at 28px (the Agents row). `chatFacets(items, labels)` yields `{ id: "needsYou", label: chats.facet.needsYou, count }` and `{ id: "unread", … }`, each only when its count is > 0.

**`SourceRowData.unread?: boolean`** is new. `SourceList` renders `<span className="source-row__unread" aria-label={unreadLabel} />` before the name when true; `SourceListProps.unreadLabel?: string` supplies the accessible label (`chats.unread`). CSS: an 8px `--color-brand` disc. Markup without `unread` is byte-identical (test).

## 5. Chat pane

- **`DetailHeader`** (`components/shell/DetailHeader.tsx`) is `DetailPage`'s `<header className="detail-page__head">` block extracted verbatim — props `{ avatar, title, status?, meta?, actions? }`; `DetailPage` renders `<DetailHeader …/>` in its place. Pure movement; existing `DetailPage` tests keep passing and `DetailHeader` gets its own markup test (no `useT` inside, so `renderToStaticMarkup` works).
- **`ChatPane`** (`components/chats/ChatPane.tsx`): `<section className="chat-pane">` = `DetailHeader` + `<div className="chat-pane__body"><ChatTab key={name} agentName displayName aboveCompose={<TaskPill agentName />} /></div>`.
  - avatar: `PetFace` 48px; title: display name; status: `statusOf(runtime?.state)`.
  - meta: `<span className="mono">{agent.model_id}</span> · <span className="mono">{channelId}</span> · {t("chatInspector.turns", { count })}`, or `chats.noChannel` in place of the last two when the agent has no channel.
  - actions: **Pop out** (`btn btn--secondary`, `chat.popout`, → `open_chat_window`) and **Open agent** (`btn btn--secondary`, `chats.openAgent`, → `onOpenAgent(name)`; `DashboardApp` implements it as `setSelected(name); setPage("agents")`).
- **Layout** (`styles/components/chats.css`, replacing the `.chats-*` rules in `work.css`): `.chat-pane { display: flex; flex-direction: column; height: 100%; min-height: 0; }`, `.chat-pane__body { flex: 1; min-height: 0; display: flex; }` so `ChatTab`'s `.chat` (already `height: 100%; min-height: 0`) scrolls its log and pins its compose box. The `.detail-page__head` padding is reused; the body gets `padding: 0 var(--space-8) var(--space-6)`.

## 6. Inspector retirement

- Delete `components/inspector/ChatInspector.tsx` and `components/shell/Inspector.tsx` (with `hasInspector`, `InspectorSelection`).
- `Shell`: remove the `inspector` prop, `inspectorVisible`, `isInspectorToggle`, the ⌘⌥I branch, the `shell--with-inspector` class and the `.shell__inspector` render. `shell.css`: delete `.shell--with-inspector`, `.shell--sidebar-collapsed.shell--with-inspector`, `.shell__inspector`. `primitives.css`: delete `--shell-inspector-width`. `shell.test.ts`: delete the `isInspectorToggle` describe.
- `DashboardApp`: remove `chatAgent`, `setChatAgent`, `onChatActive`, `inspectorSelection`, `inspectorNode`, the `inspector={…}` prop, the `Inspector` import, and `setChatAgent(null)` from the Esc handler.
- i18n: delete `chatInspector.subtitle`, `chatInspector.model`, `chatInspector.channel`; keep `chatInspector.turns` and rename `chatInspector.noChannel` → `chats.noChannel` (both used by §4–§5). New keys: `chats.selectHint`, `chats.noMatch`, `chats.openAgent`, `chats.unread`, `chats.facet.needsYou`, `chats.facet.unread`.
- CSS: delete `.chats-view*`, `.chats-item*` from `work.css` and `.conv-badge*` from `dashboard.css` (ChatsPage was their only user — `grep` confirms before deleting).
- `detail-panel.css` rules the inspector used (`.detail-panel--inspector`, `.detail-panel__close`) stay if the agent tabs still reference `.detail-panel*`; the plan greps and deletes only orphans.

## 7. Keyboard and gestures

- ⌘↩ on the Chats page with a selection → `open_chat_window` (the `DashboardApp` keydown branch from Phase 2(b) gains `page === "chats" && selectedChat`; `ChatsPage` reports its selection through `onSelect?: (name: string | null) => void`, stored as `selectedChat`). Not while a text field is focused — which covers the compose box, so ⌘↩ never fires from mid-message.
- Double-click a row → the same, via `SourceList.onOpen`.
- The ⌘K "Open <name> in window" action is agent-page only and unchanged; the Chats page's pop-out is the header button, ⌘↩, or double-click.
- ⌘F, ↑↓, Enter, Esc: `SourceList`'s existing behaviour. Esc clears the selection → the hint state.
- ⌘⌥I no longer does anything (removed with the inspector).

## 8. Errors and empty states

- No agents: `chats.empty` fills the page (today's behaviour), no list.
- Filter hides everything: `chats.noMatch` in the list; the pane keeps the current selection.
- Selected agent without a channel: subtitle and meta say `chats.noChannel`; `ChatTab` still renders (it creates the channel on first send, as today).
- `channel_list` failing: `useChannels` already yields `[]`; rows fall back to name-only with `chats.noChannel`, nothing else changes.
- `open_chat_window` failing: toast, as in Phase 2(b)'s `openDetailWindow`; reuse `showToast` from `fleetActions`.

## 9. Testing

- `chatList.test.ts` (extend): `buildChatList` joins the primary channel by id (preview, `lastActivityMs` from `updated_at`, `channelId`, `turns`), ignores fleet channels, sorts by activity; `chatRows` subtitle / status / needsYou / unread / facets; `chatFacets` omits zero-count chips.
- `SourceList.test.tsx`: markup identical without `unread`; contains `source-row__unread` with it.
- `DetailHeader.test.tsx` (new): renders avatar, title, status pill, meta, actions; omits meta / actions when absent.
- `shell.test.ts`: `isInspectorToggle` tests removed; `isSidebarToggle` tests stay.
- Browser acceptance (stubbed bridge): list shows preview + relative time, status dots, needs-you badge and unread dot from stubbed attention events; chips filter; last selection restores; `initialAgent` from Agents → Chat selects and clears; header shows model · channel · turns; Pop out and ⌘↩ and double-click call `open_chat_window`; Open agent lands on Agents with the agent selected; `TaskPill` appears above the compose box; no third column on any page; ⌘⌥I does nothing.

## 10. Implementation order

One PR (**PR 10**, branch `feat/hub-3-chats`), tasks in this order so each commit builds: (1) `DetailHeader` extraction; (2) `chatList.ts` join + `chatRows` / `chatFacets` + `SourceRowData.unread` + `SourceList` dot; (3) `ChatPane` + `ChatsPage` rewrite + `DashboardApp` wiring (`selectedChat`, `clearChatInitial`, `onOpenAgent`, ⌘↩ branch) + `chats.css`; (4) inspector retirement (files, Shell, CSS, token, tests, i18n).

## 11. Later (Phase 3(b)–(e))

Home side-peek + Quick Look (one "peek" mechanism), `SourceList` multi-select with bulk Start / Stop, Settings as a page, and channel switching in `ChatTab` (which would bring the rail back on both surfaces).
