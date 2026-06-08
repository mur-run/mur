# Hub Multi-Agent Conversation Rail Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a vertical conversation rail to the MUR Hub so the user can hold several live agent conversations at once — open multiple agents, switch instantly, and never miss background activity (streaming, HITL, completion).

**Architecture:** Frontend-only (`mur-hub-gui/ui`). A pure `conversationReducer` + `ConversationContext` track *open / active / attention* per agent; the actual message buffers stay inside **per-conversation `ChatTab` instances kept mounted** (active one visible, others hidden) so nothing is lost on switch. A `ConversationRail` shows status + attention badges; background HITL also fires the existing notification path. No `mur-core`/runtime changes.

**Tech Stack:** React 18 + TypeScript, Vite, **Vitest (node env — no DOM/testing-library in this project)**, Tauri 2 events (`listen`).

---

## Key decisions (read first)

1. **Keep `ChatTab` instances mounted, don't lift buffers into the store.** `ChatTab` already owns its message buffer and multi-turn `taskId`, and it *wipes them when its `agentName` prop changes* (`ChatTab.tsx:54-61`). So rendering one `ChatTab` and swapping `agentName` would lose history on every switch. Instead we mount **one `ChatTab` per open conversation** and toggle visibility with CSS — each instance already filters `chat-delta` by name and retains its own state. `ChatTab` itself is **unchanged**.

2. **Store holds only `open / active / attention`** — not messages. This refines the spec's "store buffers all conversations": the *outcome* (instant switch, nothing lost) is delivered by mounted instances; the store is small and pure.

3. **Test posture = pure logic.** This project has Vitest configured but **no jsdom/testing-library** (only `i18n/t.test.ts`). So the TDD target is the **pure `conversationReducer`**; components are verified with `tsc -b` + `npm run build` + a manual smoke checklist. We do **not** add a component-test harness (scope creep, against project posture).

4. **Attention = two booleans (`unread`, `hitl`).** A precise pending-HITL count needs a resolution event we don't have; for v1 both clear on `focus`. Count is a v2 refinement.

## File Map

| File | Action | Responsibility |
|---|---|---|
| `mur-hub-gui/ui/src/conversation/reducer.ts` | Create | Pure `conversationReducer` + `attentionLevel` + types |
| `mur-hub-gui/ui/src/conversation/reducer.test.ts` | Create | Vitest unit tests (the TDD target) |
| `mur-hub-gui/ui/src/conversation/ConversationContext.tsx` | Create | Provider: wire Tauri listeners → dispatch; expose actions (mirrors `AgentContext`) |
| `mur-hub-gui/ui/src/components/ConversationRail.tsx` | Create | Vertical rail: item per open conversation, status dot + attention badge + close |
| `mur-hub-gui/ui/src/components/ConversationsView.tsx` | Create | Rail (left) + all open `ChatTab`s mounted (active visible) |
| `mur-hub-gui/ui/src/components/DetailPanel.tsx` | Modify | Remove the chat tab (config-only); default tab → persona |
| `mur-hub-gui/ui/src/components/DashboardApp.tsx` | Modify | Mount `ConversationsView`; add "Chat" affordance → `open(agent)` |
| `mur-hub-gui/ui/src/App.tsx` | Modify | Wrap `<ConversationProvider>` inside `<AgentProvider>` |
| `mur-hub-gui/ui/src/conversation/notify.ts` | Create | Background-HITL → existing notification (reconcile task) |
| `mur-hub-gui/ui/src/styles*` (existing CSS) | Modify | `.conv-rail`, `.conv-item`, badge classes |

---

### Task 1: Pure `conversationReducer` + types (TDD)

**Files:**
- Create: `mur-hub-gui/ui/src/conversation/reducer.ts`
- Create: `mur-hub-gui/ui/src/conversation/reducer.test.ts`

- [ ] **Step 1: Write the failing tests**

Create `mur-hub-gui/ui/src/conversation/reducer.test.ts`:

```ts
import { describe, it, expect } from "vitest";
import {
  conversationReducer,
  attentionLevel,
  initialConversationState,
  type ConversationState,
} from "./reducer";

const base = (): ConversationState => initialConversationState();

describe("conversationReducer", () => {
  it("open adds the agent, makes it active, and clears its attention", () => {
    const s = conversationReducer(base(), { type: "open", agent: "a" });
    expect(s.open).toEqual(["a"]);
    expect(s.active).toBe("a");
    expect(s.attention["a"]).toEqual({ unread: false, hitl: false });
  });

  it("open is idempotent — re-opening just focuses, never duplicates", () => {
    let s = conversationReducer(base(), { type: "open", agent: "a" });
    s = conversationReducer(s, { type: "open", agent: "b" });
    s = conversationReducer(s, { type: "open", agent: "a" });
    expect(s.open).toEqual(["a", "b"]);
    expect(s.active).toBe("a");
  });

  it("delta for a non-active open agent sets unread", () => {
    let s = conversationReducer(base(), { type: "open", agent: "a" });
    s = conversationReducer(s, { type: "open", agent: "b" }); // b active
    s = conversationReducer(s, { type: "delta", agent: "a" });
    expect(s.attention["a"].unread).toBe(true);
  });

  it("delta for the active agent does NOT set unread", () => {
    let s = conversationReducer(base(), { type: "open", agent: "a" }); // a active
    s = conversationReducer(s, { type: "delta", agent: "a" });
    expect(s.attention["a"].unread).toBe(false);
  });

  it("delta for an agent that is not open is ignored", () => {
    const s = conversationReducer(base(), { type: "delta", agent: "ghost" });
    expect(s.attention["ghost"]).toBeUndefined();
  });

  it("hitl_open for a non-active open agent sets hitl", () => {
    let s = conversationReducer(base(), { type: "open", agent: "a" });
    s = conversationReducer(s, { type: "open", agent: "b" });
    s = conversationReducer(s, { type: "hitl_open", agent: "a" });
    expect(s.attention["a"].hitl).toBe(true);
  });

  it("focus clears unread and hitl for that agent", () => {
    let s = conversationReducer(base(), { type: "open", agent: "a" });
    s = conversationReducer(s, { type: "open", agent: "b" });
    s = conversationReducer(s, { type: "delta", agent: "a" });
    s = conversationReducer(s, { type: "hitl_open", agent: "a" });
    s = conversationReducer(s, { type: "focus", agent: "a" });
    expect(s.active).toBe("a");
    expect(s.attention["a"]).toEqual({ unread: false, hitl: false });
  });

  it("close removes the agent and reassigns active to another open one", () => {
    let s = conversationReducer(base(), { type: "open", agent: "a" });
    s = conversationReducer(s, { type: "open", agent: "b" }); // b active
    s = conversationReducer(s, { type: "close", agent: "b" });
    expect(s.open).toEqual(["a"]);
    expect(s.active).toBe("a");
    expect(s.attention["b"]).toBeUndefined();
  });

  it("close the last conversation sets active to null", () => {
    let s = conversationReducer(base(), { type: "open", agent: "a" });
    s = conversationReducer(s, { type: "close", agent: "a" });
    expect(s.open).toEqual([]);
    expect(s.active).toBeNull();
  });
});

describe("attentionLevel", () => {
  it("hitl outranks unread", () => {
    expect(attentionLevel({ unread: true, hitl: true })).toBe("hitl");
    expect(attentionLevel({ unread: true, hitl: false })).toBe("unread");
    expect(attentionLevel({ unread: false, hitl: false })).toBe("none");
  });
});
```

- [ ] **Step 2: Run the tests, verify they fail**

Run: `cd mur-hub-gui/ui && npx vitest run src/conversation/reducer.test.ts 2>&1 | tail -20`
Expected: fail — cannot resolve `./reducer`.

- [ ] **Step 3: Implement `reducer.ts`**

Create `mur-hub-gui/ui/src/conversation/reducer.ts`:

```ts
export type AttentionLevel = "none" | "unread" | "hitl";

export interface ConversationAttention {
  unread: boolean;
  hitl: boolean;
}

export interface ConversationState {
  /** Agent names with an open conversation, in open order. */
  open: string[];
  /** Currently focused agent, or null when none open. */
  active: string | null;
  /** Per-agent attention flags. */
  attention: Record<string, ConversationAttention>;
}

export type ConversationAction =
  | { type: "open"; agent: string }
  | { type: "close"; agent: string }
  | { type: "focus"; agent: string }
  | { type: "delta"; agent: string }
  | { type: "hitl_open"; agent: string };

export function initialConversationState(): ConversationState {
  return { open: [], active: null, attention: {} };
}

const CLEAR: ConversationAttention = { unread: false, hitl: false };

export function attentionLevel(a: ConversationAttention | undefined): AttentionLevel {
  if (!a) return "none";
  if (a.hitl) return "hitl";
  if (a.unread) return "unread";
  return "none";
}

export function conversationReducer(
  state: ConversationState,
  action: ConversationAction,
): ConversationState {
  switch (action.type) {
    case "open": {
      const isOpen = state.open.includes(action.agent);
      return {
        open: isOpen ? state.open : [...state.open, action.agent],
        active: action.agent,
        attention: { ...state.attention, [action.agent]: { ...CLEAR } },
      };
    }
    case "close": {
      const open = state.open.filter((a) => a !== action.agent);
      const attention = { ...state.attention };
      delete attention[action.agent];
      const active =
        state.active === action.agent ? (open[open.length - 1] ?? null) : state.active;
      return { open, active, attention };
    }
    case "focus": {
      if (!state.open.includes(action.agent)) return state;
      return {
        ...state,
        active: action.agent,
        attention: { ...state.attention, [action.agent]: { ...CLEAR } },
      };
    }
    case "delta": {
      if (!state.open.includes(action.agent) || state.active === action.agent) return state;
      const prev = state.attention[action.agent] ?? { ...CLEAR };
      return {
        ...state,
        attention: { ...state.attention, [action.agent]: { ...prev, unread: true } },
      };
    }
    case "hitl_open": {
      if (!state.open.includes(action.agent) || state.active === action.agent) return state;
      const prev = state.attention[action.agent] ?? { ...CLEAR };
      return {
        ...state,
        attention: { ...state.attention, [action.agent]: { ...prev, hitl: true } },
      };
    }
  }
}
```

- [ ] **Step 4: Run the tests, verify they pass**

Run: `cd mur-hub-gui/ui && npx vitest run src/conversation/reducer.test.ts 2>&1 | tail -20`
Expected: all tests pass.

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/conversation/reducer.ts mur-hub-gui/ui/src/conversation/reducer.test.ts
git commit -m "feat(hub-ui): pure conversationReducer + attention model (open/active/attention)"
```

---

### Task 2: `ConversationContext` provider

**Files:**
- Create: `mur-hub-gui/ui/src/conversation/ConversationContext.tsx`

Context: mirror `AgentContext.tsx`'s reducer + `listen` pattern. Listeners only *dispatch the raw event*; the reducer reads fresh `state.active`/`open` to decide, so stale listener closures are not a problem.

- [ ] **Step 1: Create the provider**

Create `mur-hub-gui/ui/src/conversation/ConversationContext.tsx`:

```tsx
import React, { createContext, useContext, useEffect, useReducer } from "react";
import { listen } from "@tauri-apps/api/event";
import {
  conversationReducer,
  initialConversationState,
  type ConversationState,
} from "./reducer";

interface ConversationContextValue extends ConversationState {
  open: string[];
  openConversation: (agent: string) => void;
  closeConversation: (agent: string) => void;
  focusConversation: (agent: string) => void;
}

const Ctx = createContext<ConversationContextValue>({
  open: [],
  active: null,
  attention: {},
  openConversation: () => {},
  closeConversation: () => {},
  focusConversation: () => {},
});

export function ConversationProvider({ children }: { children: React.ReactNode }) {
  const [state, dispatch] = useReducer(conversationReducer, undefined, initialConversationState);

  useEffect(() => {
    const unDelta = listen<{ agent: string }>("chat-delta", (e) =>
      dispatch({ type: "delta", agent: e.payload.agent }),
    );
    const unHitl = listen<{ agent: string }>("hitl-approval-needed", (e) =>
      dispatch({ type: "hitl_open", agent: e.payload.agent }),
    );
    return () => {
      unDelta.then((f) => f());
      unHitl.then((f) => f());
    };
  }, []);

  return (
    <Ctx.Provider
      value={{
        open: state.open,
        active: state.active,
        attention: state.attention,
        openConversation: (agent) => dispatch({ type: "open", agent }),
        closeConversation: (agent) => dispatch({ type: "close", agent }),
        focusConversation: (agent) => dispatch({ type: "focus", agent }),
      }}
    >
      {children}
    </Ctx.Provider>
  );
}

export function useConversations() {
  return useContext(Ctx);
}
```

- [ ] **Step 2: Typecheck**

Run: `cd mur-hub-gui/ui && npx tsc -b 2>&1 | tail -20`
Expected: no errors. (The provider isn't mounted yet; this just confirms it compiles.)

- [ ] **Step 3: Commit**

```bash
git add mur-hub-gui/ui/src/conversation/ConversationContext.tsx
git commit -m "feat(hub-ui): ConversationProvider wiring chat-delta/hitl events to the reducer"
```

---

### Task 3: `ConversationRail` component

**Files:**
- Create: `mur-hub-gui/ui/src/components/ConversationRail.tsx`

- [ ] **Step 1: Create the component**

`runtimeStatuses` from `useAgents()` provides per-agent status; map it to a dot. Attention from the store.

```tsx
import { useAgents } from "../context/AgentContext";
import { useConversations } from "../conversation/ConversationContext";
import { attentionLevel } from "../conversation/reducer";

function statusOf(name: string, statuses: { name: string; state?: string }[]): string {
  const s = statuses.find((x) => x.name === name);
  // RECONCILE: confirm the field carrying running/stopped in AgentRuntimeStatus (types.ts).
  return s?.state ?? "idle";
}

export function ConversationRail() {
  const { runtimeStatuses, agents } = useAgents();
  const { open, active, attention, focusConversation, closeConversation } = useConversations();

  if (open.length === 0) return null;

  return (
    <div className="conv-rail">
      {open.map((name) => {
        const display = agents.find((a) => a.name === name)?.display_name ?? name;
        const level = attentionLevel(attention[name]);
        const status = statusOf(name, runtimeStatuses);
        return (
          <button
            key={name}
            className={`conv-item${active === name ? " conv-item--active" : ""}`}
            onClick={() => focusConversation(name)}
            title={display}
          >
            <span className={`conv-status conv-status--${status}`} />
            <span className="conv-name">{display}</span>
            {level !== "none" && <span className={`conv-badge conv-badge--${level}`} />}
            <span
              className="conv-close"
              role="button"
              aria-label="Close conversation"
              onClick={(e) => {
                e.stopPropagation();
                closeConversation(name);
              }}
            >
              ×
            </span>
          </button>
        );
      })}
    </div>
  );
}
```

> RECONCILE: open `mur-hub-gui/ui/src/types.ts` and confirm the exact `AgentRuntimeStatus` field name carrying the running/idle/error state; adjust `statusOf` + the `conv-status--*` class names to match. Confirm `AgentEntry.display_name` exists (it does, per `DetailPanel`/`DashboardApp` usage).

- [ ] **Step 2: Add minimal CSS**

Append to the existing dashboard stylesheet (locate it: `grep -rl "conv-rail\|detail-panel-tabs\|\.chat__log" mur-hub-gui/ui/src` — use the file that defines `.chat__log`). Add:

```css
.conv-rail { display: flex; flex-direction: column; gap: 6px; width: 168px;
  padding: 8px; border-right: 1px solid var(--border, #8883); overflow-y: auto; }
.conv-item { display: flex; align-items: center; gap: 8px; padding: 6px 8px;
  border-radius: 8px; background: transparent; border: none; cursor: pointer;
  text-align: left; color: inherit; }
.conv-item--active { background: var(--accent-soft, #7c5cff22); }
.conv-name { flex: 1; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.conv-status { width: 8px; height: 8px; border-radius: 50%; background: #888; flex: none; }
.conv-status--running { background: #3ad17a; }
.conv-status--error { background: #e5484d; }
.conv-badge { width: 8px; height: 8px; border-radius: 50%; flex: none; }
.conv-badge--unread { background: #5b8cff; }
.conv-badge--hitl { background: #f5a623; }
.conv-close { opacity: .5; padding: 0 2px; }
.conv-close:hover { opacity: 1; }
```

- [ ] **Step 3: Typecheck + build**

Run: `cd mur-hub-gui/ui && npx tsc -b && npm run build 2>&1 | tail -15`
Expected: builds clean.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/ConversationRail.tsx mur-hub-gui/ui/src/
git commit -m "feat(hub-ui): ConversationRail (status dot + attention badge + close)"
```

---

### Task 4: `ConversationsView` (mounted-per-conversation ChatTabs)

**Files:**
- Create: `mur-hub-gui/ui/src/components/ConversationsView.tsx`

- [ ] **Step 1: Create the view**

All open `ChatTab`s stay mounted; only the active one is visible (CSS), so background buffers persist.

```tsx
import { ChatTab } from "./ChatTab";
import { ConversationRail } from "./ConversationRail";
import { useAgents } from "../context/AgentContext";
import { useConversations } from "../conversation/ConversationContext";

export function ConversationsView() {
  const { agents } = useAgents();
  const { open, active } = useConversations();

  if (open.length === 0) return null;

  return (
    <div className="conv-surface">
      <ConversationRail />
      <div className="conv-panels">
        {open.map((name) => {
          const display = agents.find((a) => a.name === name)?.display_name ?? name;
          return (
            <div
              key={name}
              className="conv-panel"
              style={{ display: active === name ? "flex" : "none" }}
            >
              <ChatTab agentName={name} displayName={display} />
            </div>
          );
        })}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: CSS for the surface**

Append to the same stylesheet:

```css
.conv-surface { display: flex; height: 100%; min-height: 0; }
.conv-panels { flex: 1; min-width: 0; display: flex; }
.conv-panel { flex: 1; min-width: 0; flex-direction: column; }
```

- [ ] **Step 3: Typecheck + build**

Run: `cd mur-hub-gui/ui && npx tsc -b && npm run build 2>&1 | tail -15`
Expected: builds clean.

- [ ] **Step 4: Commit**

```bash
git add mur-hub-gui/ui/src/components/ConversationsView.tsx mur-hub-gui/ui/src/
git commit -m "feat(hub-ui): ConversationsView keeps a ChatTab mounted per open conversation"
```

---

### Task 5: Wire in — provider, mount, "Chat" affordance, config-only DetailPanel

**Files:**
- Modify: `mur-hub-gui/ui/src/App.tsx`
- Modify: `mur-hub-gui/ui/src/components/DashboardApp.tsx`
- Modify: `mur-hub-gui/ui/src/components/DetailPanel.tsx`

- [ ] **Step 1: Wrap the provider in `App.tsx`**

In `mur-hub-gui/ui/src/App.tsx`, import and nest the provider inside `AgentProvider` (so it can read agents):

```tsx
import { ConversationProvider } from "./conversation/ConversationContext";
// ...
return (
  <AgentProvider>
    <ConversationProvider>
      {route === "popover" ? <PopoverApp /> : <DashboardApp />}
    </ConversationProvider>
  </AgentProvider>
);
```

- [ ] **Step 2: Mount `ConversationsView` + add a "Chat" affordance in `DashboardApp.tsx`**

Import at top:
```tsx
import { ConversationsView } from "./ConversationsView";
import { useConversations } from "../conversation/ConversationContext";
```

Mount the surface next to the existing `DetailPanel` block (after line ~642, the `{selectedAgent && (<DetailPanel ... />)}`):
```tsx
<ConversationsView />
```

Add a "Chat" button to the agent card. In `GridCard`/`ListRow`, pull the action:
```tsx
const { openConversation } = useConversations();
```
and add a button (next to the existing Run/Stop/Share buttons, ~line 174-186):
```tsx
<button
  onClick={(e) => { e.stopPropagation(); openConversation(agent.name); }}
  title={t("dashboard.chatTooltip")}
>
  {t("dashboard.chat")}
</button>
```

> RECONCILE: add `dashboard.chat` + `dashboard.chatTooltip` keys to `mur-hub-gui/ui/src/i18n/en.ts` and `zh-TW.ts` (e.g. en: `"Chat"` / `"Open a conversation with this agent"`; zh-TW: `"對話"` / `"與此代理開啟對話"`). Match the existing key structure in those files.

- [ ] **Step 3: Make `DetailPanel` config-only (remove chat tab)**

In `mur-hub-gui/ui/src/components/DetailPanel.tsx`:
- Remove `"chat"` from the `DetailTab` union type (find `type DetailTab =`).
- Change the default at line 56: `useState<DetailTab>("persona")` (was `"chat"`).
- Change the reset at line 60: `setActiveTab("persona")` (was `"chat"`).
- Remove the chat tab entry from the tab array rendered at line ~174 (the array the `.map` iterates — drop `"chat"`).
- Remove the render block at lines 188-190:
  ```tsx
  {activeTab === "chat" && (
    <ChatTab agentName={agentName} displayName={detail.display_name} />
  )}
  ```
- Remove the now-unused `import { ChatTab } from "./ChatTab";` (line 17).
- Update the file's top doc comment (line 2) from "7 tabs" / chat mention to reflect config-only tabs.

> RECONCILE: confirm the exact tab-array variable feeding the `.map` at line ~174 and remove only the chat entry; leave persona/style/behavior/skills/mcp/permissions/inbox/mobile/memory intact.

- [ ] **Step 4: Typecheck + build**

Run: `cd mur-hub-gui/ui && npx tsc -b && npm run build 2>&1 | tail -20`
Expected: builds clean (no unused-import errors for `ChatTab` in DetailPanel).

- [ ] **Step 5: Commit**

```bash
git add mur-hub-gui/ui/src/App.tsx mur-hub-gui/ui/src/components/DashboardApp.tsx \
        mur-hub-gui/ui/src/components/DetailPanel.tsx mur-hub-gui/ui/src/i18n/
git commit -m "feat(hub-ui): mount conversation rail; Chat affordance; DetailPanel config-only"
```

---

### Task 6: Background-HITL notification (reconcile with existing path)

**Files:**
- Create: `mur-hub-gui/ui/src/conversation/notify.ts`
- Modify: `mur-hub-gui/ui/src/conversation/ConversationContext.tsx`

Context: the spec wants a background HITL (on a non-active agent) to also fire the existing Phase-2 notification, so it isn't missed. First find how notifications are emitted today.

- [ ] **Step 1: Locate the existing notification path**

```bash
cd /Volumes/Firecuda4tb/Projects/mur
grep -rniE "notification|notify|isPermissionGranted|sendNotification|plugin-notification" mur-hub-gui/ui/src mur-hub-gui/src-tauri/src | head -20
```
Record whether notifications are emitted (a) already on the backend when HITL fires (then this task is **badge-only — no new code**, mark done), or (b) only via a frontend helper you must call.

- [ ] **Step 2: If frontend-emitted, add a thin helper**

Only if Step 1 shows the UI must emit. Create `mur-hub-gui/ui/src/conversation/notify.ts`:

```ts
// Thin wrapper over the project's existing notification mechanism.
// RECONCILE: replace the body with the actual call discovered in Step 1
// (e.g. invoke("notify", {...}) or @tauri-apps/plugin-notification).
export async function notifyHitl(agentDisplay: string): Promise<void> {
  // placeholder intentionally omitted — wire to the real path from Step 1
}
```

In `ConversationContext.tsx`, fire it from the hitl listener only when the agent is **not** active. Because the listener closure can't see fresh `active`, gate inside an effect that watches attention transitions instead:

```tsx
// after the reducer/useReducer:
useEffect(() => {
  // when any non-active agent gains hitl attention, notify once.
  // (Implementation detail: track previously-notified set in a ref to avoid repeats.)
}, [state.attention]);
```

> RECONCILE: if Step 1 found the backend already notifies on HITL, **delete `notify.ts` and skip the effect** — the rail badge is the only addition. Keep this task's scope to "don't double-notify."

- [ ] **Step 3: Build + commit**

Run: `cd mur-hub-gui/ui && npx tsc -b && npm run build 2>&1 | tail -15`
```bash
git add mur-hub-gui/ui/src/conversation/
git commit -m "feat(hub-ui): background-HITL notification reconciled with existing path"
```

---

### Task 7: Full verification + manual smoke

**Files:** none.

- [ ] **Step 1: Unit tests + typecheck + build + lint**

```bash
cd mur-hub-gui/ui
npx vitest run 2>&1 | tail -15      # reducer tests green
npx tsc -b 2>&1 | tail -10          # no type errors
npm run build 2>&1 | tail -10       # builds
npm run lint 2>&1 | tail -15        # no new lint errors
```
Expected: all green.

- [ ] **Step 2: Manual smoke (run the Hub)**

Build/run the Hub (per `CLAUDE.md`: the Hub is workspace-excluded; build via its own manifest, and copy `target/release/mur-agent-runtime` next to the Hub binary). Then verify:
- Click **Chat** on two different agents → both appear in the left rail; the second becomes active.
- Send a message to agent A, switch to B, switch back to A → **A's history is intact** (mounted-instance check).
- While viewing A, have B stream/raise a HITL → B's rail item shows an **attention badge**; clicking B clears it.
- Open the config **DetailPanel** for an agent → it has **no chat tab**; config tabs work.
- Close a conversation → it leaves the rail; active reassigns.

- [ ] **Step 3: Final commit (if lint/fmt fixes were needed)**

```bash
git add -A
git commit -m "style: lint fixes for conversation rail"
```

---

## Self-Review (coverage map)

| Spec item | Task |
|---|---|
| Vertical rail of open conversations | 3 |
| Buffered multi-conversation (nothing lost on switch) — via mounted ChatTabs | 1 (state), 4 (mount) |
| Attention badges (unread / HITL) | 1 (model), 3 (render) |
| Background HITL → existing notification | 6 |
| Separation: chat out of DetailPanel (config-only); under 800 lines | 5 |
| Open/focus from grid; provider wiring | 2, 5 |
| Status dot from runtime status | 3 |
| Error states (agent stop / removed) | manual smoke 7; status dot 3 (RECONCILE field) |
| No mur-core/runtime changes | (entire plan is frontend) |

**Known v1 limitations (from spec):** attention is boolean not a precise count (v2); split/concurrent view deferred (v2); no inter-agent routing (Commander). **RECONCILE points:** `AgentRuntimeStatus` status field name (Task 3); exact DetailPanel tab-array variable (Task 5); existing notification path (Task 6); i18n keys (Task 5).
