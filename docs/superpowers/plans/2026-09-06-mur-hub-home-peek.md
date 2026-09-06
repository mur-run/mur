# MUR Hub 2.0 — Phase 3(b) side-peek from Home — implementation plan

> **Execute with `mur-executing-plans`.** Spec: `docs/superpowers/specs/2026-09-06-mur-hub-home-peek-design.md` (§ references below point there). One PR (**PR 11**), three tasks, each commit builds.

## Goal

From Home, an agent chip, a HITL or companion card, or a fleet's channel row opens a right-side peek panel showing that conversation or that fleet's Jobs, without leaving Home.

## Architecture

`DashboardApp` holds `peek: PeekTarget | null` and renders `PeekPanel` beside the `Shell`; the panel reuses `ChatPane` (3(a)) and a `FleetHost` extracted from `DetailWindow` (2(b)). Home's entry points map a channel to a target with the pure `peekTargetForChannel`, falling back to today's navigation when there is none.

## Tech stack

React 18 + TypeScript 5.5 + Vite 5, plain CSS on the two-tier tokens, Vitest 4 without jsdom, the lightweight i18n (`en.ts` defines keys, `zh-TW.ts` is typed `Table`). No Rust.

## Global Constraints

Copied from the design and `CLAUDE.md`. Every task includes all of them.

1. Brand name is uppercase **MUR** in every user-visible string.
2. Single source file ≤ 800 lines.
3. Every new user-visible string lands in both `src/i18n/en.ts` and `src/i18n/zh-TW.ts` in the same commit (`tsc` enforces the table).
4. Components reference only semantic tokens; no raw hex in component CSS or TSX (new tokens go in `tokens/*.css`).
5. No hardcoded numbers or storage keys in TSX: named constants.
6. Never pair `Foo.tsx` with `foo.ts` in one directory (APFS is case-insensitive; Vite and `tsc` resolve the wrong file).
7. Tests never touch the DOM: pure functions, or `renderToStaticMarkup` for markup (`useT` needs a provider, so `PeekPanel` has no markup test).
8. Every commit is gated on the real exit code: `set -o pipefail; npm test 2>&1 | grep …` — never on grep's.
9. No new data path: the peek reads `channel_list` / runtime state through the values `DashboardApp` already holds, and the fleet host runs the same two commands `DetailWindow` ran.
10. Every PR leaves the app usable: `npm run build`, `npm test`, `npm run lint` green and the manual acceptance list passes.

## Working agreement

- Paths are relative to `mur-hub-gui/ui/` unless they start with `docs/`.
- Line numbers cite `main` at `4f13c574` (2026-09-06); re-check with `grep -n` before cutting.
- Commands from `mur-hub-gui/ui/`: `npm test -- <path>`, `npm test`, `npm run build`, `npm run lint`. `npm run lint` reports 6 pre-existing warnings in files this plan does not touch; 0 errors is the bar.
- Browser acceptance: `npm run dev -- --port 5174 --strictPort`, inject the Tauri stub the Phase 1 plan describes (store its source in `sessionStorage`, `eval` it after each reload, click the error boundary's **Try again** by finding the button by text and calling `.click()`), with `plugin:event|listen` storing handler ids per event name so `chat-delta` / `hitl-approval-needed` can be fired from the console.
- Commit after every task with the message given.

## File structure

| File | Responsibility |
|---|---|
| `src/components/detail/fleet/FleetHost.tsx` (new) | `fleet_list` + `fleet_labels_list` loader, `agentMap`, missing slot, renders `FleetDetailPane` |
| `src/components/detail/fleet/FleetDetailPane.tsx` (modify) | `initialTab?` |
| `src/components/detail/window/DetailWindow.tsx` (modify) | renders `FleetHost` instead of its own `FleetBody` |
| `src/components/peek/peekModel.ts` (+ `.test.ts`) (new) | `PeekTarget`, `peekTargetForChannel` |
| `src/components/peek/PeekPanel.tsx` (new) | the slide-over: scrim, header actions, keys, focus, attention, body |
| `src/styles/tokens/primitives.css`, `src/styles/tokens/semantic.css` (modify) | `--z-peek`, `--scrim` |
| `src/styles/components/peek.css` (new) + `src/styles/index.css` (modify) | `.peek*` |
| `src/components/DashboardApp.tsx` (modify) | `peek` state, Esc precedence, `PeekPanel` render, Home props |
| `src/components/home/HomePage.tsx` (modify) | `onPeek`, channel → target mapping |
| `src/components/home/NowRunning.tsx` (modify) | chips as buttons (`onPeekAgent`), rows pass the channel |
| `src/components/home/NeedsYou.tsx` (modify) | "View conversation" on HITL / companion cards (`onPeekAgent`) |
| `src/styles/components/home.css` (modify) | button reset for `.home-run-agent`, `.home-card__peek` |
| `src/i18n/en.ts`, `src/i18n/zh-TW.ts` (modify) | `peek.go`, `peek.viewConversation` |
| `docs/superpowers/specs/2026-09-06-mur-hub-master-detail-shell-design.md` (modify) | Quick Look marked dropped |

---

### Task 11.1 — `FleetHost` extraction and `FleetDetailPane.initialTab`

**Interfaces.** Produces `FleetHost({ name, initialTab?, onDeleted, missing, onOpenInWindow?, onTitle? })` and `FleetDetailPaneProps.initialTab?: FleetTabId`; 11.2 consumes `FleetHost`.

- [x] `src/components/detail/fleet/FleetDetailPane.tsx`: add to `FleetDetailPaneProps` after `onOpenInWindow?`:
  ```ts
  /** Tab to open on; the Home peek opens on Jobs. Default Overview. */
  initialTab?: FleetTabId;
  ```
  destructure `initialTab = "overview"` in the component signature, and change `useState<FleetTabId>("overview")` to `useState<FleetTabId>(initialTab)`.
- [x] Create `src/components/detail/fleet/FleetHost.tsx`:

```tsx
import { useCallback, useEffect, useState, type ReactNode } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { AgentEntry } from "../../../types";
import { useAgents } from "../../../context/AgentContext";
import type { FleetSummary, LabelView } from "../../fleet/types";
import type { FleetTabId } from "../../shell/detailTabs";
import { FleetDetailPane } from "./FleetDetailPane";

export interface FleetHostProps {
  name: string;
  /** Tab to open on. Default Overview. */
  initialTab?: FleetTabId;
  onDeleted: () => void;
  /** Rendered when fleet_list does not list `name`. */
  missing: ReactNode;
  onOpenInWindow?: () => void;
  /** The fleet's display name once fleet_list answers (the peek's title). */
  onTitle?: (displayName: string) => void;
}

/** Everything FleetDetailPane needs from outside a Fleets page: the summary
 *  (status + labels) from fleet_list, the label registry, and the agent map
 *  from the AgentProvider. Shared by the detail window and the Home peek. */
export function FleetHost({ name, initialTab, onDeleted, missing, onOpenInWindow, onTitle }: FleetHostProps) {
  const { agents } = useAgents();
  const [fleets, setFleets] = useState<FleetSummary[] | null>(null);
  const [labels, setLabels] = useState<LabelView[]>([]);
  const [agentMap, setAgentMap] = useState<Map<string, AgentEntry>>(new Map());

  useEffect(() => {
    setAgentMap(new Map(agents.map((a) => [a.name, a])));
  }, [agents]);

  const load = useCallback(() => {
    invoke<FleetSummary[]>("fleet_list").then(setFleets).catch(() => setFleets([]));
    invoke<LabelView[]>("fleet_labels_list").then(setLabels).catch(() => setLabels([]));
  }, []);
  useEffect(load, [load]);

  const displayName = fleets?.find((f) => f.name === name)?.display_name;
  useEffect(() => {
    if (displayName) onTitle?.(displayName);
  }, [displayName, onTitle]);

  if (fleets === null) return null;
  const summary = fleets.find((f) => f.name === name);
  if (!summary) return <>{missing}</>;
  return (
    <FleetDetailPane
      name={name}
      summary={summary}
      labels={labels}
      agentMap={agentMap}
      onRefresh={load}
      onDeleted={onDeleted}
      onOpenInWindow={onOpenInWindow}
      initialTab={initialTab}
    />
  );
}
```
- [x] `src/components/detail/window/DetailWindow.tsx`: delete the whole `function FleetBody(…) { … }` (lines 109–139) and the now-unused imports `useCallback`, `type AgentEntry`, `FleetSummary, LabelView`, `FleetDetailPane` (keep `useEffect`, `useState`, `invoke`, `getCurrentWindow`, `useAgents`, `AgentProvider`); add `import { FleetHost } from "../fleet/FleetHost";`. Replace `<FleetBody name={route.name} />` with:
  ```tsx
        <FleetHost
          name={route.name}
          missing={<Missing text={t("detailWindow.missingFleet")} />}
          onDeleted={() => void getCurrentWindow().close()}
        />
  ```
  (`t` is in scope in `DetailWindowInner`.) `grep -n 'FleetBody\|useCallback\|FleetSummary' src/components/detail/window/DetailWindow.tsx` → none.
- [x] `npm test`, `npm run build`, `npm run lint` (0 errors). Browser: `#/detail/fleet/<name>` renders as before (Overview tab first); `#/detail/fleet/nope` shows the missing state.
- [x] Commit: `refactor(hub): extract FleetHost from DetailWindow; FleetDetailPane.initialTab`

### Task 11.2 — peek model, tokens, `PeekPanel`, `DashboardApp` state and keys

**Interfaces.** Consumes `FleetHost` (11.1), `ChatPane` / `popOutChat` / `buildChatList` (3(a)), `openDetailWindow` / `isOpenInWindowShortcut` / `isEditingTarget` (2(b)). Produces `PeekTarget`, `peekTargetForChannel(channel, agentNames)`, `PeekPanel`, and `DashboardApp.openPeek(target)`; 11.3 consumes `openPeek`.

- [x] Create `src/components/peek/peekModel.test.ts`:

```ts
import { describe, expect, it } from "vitest";
import { peekTargetForChannel } from "./peekModel";

const agents = new Set(["aura", "scout"]);

describe("peekTargetForChannel", () => {
  it("maps a fleet channel to its fleet", () => {
    expect(peekTargetForChannel({ id: "fleet-night-ops" }, agents)).toEqual({ kind: "fleet", name: "night-ops" });
  });
  it("maps an agent's primary channel to its chat", () => {
    expect(peekTargetForChannel({ id: "aura" }, agents)).toEqual({ kind: "chat", agent: "aura" });
  });
  it("has no target for other channels", () => {
    expect(peekTargetForChannel({ id: "aura-2" }, agents)).toBeNull();
    expect(peekTargetForChannel({ id: "shared-x" }, agents)).toBeNull();
    expect(peekTargetForChannel({ id: "fleet-" }, agents)).toBeNull();
  });
});
```
- [x] `npm test -- src/components/peek/peekModel.test.ts` → fails (module missing).
- [x] Create `src/components/peek/peekModel.ts`:

```ts
/** What the Home peek can show (spec 3(b) §3). */
export type PeekTarget = { kind: "chat"; agent: string } | { kind: "fleet"; name: string };

export const FLEET_CHANNEL_PREFIX = "fleet-";

/** A fleet's channel → that fleet; an agent's primary channel (id == agent
 *  name) → that chat; anything else → null, and the caller keeps today's
 *  navigation. */
export function peekTargetForChannel(channel: { id: string }, agentNames: ReadonlySet<string>): PeekTarget | null {
  if (channel.id.startsWith(FLEET_CHANNEL_PREFIX)) {
    const name = channel.id.slice(FLEET_CHANNEL_PREFIX.length);
    return name ? { kind: "fleet", name } : null;
  }
  if (agentNames.has(channel.id)) return { kind: "chat", agent: channel.id };
  return null;
}
```
- [x] `npm test -- src/components/peek/peekModel.test.ts` → 3 passed.
- [x] Tokens. `src/styles/tokens/primitives.css`: on the line holding `--shadow-pop` (line 39) append ` --z-peek:900;` (the modal overlay is `z-index: 1000` in `modal.css`; the peek stays below it) and `--scrim-light:rgba(16,24,40,.32); --scrim-dark:rgba(0,0,0,.5);`. `src/styles/tokens/semantic.css`: add `--scrim:var(--scrim-light);` to the `:root` block (after the `--surface-list… --surface-detail…` line) and to the `:root[data-theme="light"]` block; add `--scrim:var(--scrim-dark);` to the `@media (prefers-color-scheme: dark)` block's `:root:not([data-theme="light"])` and to the `:root[data-theme="dark"]` block (each after its `--surface-list… --surface-detail…` line).
- [x] Create `src/styles/components/peek.css` and add `@import "./components/peek.css";` after the `chats.css` line in `src/styles/index.css`:

```css
/* Home side-peek (Phase 3(b) §5): scrim + right-anchored slide-over hosting ChatPane / FleetDetailPane. */
.peek__scrim { position: fixed; inset: 0; z-index: var(--z-peek); background: var(--scrim); }
.peek {
  position: fixed; top: 0; right: 0; bottom: 0; z-index: calc(var(--z-peek) + 1);
  width: min(560px, calc(100vw - var(--shell-sidebar-width) - 40px));
  display: flex; flex-direction: column; min-height: 0;
  background: var(--surface-detail); box-shadow: var(--shadow-pop);
  animation: peek-in var(--dur-base) var(--ease-out);
}
@keyframes peek-in { from { transform: translateX(100%); } to { transform: translateX(0); } }
@media (prefers-reduced-motion: reduce) { .peek { animation: none; } }
.peek__bar {
  display: flex; align-items: center; gap: var(--space-4); height: 44px; flex: none;
  padding: 0 var(--space-5) 0 var(--space-6); border-bottom: 1px solid var(--border-line);
}
.peek__title { flex: 1; min-width: 0; font-weight: var(--fw-semi); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.peek__actions { display: flex; align-items: center; gap: var(--space-3); }
.peek__close {
  background: none; border: 0; color: var(--text-tertiary); font-size: 18px; line-height: 1;
  padding: 2px 6px; border-radius: var(--radius-sm); cursor: pointer;
}
.peek__close:hover { background: var(--surface-hover); color: var(--text-primary); }
.peek__body { flex: 1; min-height: 0; display: flex; flex-direction: column; }
.peek__missing { margin: var(--space-8); color: var(--text-secondary); font-size: var(--text-sm); }
```
- [x] i18n. `en.ts` after `"home.nowRunning"`:
  ```ts
  "peek.go": "Go",
  "peek.viewConversation": "View conversation",
  ```
  `zh-TW.ts` after `"home.nowRunning"`:
  ```ts
  "peek.go": "前往",
  "peek.viewConversation": "查看對話",
  ```
- [x] Create `src/components/peek/PeekPanel.tsx`:

```tsx
import { useEffect, useRef, useState } from "react";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ChannelSummary } from "../../work/types";
import { useT } from "../../i18n";
import { useConversations } from "../../conversation/ConversationContext";
import { buildChatList } from "../chats/chatList";
import { ChatPane } from "../chats/ChatPane";
import { FleetHost } from "../detail/fleet/FleetHost";
import { isEditingTarget, isOpenInWindowShortcut } from "../detail/window/openInWindow";
import type { PeekTarget } from "./peekModel";

export interface PeekPanelProps {
  target: PeekTarget;
  agents: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  channels: ChannelSummary[];
  onClose: () => void;
  /** Leave Home for the page that owns the target, with it selected. */
  onGo: (t: PeekTarget) => void;
  /** The chat window or the fleet detail window; the caller closes the peek. */
  onOpenInWindow: (t: PeekTarget) => void;
  /** ChatPane's "Open agent": the Agents page with that agent selected. */
  onOpenAgent: (name: string) => void;
}

/** The right-side slide-over Home peeks into (spec 3(b) §5). Esc and the
 *  scrim close it; ⌘↩ opens the target in its window. */
export function PeekPanel({ target, agents, runtimeMap, channels, onClose, onGo, onOpenInWindow, onOpenAgent }: PeekPanelProps) {
  const { t } = useT();
  const closeRef = useRef<HTMLButtonElement>(null);
  const [fleetTitle, setFleetTitle] = useState<string | null>(null);

  // Focus the close button on open; give focus back on close.
  useEffect(() => {
    const previous = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    return () => previous?.focus();
  }, []);

  // Esc closes only the peek (capture phase, stopPropagation: the page's
  // global Esc must not also clear a selection); ⌘↩ opens in a window.
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === "Escape") {
        e.stopPropagation();
        e.preventDefault();
        onClose();
      } else if (isOpenInWindowShortcut(e) && !isEditingTarget(document.activeElement)) {
        e.stopPropagation();
        e.preventDefault();
        onOpenInWindow(target);
      }
    }
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  }, [target, onClose, onOpenInWindow]);

  const entry = target.kind === "chat" ? agents.find((a) => a.name === target.agent) : undefined;
  const title = target.kind === "chat" ? (entry?.display_name ?? target.agent) : (fleetTitle ?? target.name);

  return (
    <>
      <div className="peek__scrim" onClick={onClose} />
      <aside className="peek" role="dialog" aria-modal="true" aria-label={title}>
        <header className="peek__bar">
          <span className="peek__title">{title}</span>
          <div className="peek__actions">
            <button type="button" className="btn btn--secondary" onClick={() => onGo(target)}>
              {t("peek.go")}
            </button>
            <button type="button" className="btn btn--secondary" onClick={() => onOpenInWindow(target)}>
              {t("action.openInWindow")}
            </button>
            <button ref={closeRef} type="button" className="peek__close" onClick={onClose} aria-label={t("detail.close")}>
              ×
            </button>
          </div>
        </header>
        <div className="peek__body">
          {target.kind === "chat" ? (
            <ChatBody agent={target.agent} entry={entry} runtimeMap={runtimeMap} channels={channels} onOpenAgent={onOpenAgent} />
          ) : (
            <FleetHost
              name={target.name}
              initialTab="jobs"
              missing={<p className="peek__missing">{t("detailWindow.missingFleet")}</p>}
              onDeleted={onClose}
              onTitle={setFleetTitle}
            />
          )}
        </div>
      </aside>
    </>
  );
}

function ChatBody({ agent, entry, runtimeMap, channels, onOpenAgent }: {
  agent: string;
  entry: AgentEntry | undefined;
  runtimeMap: Map<string, AgentRuntimeStatus>;
  channels: ChannelSummary[];
  onOpenAgent: (name: string) => void;
}) {
  const { t } = useT();
  const { attention, openConversation, focusConversation, blurConversation } = useConversations();

  // Reading the conversation: the Chats page's attention rule (spec 3(a) §4).
  useEffect(() => {
    openConversation(agent);
    focusConversation(agent);
    return () => blurConversation();
  }, [agent, openConversation, focusConversation, blurConversation]);

  if (!entry) return <p className="peek__missing">{t("detailWindow.missingAgent")}</p>;
  const item = buildChatList([entry], attention, channels)[0];
  return <ChatPane item={item} runtime={runtimeMap.get(agent)} onOpenAgent={onOpenAgent} />;
}
```
- [x] `src/components/DashboardApp.tsx`:
  - Imports: `import { PeekPanel } from "./peek/PeekPanel";` and `import type { PeekTarget } from "./peek/peekModel";`.
  - State, after `openAgentFromChat`:
    ```tsx
    // Home's side-peek (spec 3(b) §3); rendered beside the Shell. Home gets
    // its opener in 11.3.
    const [peek, setPeek] = useState<PeekTarget | null>(null);
    const closePeek = useCallback(() => setPeek(null), []);
    const goFromPeek = useCallback((target: PeekTarget) => {
      setPeek(null);
      if (target.kind === "chat") openChatWith(target.agent);
      else {
        setFleetRequest(target.name);
        setPage("fleets");
      }
    }, [openChatWith]);
    const openWindowFromPeek = useCallback((target: PeekTarget) => {
      setPeek(null);
      if (target.kind === "chat") popOutChat(target.agent);
      else {
        const f = paletteFleets.find((x) => x.name === target.name);
        void openDetailWindow("fleet", target.name, f?.display_name ?? target.name);
      }
    }, [paletteFleets]);
    const openAgentFromPeek = useCallback((name: string) => {
      setPeek(null);
      openAgentFromChat(name);
    }, [openAgentFromChat]);
    ```
    (`openChatWith` and `openAgentFromChat` are declared above this point; `paletteFleets` too.)
  - Global Esc handler (lines 349–359): add `if (peek) return; // PeekPanel owns Esc while open` as the first line inside `onKey`, and `peek` to its dependency array.
  - After `</Shell>` and its closing `</div>` (line 596), before `<WizardModal`, render:
    ```tsx
          {peek && (
            <PeekPanel
              target={peek}
              agents={agents}
              runtimeMap={runtimeMap}
              channels={channels}
              onClose={closePeek}
              onGo={goFromPeek}
              onOpenInWindow={openWindowFromPeek}
              onOpenAgent={openAgentFromPeek}
            />
          )}
    ```
- [x] `npm test`, `npm run build`, `npm run lint` (0 errors). Nothing opens the panel yet; the browser check is in 11.3.
- [x] Commit: `feat(hub): PeekPanel — right-side slide-over for a chat or a fleet, with Esc / ⌘↩ / scrim`

### Task 11.3 — Home entry points, the Quick Look note

**Interfaces.** Consumes `peek` / `setPeek` and `PeekPanel` (11.2), `peekTargetForChannel`. Produces `DashboardApp.openPeek`, `HomePage.onPeek`, `NowRunning.onPeekAgent`, `NowRunning.onOpen(channel)`, `NeedsYou.onPeekAgent`.

- [x] `src/components/DashboardApp.tsx`: after `closePeek` add `const openPeek = useCallback((target: PeekTarget) => setPeek(target), []);` and pass `onPeek={openPeek}` to `<HomePage …/>`.
- [x] `src/components/home/HomePage.tsx`: add `import { peekTargetForChannel, type PeekTarget } from "../peek/peekModel";`; add the prop `onPeek: (target: PeekTarget) => void;` to `Props` (comment `/** Open the side-peek (spec 3(b)). */`); destructure it; replace `openChat` with:
  ```tsx
  const agentNames = new Set(agents.map((a) => a.name));
  // A channel row peeks its fleet or its agent's chat; other channels have no
  // viewer yet and keep going to the Chats page (spec 3(b) §4).
  function openChat(ch?: ChannelSummary) {
    const target = ch ? peekTargetForChannel(ch, agentNames) : null;
    if (target) onPeek(target);
    else onNavigate("chats");
  }
  const peekAgent = (name: string) => onPeek({ kind: "chat", agent: name });
  ```
  Pass `onPeekAgent={peekAgent}` to `<NeedsYou …/>` and `<NowRunning …/>`, and change `<NowRunning … onOpen={() => openChat()} />` to `onOpen={openChat}`.
- [x] `src/components/home/NowRunning.tsx`: change the prop to `onOpen: (channel: ChannelSummary) => void;` and add `onPeekAgent: (name: string) => void;`; destructure `onPeekAgent`; the chip becomes
  ```tsx
              <button
                key={s.name}
                type="button"
                className="home-run-agent"
                onClick={() => onPeekAgent(s.name)}
                aria-label={entry?.display_name ?? s.name}
              >
  ```
  (closing `</div>` → `</button>`); the row becomes `<button className="home-run-row" onClick={() => onOpen(ch)}>`.
- [x] `src/components/home/NeedsYou.tsx`: add `onPeekAgent: (name: string) => void;` to `Props` (comment `/** Peek the owning agent's conversation (spec 3(b) §4). */`), destructure it, pass `onPeekAgent={onPeekAgent}` to `HitlInboxCard` and `CompanionInboxCard`, add the prop to both components' prop types, and in each header render, after the title span:
  ```tsx
        {agent && (
          <button type="button" className="home-card__peek" onClick={() => onPeekAgent(agent)}>
            {t("peek.viewConversation")}
          </button>
        )}
  ```
  with `const agent = item.agent;` declared at the top of each card's body (no non-null assertion). `CompanionInboxCard` has no `useT` today: add `const { t } = useT();`. In the HITL header place it after the `home-card__tag` span; in the companion header after `inbox-situation`.
- [x] `src/styles/components/home.css`: on `.home-run-agent` add `font: inherit; color: inherit; cursor: pointer; text-align: left;` (it is a button now) and a hover `.home-run-agent:hover { background: var(--surface-hover); }`; append
  ```css
  .home-card__peek {
    margin-left: auto; background: none; border: 0; padding: 2px 6px; border-radius: var(--radius-sm);
    color: var(--color-brand); font: inherit; font-size: var(--text-xs); cursor: pointer; white-space: nowrap;
  }
  .home-card__peek:hover { background: var(--surface-hover); }
  ```
- [x] `docs/superpowers/specs/2026-09-06-mur-hub-master-detail-shell-design.md` line 26: change `Quick Look preview, side-peek from Home.` to `Quick Look preview (dropped in Phase 3(b): the detail pane already previews the selection), side-peek from Home (Phase 3(b)).`; line 228: change `Quick Look preview (space bar), side-peek from Home.` to `side-peek from Home (Phase 3(b)); Quick Look was dropped there.`
- [x] `npm test`, `npm run build`, `npm run lint` (0 errors).
- [x] Browser acceptance (stub: two agents, `list_runtime_statuses` with `aura` running, `channel_list` with `aura`'s primary channel, a `fleet-night-ops` channel with a non-empty `goal` and state `running`, and a `shared-x` channel with two agents and a goal; `hitl_pending_list` with one HITL for `aura`; `fleet_list` / `fleet_detail` / `fleet_jobs` / `fleet_labels_list` for `night-ops`; `channel_load → []`; `open_chat_window` / `open_detail_window → null`): on Home, the `AURA` chip opens the panel titled AURA with the `ChatPane` (model · channel · turns, compose box); the HITL card shows **View conversation** and opens the same; the `fleet-night-ops` row opens the fleet with the **Jobs** tab active and the fleet's display name as title; the `shared-x` row navigates to Chats (no panel); **Go** from a chat peek lands on Chats with AURA selected, from a fleet peek on Fleets with night-ops selected; **Open in window** calls `open_chat_window` / `open_detail_window {kind:"fleet"}` and closes; Esc and the scrim close; with the panel open, Esc does not clear `mur.agents.lastSelected`'s live selection on another page (open Agents, select, go Home, peek, Esc, back to Agents → still selected); a fired `chat-delta` for `aura` while its peek is open leaves the Chats row without the unread dot, and one fired after closing sets it; `peekTargetForChannel` for an unknown agent (edit the stub to drop `aura` from `list_agents` and reload) shows "This agent no longer exists." in the panel.
- [x] Commit: `feat(hub): Home side-peek — agent chips, HITL / companion cards, and fleet rows open the PeekPanel`

**Done (2026-09-06), deviations recorded:**
- 11.2: the peek state block had to sit after `paletteFleets` is declared (TS2454 otherwise); the plan placed it after `openAgentFromChat`.
- 11.3 (small addition): `onOpenInWindow(target, title)` — the panel passes the display name it already knows, so a fleet's ⌘↩ / Open in window titles the window "Night Ops" instead of `night-ops`; `DashboardApp` no longer looks the fleet up in `paletteFleets` for this.
- Browser note: the sidebar's Home button reads "首頁1" with a badge, so acceptance scripts match `startsWith`, not equality.
- `#/detail/fleet/<name>` and `#/detail/fleet/nope` verified after the `FleetHost` extraction (11.1's browser check, run with 11.3's dev server).

**Manual acceptance PR 11 (real build):** the slide-in motion and `prefers-reduced-motion`; the scrim colour in light and dark; a real streamed reply inside a chat peek; a fleet peek's Run ▾ menu (the popover must not be clipped by the panel).

## Spec coverage

| Spec § | Task |
|---|---|
| 3 peek targets, `DashboardApp` state | 11.2 |
| 4 Home entry points | 11.3 |
| 5 `PeekPanel` (markup, keys, focus, attention, CSS, tokens) | 11.2 |
| 6 `FleetHost`, `initialTab` | 11.1 |
| 7 errors / missing | 11.2 (`peek__missing`), 11.3 (fallback navigation) |
| 8 keyboard | 11.2 (capture-phase Esc / ⌘↩, `DashboardApp` Esc guard) |
| 9 tests | 11.2 (`peekModel`), browser lists in 11.1 / 11.3 |
| 2 Quick Look dropped | 11.3 (shell spec note) |
