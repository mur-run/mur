# MUR Hub 2.0 — Phase 3(b): side-peek from Home

**Date:** 2026-09-06 · **Status:** Approved; implemented in PR 11
**Follows:** `2026-09-06-mur-hub-master-detail-shell-design.md` (§10 Phase 3), `2026-09-06-mur-hub-chats-master-detail-design.md` (Phase 3(a), #1177–#1178: `ChatPane`, the attention reducer wiring), `2026-09-06-mur-hub-open-in-window-design.md` (Phase 2(b): `FleetDetailPane`, `DetailWindow`, `openDetailWindow`).
**Scope:** `mur-hub-gui/ui` only. No Rust change.

## 1. Problem

Home is mission control, but nothing on it leads anywhere useful: the "Now running" agent chips are inert `div`s; the running-channel and recent-activity rows call `onNavigate("chats")` and drop the channel on the way (the Chats page opens with no selection); the Needs-you HITL and companion cards act inline but cannot show the conversation they belong to. The shell spec deferred two answers to this — a side-peek from Home and a space-bar Quick Look on lists — and this spec settles both.

## 2. Decisions

| Question | Decision | Rejected |
|---|---|---|
| Quick Look on `SourceList` rows | **Dropped.** On a master–detail page the selected row's detail already fills the right pane; a space-bar preview of the same thing is redundant, and previewing "a different facet" per page is a rule to learn. The shell spec's deferred list is updated to say so. | Keep it, previewing another facet per page. |
| Presentation | **A right-anchored slide-over panel** (`PeekPanel`) with a light scrim, over the content column, while Home stays where it is. Esc / scrim click close it; the panel's own header has **Go** (to the page, selected) and **Open in window**. | A centred modal (cramped for a conversation); Home as a two-column master–detail (cards get squeezed). |
| What a peek shows | **A conversation or a fleet.** Agent chip, HITL card, companion card → that agent's `ChatPane` (the HITL card is visible in the thread). `fleet-*` channel row → `FleetDetailPane` opened on **Jobs**. Any other multi-agent channel row has no viewer in the app → keeps today's behaviour (Chats page). Install / upgrade cards are global → no peek. | `AgentDetail` (six tabs) for everything agent-shaped: far from "what is it doing right now", and the HITL card would sit one click deeper. |
| Where the state lives | **`DashboardApp`** holds `peek`; the panel is rendered there. It needs `openChatWith`, `runtimeMap`, `channels`, and the fleet jump — all `DashboardApp`'s. | Inside `HomePage`: every callback would still be threaded down, and the panel could not overlay the content column. |
| Attention | Peeking a conversation is reading it: on open the panel `openConversation` + `focusConversation`s the agent (clears its flags); on close it `blurConversation`s — the Chats page's rule (3(a) §4). | Leave the flags: the unread dot would survive the user having just read the thread. |

## 3. Peek targets

`components/peek/peekModel.ts` (pure, tested):

```ts
export type PeekTarget = { kind: "chat"; agent: string } | { kind: "fleet"; name: string };

export const FLEET_CHANNEL_PREFIX = "fleet-";

/** What a Home channel row can peek: a fleet's channel → that fleet; an
 *  agent's primary channel (id == agent name) → that chat; anything else →
 *  null, and the caller keeps today's navigation. */
export function peekTargetForChannel(channel: { id: string }, agentNames: ReadonlySet<string>): PeekTarget | null
```

`DashboardApp`: `const [peek, setPeek] = useState<PeekTarget | null>(null)`; `openPeek(target)`, `closePeek()` (stable callbacks).

## 4. Home entry points

`HomePage` gains `onPeek: (target: PeekTarget) => void` and an `agentNames` set derived from `agents`.

- **Now running — agent chips** (`NowRunning`): the `div.home-run-agent` becomes a `<button type="button" className="home-run-agent">` with `aria-label` = the display name; click → `onPeekAgent(name)` (new prop) → `onPeek({ kind: "chat", agent })`.
- **Now running — channel rows** and **Recent activity rows**: `onOpen(channel)` keeps its signature; `HomePage.openChat(ch)` becomes: `const t = peekTargetForChannel(ch, agentNames); if (t) onPeek(t); else onNavigate("chats");`. `NowRunning`'s row `onClick={onOpen}` (which today ignores the channel) becomes `onClick={() => onOpen(ch)}` so the channel reaches the mapper.
- **Needs you — HITL and companion cards**: a `home-card__peek` link-button ("View conversation") in the card header, rendered only when `item.agent` is set, calling `onPeekAgent(item.agent)`. `NeedsYou` gains `onPeekAgent: (name: string) => void`. Approve / deny stay inline and untouched. Install and upgrade cards are unchanged.

## 5. `PeekPanel`

`components/peek/PeekPanel.tsx`, rendered by `DashboardApp` after `<Shell>` (a sibling, not inside the page) whenever `peek !== null`:

```
<div className="peek__scrim" onClick={onClose} />
<aside className="peek" role="dialog" aria-modal="true" aria-label={title}>
  <header className="peek__bar">
    <span className="peek__title">{title}</span>
    <div className="peek__actions">
      <button class="btn btn--secondary">Go</button>            // peek.goLabel
      <button class="btn btn--secondary">Open in window</button> // action.openInWindow
      <button class="peek__close" aria-label={detail.close} ref={closeRef}>×</button>
    </div>
  </header>
  <div className="peek__body">…ChatPane | FleetHost…</div>
</aside>
```

- **Props:** `{ target: PeekTarget; agents: AgentEntry[]; runtimeMap: Map<string, AgentRuntimeStatus>; channels: ChannelSummary[]; onClose: () => void; onGo: (t: PeekTarget) => void; onOpenInWindow: (t: PeekTarget, title: string) => void; onOpenAgent: (name: string) => void }` — `title` is the display name the panel shows, so the window gets it too.
- **Chat body:** `const entry = agents.find(a => a.name === target.agent)`; if absent → `<p className="peek__missing">{t("detailWindow.missingAgent")}</p>`; else `const item = buildChatList([entry], attention, channels)[0]` (`attention` from `useConversations()`) and `<ChatPane item={item} runtime={runtimeMap.get(name)} onOpenAgent={onOpenAgent} />`. `ChatPane`'s **Open agent** button keeps its meaning (the Agents page with that agent selected): `PeekPanel` takes `onOpenAgent: (name: string) => void`, `DashboardApp` passes its existing `openAgentFromChat` wrapped to close the peek first. Title = `entry.display_name`.
- **Fleet body:** `FleetHost` (§6) with `initialTab="jobs"`, `onDeleted={onClose}`, missing text `detailWindow.missingFleet`. Title = the fleet's display name once `fleet_list` answers, the name before.
- **Go:** chat → `openChatWith(agent)` (existing: Chats page with that agent); fleet → `setFleetRequest(name); setPage("fleets")` (the palette's path). Both then `closePeek()`.
- **Open in window:** chat → `popOutChat(agent)`; fleet → `openDetailWindow("fleet", name, title)` with the panel's title. Both then `closePeek()`.
- **Keys:** a window `keydown` listener registered by `PeekPanel` (capture phase): `Escape` → `onClose` and `stopPropagation` (so `DashboardApp`'s global Esc, which clears selections, does not also fire); `isOpenInWindowShortcut(e)` → the Open-in-window action (not while `isEditingTarget(document.activeElement)` — the compose box). `DashboardApp`'s own ⌘↩ branch is unaffected because Home has no selection.
- **Focus:** on mount, focus the close button; on unmount, restore focus to the element that was active before (captured in a ref on mount). No focus trap (Tab can leave the panel) — recorded as a known limitation; a trap is a later refinement.
- **Attention:** on mount `openConversation(agent); focusConversation(agent)` for a chat target; on unmount `blurConversation()`.
- **CSS** (`styles/components/peek.css`): scrim `position: fixed; inset: 0; background: var(--scrim)` (new semantic token `--scrim: rgba(16,24,40,.32)` light / `rgba(0,0,0,.5)` dark, in `semantic.css`), `z-index: var(--z-peek)` (new primitive `--z-peek: 900`, below `.modal__overlay`'s 1000 so a confirm dialog opened from the peek still wins). Panel: `position: fixed; top: 0; right: 0; bottom: 0; width: min(560px, calc(100vw - var(--shell-sidebar-width) - 40px)); background: var(--surface-detail); box-shadow: var(--shadow-pop); display: flex; flex-direction: column;` with `transform: translateX(0)` animated from `translateX(100%)` over `var(--dur-base) var(--ease-out)` via a `@keyframes peek-in`, disabled under `prefers-reduced-motion`. `.peek__bar` = 44px, flex, gap; `.peek__body { flex: 1; min-height: 0; display: flex; flex-direction: column; }` so `ChatPane` / `FleetDetailPane` (both `height: 100%` flex columns) fill it.

## 6. `FleetHost` (pure movement)

`DetailWindow.tsx`'s `FleetBody` (the `fleet_list` + `fleet_labels_list` loader, the `agentMap`, the missing state, the `FleetDetailPane` render) moves to `components/detail/fleet/FleetHost.tsx`:

```ts
export interface FleetHostProps {
  name: string;
  initialTab?: FleetTabId;      // default "overview"
  onDeleted: () => void;
  /** Rendered when fleet_list does not list `name`. */
  missing: ReactNode;
  onOpenInWindow?: () => void;
}
```

`DetailWindow` renders `<FleetHost name missing={<Missing text=… />} onDeleted={close} />`; `PeekPanel` renders `<FleetHost name initialTab="jobs" missing={<p className="peek__missing">…</p>} onDeleted={onClose} />`. `FleetDetailPane` gains `initialTab?: FleetTabId` and seeds its `tab` state from it. `AgentBody` stays in `DetailWindow` (the peek uses `ChatPane`, not `AgentDetail`).

## 7. Errors and empty states

- Channel with no target → today's `onNavigate("chats")`; nothing new.
- Agent no longer listed → the panel body says `detailWindow.missingAgent`; header actions still work (Go lands on Chats, which shows its own state).
- Fleet no longer listed → `detailWindow.missingFleet` via `FleetHost`'s `missing`.
- `open_chat_window` / `open_detail_window` failing → the existing toasts (`popOutChat`, `openDetailWindow`).
- Deleting the fleet from inside the peek → `onDeleted` closes the panel; the Fleets page will not list it on its next load.

## 8. Keyboard summary

| Key | Peek closed | Peek open |
|---|---|---|
| Esc | clears the page's selection (existing) | closes the peek only |
| ⌘↩ | page's open-in-window (Agents / Fleets / Chats) | the peek's Open in window, then close |
| ⌘K, ⌘F, ⌘\ | as today | as today (the panel does not intercept them) |

## 9. Testing

- `peekModel.test.ts`: `fleet-night-ops` → fleet `night-ops`; `aura` with `aura` in the set → chat; `aura-2` / `shared-x` / unknown → null.
- `chatList` unchanged; `FleetDetailPane` `initialTab` is exercised by the browser list.
- Browser acceptance (stubbed bridge): from Home, an agent chip, a HITL card's "View conversation", and a companion card each open the panel with that agent's `ChatPane` (header shows model · channel · turns; compose box present); a `fleet-x` row opens the fleet on the Jobs tab; a multi-agent non-fleet row still navigates to Chats; **Go** lands on Chats with the agent selected / Fleets with the fleet selected; **Open in window** calls `open_chat_window` / `open_detail_window` and closes; Esc and the scrim close; while open, Esc does not clear another page's stored selection; a fired `chat-delta` for the peeked agent does not mark it unread while the peek is open and does after it closes; an unknown agent shows the missing text; `#/detail/fleet/<name>` still renders (the `FleetHost` extraction).
- Real build: the slide-in motion and reduced-motion, the dark scrim.

## 10. Implementation order

One PR (**PR 11**, branch `feat/hub-3b-peek`): (1) `FleetHost` extraction + `FleetDetailPane.initialTab`; (2) `peekModel` + test, tokens, `peek.css`, `PeekPanel`, `DashboardApp` state / keys / render; (3) Home entry points (`NowRunning` chips as buttons and channel rows passing the channel, `RecentActivity` unchanged, `NeedsYou` peek buttons, `HomePage.onPeek`) + strings; (4) the shell spec's deferred list marks Quick Look as dropped.

## 11. Later

- A focus trap in `PeekPanel`.
- A generic channel viewer (read-only event list) so non-fleet multi-agent channels can peek too.
- Peek from other non-master–detail surfaces (Settings, the palette) once they exist.
