# MUR Hub 2.0 — Phase 3(a) Chats on the master–detail shell — implementation plan

> **Execute with `mur-executing-plans`.** Spec: `docs/superpowers/specs/2026-09-06-mur-hub-chats-master-detail-design.md` (§ references below point there). One PR (**PR 10**), four tasks, each commit builds.

## Goal

The Chats page renders on the shared master–detail shell (SourceList + a headed conversation pane) and the inspector column disappears from the app.

## Architecture

`ChatsPage` becomes the same `master-detail` grid as Agents: `SourceList` rows built by a pure `chatRows` from `buildChatList` (now joined with the primary channel from `channel_list`), and a `ChatPane` = `DetailHeader` (extracted from `DetailPage`) + `ChatTab` with `TaskPill`. The header shows the three facts the inspector showed, from data already on the page, so `ChatInspector`, `Inspector`, and the Shell's inspector track are deleted.

## Tech stack

React 18 + TypeScript 5.5 + Vite 5, plain CSS on the two-tier tokens, Vitest 4 without jsdom, the lightweight i18n (`en.ts` defines keys, `zh-TW.ts` is typed `Table`). No Rust.

## Global Constraints

Copied from the design and `CLAUDE.md`. Every task includes all of them.

1. Brand name is uppercase **MUR** in every user-visible string.
2. Single source file ≤ 800 lines.
3. Every new user-visible string lands in both `src/i18n/en.ts` and `src/i18n/zh-TW.ts` in the same commit (`tsc` enforces the table).
4. Components reference only semantic tokens; no raw hex in component CSS or TSX.
5. No hardcoded numbers or storage keys in TSX: named constants.
6. Never pair `Foo.tsx` with `foo.ts` in one directory (APFS is case-insensitive; Vite and `tsc` resolve the wrong file).
7. Tests never touch the DOM: pure functions, or `renderToStaticMarkup` for markup (`useT` needs a provider, so markup tests cover only components without it — `StatusPill` calls `useT`, so `DetailHeader` is tested without `status`).
8. Every commit is gated on the real exit code: `set -o pipefail; npm test 2>&1 | grep …` — never on grep's.
9. No new data path: the page reads `channel_list` through the `useChannels()` result `DashboardApp` already holds, and runtime state through the `runtimeMap` it already builds.
10. Every PR leaves the app usable: `npm run build`, `npm test`, `npm run lint` green and the manual acceptance list passes.

## Working agreement

- Paths are relative to `mur-hub-gui/ui/`.
- Line numbers cite `main` at `d41f0478` (2026-09-06); re-check with `grep -n` before cutting.
- Commands from `mur-hub-gui/ui/`: `npm test -- <path>`, `npm test`, `npm run build`, `npm run lint`. `npm run lint` reports 6 pre-existing warnings in files this plan does not touch; 0 errors is the bar.
- Browser acceptance: `npm run dev -- --port 5174 --strictPort`, inject the Tauri stub the Phase 1 plan describes (`window.__TAURI_INTERNALS__` with `metadata.currentWindow`, an `invoke` stub, `plugin:event|listen` storing the handler ids per event name, `plugin:dialog|message → "Ok"`), keep the stub source in `sessionStorage` and `eval` it after each reload, click the error boundary's **Try again** programmatically (find the button by text, then `.click()`).
- Commit after every task with the message given.

## File structure

| File | Responsibility |
|---|---|
| `src/components/shell/DetailHeader.tsx` (+ `DetailHeader.test.tsx`) (new) | the identity strip (avatar, title, status, meta, actions) |
| `src/components/shell/DetailPage.tsx` (modify) | renders `DetailHeader` instead of its inline header |
| `src/components/chats/chatList.ts` (+ `.test.ts`) (modify) | `buildChatList` joins the primary channel; `chatRows`, `chatFacets` |
| `src/components/shell/sourceListModel.ts` (modify) | `SourceRowData.unread?` |
| `src/components/shell/SourceList.tsx` (+ `.test.tsx`) (modify) | `createLabel?`, `unreadLabel?`, the unread dot |
| `src/styles/components/source-list.css` (modify) | `.source-row__unread` |
| `src/components/chats/ChatPane.tsx` (new) | header + `ChatTab` + `TaskPill`; `popOutChat` |
| `src/components/chats/ChatsPage.tsx` (rewrite) | the master–detail page |
| `src/styles/components/chats.css` (new) + `src/styles/index.css` (modify) | `.chat-pane*`, `.chats-empty` |
| `src/components/DashboardApp.tsx` (modify) | `ChatsPage` props, `selectedChat`, `clearChatInitial`, ⌘↩ on Chats; inspector wiring removed |
| `src/components/shell/Shell.tsx` (+ `shell.test.ts`) (modify) | inspector prop / toggle removed |
| `src/components/inspector/ChatInspector.tsx`, `src/components/shell/Inspector.tsx` (delete) | — |
| `src/styles/components/shell.css`, `src/styles/tokens/primitives.css`, `src/styles/components/work.css`, `src/styles/components/dashboard.css`, `src/styles/components/detail-panel.css` (modify) | dead rules removed |
| `src/i18n/en.ts`, `src/i18n/zh-TW.ts` (modify) | new strings; `chatInspector.*` pruned |

---

### Task 10.1 — `DetailHeader` extraction

**Interfaces.** Produces `DetailHeader({ avatar, title, status?, meta?, actions? })`; 10.3 consumes it. `DetailPage`'s props and markup are unchanged.

- [ ] Create `src/components/shell/DetailHeader.test.tsx`:

```tsx
import { describe, expect, it } from "vitest";
import { renderToStaticMarkup } from "react-dom/server";
import { DetailHeader } from "./DetailHeader";

describe("DetailHeader markup", () => {
  it("renders avatar, title, meta and actions", () => {
    const html = renderToStaticMarkup(
      <DetailHeader avatar="A" title="AURA" meta={<span>m1</span>} actions={<button type="button">act</button>} />,
    );
    expect(html).toContain("detail-page__head");
    expect(html).toContain("detail-page__title");
    expect(html).toContain("AURA");
    expect(html).toContain("detail-page__meta");
    expect(html).toContain("m1");
    expect(html).toContain("detail-page__actions");
    expect(html).toContain("act");
  });
  it("omits the meta and actions wrappers when absent", () => {
    const html = renderToStaticMarkup(<DetailHeader avatar="A" title="AURA" />);
    expect(html).not.toContain("detail-page__meta");
    expect(html).not.toContain("detail-page__actions");
    expect(html).not.toContain("status-pill");
  });
});
```
- [ ] `npm test -- src/components/shell/DetailHeader.test.tsx` → fails (module missing).
- [ ] Create `src/components/shell/DetailHeader.tsx`:

```tsx
import type { ReactNode } from "react";
import { StatusPill, type StatusKind } from "./Status";

export interface DetailHeaderProps {
  avatar: ReactNode;
  title: string;
  /** Omit for objects without a runtime state (Library items). */
  status?: StatusKind;
  meta?: ReactNode;
  actions?: ReactNode;
}

/** The identity strip every detail shares (spec 3(a) §5): DetailPage renders
 *  it above its tabs, ChatPane above the conversation. */
export function DetailHeader(p: DetailHeaderProps) {
  return (
    <header className="detail-page__head">
      <span className="detail-page__avatar">{p.avatar}</span>
      <div className="detail-page__ident">
        <h1 className="detail-page__title">
          {p.title} {p.status && <StatusPill kind={p.status} />}
        </h1>
        {p.meta && <div className="detail-page__meta">{p.meta}</div>}
      </div>
      {p.actions && <div className="detail-page__actions">{p.actions}</div>}
    </header>
  );
}
```
- [ ] In `src/components/shell/DetailPage.tsx`: change the import line `import { StatusPill, type StatusKind } from "./Status";` to `import type { StatusKind } from "./Status";` and add `import { DetailHeader } from "./DetailHeader";`. Replace the whole `<header className="detail-page__head">…</header>` block (the 10 lines from `<header` to `</header>`) with:
  ```tsx
      <DetailHeader avatar={p.avatar} title={p.title} status={p.status} meta={p.meta} actions={p.actions} />
  ```
- [ ] `npm test` → 43 files, all pass (269 + 2). `npm run build`, `npm run lint`.
- [ ] Commit: `refactor(hub): extract DetailHeader from DetailPage`

### Task 10.2 — rows: channel join, `chatRows`, `chatFacets`, the unread dot, strings

**Interfaces.** Consumes nothing new. Produces:
- `buildChatList(agents, attention, channels: ChannelSummary[], query?)`, `ChatListItem` with `lastActivityMs?`, `updatedAt?`, `preview?`, `channelId?`, `turns?`.
- `chatRows(items, runtimeMap, nowMs, labels: { noChannel }, avatar: (item) => ReactNode): SourceRowData[]`, `chatFacets(items, labels: { needsYou, unread }): SourceFacet[]`, `FACET_NEEDS_YOU = "needsYou"`, `FACET_UNREAD = "unread"`.
- `SourceRowData.unread?: boolean`; `SourceListProps.createLabel?`, `SourceListProps.unreadLabel?`.
- i18n keys `chats.selectHint`, `chats.noMatch`, `chats.openAgent`, `chats.popout`, `chats.unread`, `chats.facet.needsYou`, `chats.facet.unread`, `chats.noChannel`.

- [ ] Replace the `buildChatList` describe in `src/components/chats/chatList.test.ts` with the following (keep the `sortConversations` and `groupByAgent` describes; add the imports and helper shown):

```ts
import type { ChannelSummary } from "../../work/types";
import type { AgentRuntimeStatus } from "../../types";
import { chatFacets, chatRows, FACET_NEEDS_YOU, FACET_UNREAD } from "./chatList";

function channel(id: string, over: Partial<ChannelSummary> = {}): ChannelSummary {
  return {
    id, title: id, state: "idle", goal: "", created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-06T10:00:00Z",
    participants: [], agents: [id], turns: 3, preview: `last from ${id}`, ...over,
  };
}

describe("buildChatList", () => {
  it("maps attention onto rows and sorts", () => {
    const out = buildChatList([agent("a", "Alpha"), agent("b", "Bravo")], { b: { unread: true, hitl: false } }, []);
    expect(out.map((i) => i.name)).toEqual(["b", "a"]);
    expect(out[0].unread).toBe(true);
  });
  it("filters by query against name and display name", () => {
    const out = buildChatList([agent("scout", "Scout"), agent("mapper", "Mapper")], {}, [], "map");
    expect(out.map((i) => i.name)).toEqual(["mapper"]);
  });
  it("joins the primary channel by id and sorts by its activity", () => {
    const out = buildChatList(
      [agent("a"), agent("b"), agent("c")],
      {},
      [channel("a", { updated_at: "2026-09-06T09:00:00Z" }), channel("b", { updated_at: "2026-09-06T11:00:00Z", turns: 7 }), channel("fleet-x")],
    );
    expect(out.map((i) => i.name)).toEqual(["b", "a", "c"]);
    expect(out[0]).toMatchObject({ channelId: "b", turns: 7, preview: "last from b", updatedAt: "2026-09-06T11:00:00Z" });
    expect(out[0].lastActivityMs).toBe(Date.parse("2026-09-06T11:00:00Z"));
    expect(out[2].channelId).toBeUndefined();
    expect(out[2].lastActivityMs).toBeUndefined();
  });
});

describe("chatRows", () => {
  const rt = new Map<string, AgentRuntimeStatus>([["a", { name: "a", state: { state: "running" } } as AgentRuntimeStatus]]);
  const labels = { noChannel: "no channel" };
  const now = Date.parse("2026-09-06T12:00:00Z");
  it("builds subtitle, status, badges and facets", () => {
    const [a, b] = chatRows(
      [item({ name: "a", hitl: true, preview: "hi", updatedAt: "2026-09-06T11:00:00Z", lastActivityMs: 1 }), item({ name: "b", unread: true })],
      rt, now, labels, (i) => i.name.toUpperCase(),
    );
    expect(a.subtitle?.startsWith("hi · ")).toBe(true);
    expect(a.status).toBe("running");
    expect(a.needsYou).toBe(1);
    expect(a.unread).toBe(false);
    expect(a.facets).toEqual([FACET_NEEDS_YOU]);
    expect(a.avatar).toBe("A");
    expect(b.subtitle).toBe("no channel");
    expect(b.status).toBe("stopped");
    expect(b.needsYou).toBe(0);
    expect(b.unread).toBe(true);
    expect(b.facets).toEqual([FACET_UNREAD]);
  });
});

describe("chatFacets", () => {
  it("counts needs-you and unread, omitting empty chips", () => {
    const labels = { needsYou: "Needs you", unread: "Unread" };
    expect(chatFacets([item({ name: "a", hitl: true }), item({ name: "b", unread: true }), item({ name: "c", unread: true })], labels))
      .toEqual([{ id: FACET_NEEDS_YOU, label: "Needs you", count: 1 }, { id: FACET_UNREAD, label: "Unread", count: 2 }]);
    expect(chatFacets([item({ name: "a" })], labels)).toEqual([]);
  });
});
```
- [ ] `npm test -- src/components/chats/chatList.test.ts` → fails (signature / missing exports).
- [ ] Edit `src/components/chats/chatList.ts`: add imports and replace `ChatListItem` and `buildChatList`, append the row builders:

```ts
import type { ReactNode } from "react";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ConversationAttention } from "../../conversation/reducer";
import type { ChannelSummary } from "../../work/types";
import type { SourceFacet, SourceRowData } from "../shell/sourceListModel";
import { statusOf } from "../shell/Status";
import { relativeTime } from "../../work/format";

export interface ChatListItem {
  name: string;
  displayName: string;
  agent: AgentEntry;
  /** Unread deltas arrived while this chat was not focused. */
  unread: boolean;
  /** A HITL approval is pending on this chat. */
  hitl: boolean;
  /** Epoch ms of the primary channel's updated_at; undefined without a channel. */
  lastActivityMs?: number;
  /** The primary channel's updated_at (ISO), for relativeTime. */
  updatedAt?: string;
  /** The primary channel's preview line; undefined without a channel or when empty. */
  preview?: string;
  /** The primary channel's id and turn count, for the header meta. */
  channelId?: string;
  turns?: number;
}
```
  `sortConversations` and `groupByAgent` stay as they are. `buildChatList` becomes:
```ts
/**
 * Build the ordered, filtered chat list from the agent roster, the live
 * conversation attention map, and the channel summaries. An agent's primary
 * channel has the agent's name as its id (the rule ChatChannelRail uses);
 * fleet and other channels are ignored here. `query` filters by name or
 * display name.
 */
export function buildChatList(
  agents: AgentEntry[],
  attention: Record<string, ConversationAttention>,
  channels: ChannelSummary[],
  query?: string,
): ChatListItem[] {
  const q = query?.trim().toLowerCase() ?? "";
  const byId = new Map(channels.map((c) => [c.id, c]));
  const items = agents
    .filter((a) => !q || a.name.toLowerCase().includes(q) || a.display_name.toLowerCase().includes(q))
    .map((a): ChatListItem => {
      const attn = attention[a.name];
      const ch = byId.get(a.name);
      const updated = ch ? Date.parse(ch.updated_at) : Number.NaN;
      return {
        name: a.name,
        displayName: a.display_name,
        agent: a,
        unread: attn?.unread ?? false,
        hitl: attn?.hitl ?? false,
        lastActivityMs: Number.isFinite(updated) ? updated : undefined,
        updatedAt: ch?.updated_at,
        preview: ch?.preview || undefined,
        channelId: ch?.id,
        turns: ch?.turns,
      };
    });
  return sortConversations(items);
}

export const FACET_NEEDS_YOU = "needsYou";
export const FACET_UNREAD = "unread";

const SUBTITLE_SEP = " · ";

/** SourceList rows for the Chats page (spec 3(a) §4). `avatar` is injected so
 *  this module stays free of JSX. */
export function chatRows(
  items: ChatListItem[],
  runtimeMap: Map<string, AgentRuntimeStatus>,
  nowMs: number,
  labels: { noChannel: string },
  avatar: (item: ChatListItem) => ReactNode,
): SourceRowData[] {
  return items.map((i) => ({
    id: i.name,
    name: i.displayName,
    subtitle: i.preview && i.updatedAt ? `${i.preview}${SUBTITLE_SEP}${relativeTime(i.updatedAt, nowMs)}` : labels.noChannel,
    status: statusOf(runtimeMap.get(i.name)?.state),
    needsYou: i.hitl ? 1 : 0,
    unread: i.unread,
    avatar: avatar(i),
    facets: [...(i.hitl ? [FACET_NEEDS_YOU] : []), ...(i.unread ? [FACET_UNREAD] : [])],
  }));
}

/** Chips: Needs you / Unread, each only while it has members. */
export function chatFacets(items: ChatListItem[], labels: { needsYou: string; unread: string }): SourceFacet[] {
  const needsYou = items.filter((i) => i.hitl).length;
  const unread = items.filter((i) => i.unread).length;
  return [
    ...(needsYou > 0 ? [{ id: FACET_NEEDS_YOU, label: labels.needsYou, count: needsYou }] : []),
    ...(unread > 0 ? [{ id: FACET_UNREAD, label: labels.unread, count: unread }] : []),
  ];
}
```
  (`chatRows` yields `status: "stopped"` for an agent without runtime state — that is what `statusOf(undefined)` returns; the Agents page shows the same.)
- [ ] `src/components/shell/sourceListModel.ts`: add to `SourceRowData` after `needsYou`:
  ```ts
  /** Brand-coloured dot before the name (Chats: activity while not focused). */
  unread?: boolean;
  ```
- [ ] `src/components/shell/SourceList.tsx`: change `createLabel: string;` to `/** Title of the "+" button; only read when it renders. */\n  createLabel?: string;`; add after `onOpen?`:
  ```ts
  /** Accessible label for the unread dot; required when any row sets `unread`. */
  unreadLabel?: string;
  ```
  and in the row, before `<span className="source-row__avatar">`, add:
  ```tsx
                {r.unread && <span className="source-row__unread" role="img" aria-label={p.unreadLabel} />}
  ```
- [ ] `src/styles/components/source-list.css`: append
  ```css
  .source-row__unread { flex: none; width: 8px; height: 8px; border-radius: 50%; background: var(--color-brand); }
  ```
- [ ] `src/components/shell/SourceList.test.tsx`: add inside the `SourceList markup` describe:
  ```tsx
  it("renders the unread dot only for unread rows", () => {
    const withUnread = [{ ...rows[0], unread: true }, rows[1]];
    const props = { title: "Chats", count: 2, facets: [], allLabel: "All", activeFacet: null, onFacet: noop,
      filter: "", onFilter: noop, filterPlaceholder: "Filter", selectedId: null, onSelect: noop, emptyState: <p>none</p> };
    const html = renderToStaticMarkup(<SourceList {...props} rows={withUnread} unreadLabel="Unread" />);
    expect(html.match(/source-row__unread/g)).toHaveLength(1);
    expect(renderToStaticMarkup(<SourceList {...props} rows={rows} />)).not.toContain("source-row__unread");
  });
  ```
  (This test also exercises the now-optional `createLabel`: neither it nor `onCreate` is passed, and no "+" renders.)
- [ ] i18n. `en.ts` after `"chats.filter"`:
  ```ts
  "chats.selectHint": "Select an agent to start chatting.",
  "chats.noMatch": "No chats match.",
  "chats.openAgent": "Open agent",
  "chats.popout": "Pop out",
  "chats.unread": "Unread",
  "chats.facet.needsYou": "Needs you",
  "chats.facet.unread": "Unread",
  "chats.noChannel": "No channel yet.",
  ```
  `zh-TW.ts` after `"chats.filter"`:
  ```ts
  "chats.selectHint": "選一個 agent 開始對話。",
  "chats.noMatch": "沒有符合的對話。",
  "chats.openAgent": "開啟 agent",
  "chats.popout": "彈出視窗",
  "chats.unread": "未讀",
  "chats.facet.needsYou": "需要你",
  "chats.facet.unread": "未讀",
  "chats.noChannel": "尚無頻道。",
  ```
- [ ] Keep the build green until 10.3 replaces the page: in the old `src/components/chats/ChatsPage.tsx` change `buildChatList(agents, attention, localQuery || query)` to `buildChatList(agents, attention, [], localQuery || query)` (no channels yet; rows keep today's name-only look for this one commit).
- [ ] `npm test` → 43 files, all pass (272 + 5 new). `npm run build`, `npm run lint`. Commit: `feat(hub): chat rows join the primary channel; SourceList unread dot; strings`

### Task 10.3 — `ChatPane`, the `ChatsPage` rewrite, `DashboardApp` wiring

**Interfaces.** Consumes 10.1 `DetailHeader`, 10.2 builders / props / keys. Produces `ChatsPage` props `{ agents, runtimeMap, channels, initialAgent?, onInitialHandled?, onSelect?, onOpenAgent }`, `popOutChat(name)`, and `DashboardApp.selectedChat`.

- [ ] Create `src/components/chats/ChatPane.tsx`:

```tsx
import { invoke } from "@tauri-apps/api/core";
import type { AgentRuntimeStatus } from "../../types";
import { useT } from "../../i18n";
import { avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import { ChatTab } from "../ChatTab";
import { TaskPill } from "../chat/TaskPill";
import { DetailHeader } from "../shell/DetailHeader";
import { statusOf } from "../shell/Status";
import { showToast } from "../detail/fleet/fleetActions";
import type { ChatListItem } from "./chatList";

const POPOUT_ERROR_TOAST_MS = 4000;
const HEADER_AVATAR_PX = 48;

/** The chat's "open in window": the existing chat window (spec 3(a) §7).
 *  Header button, ⌘↩, and row double-click all come here. */
export function popOutChat(name: string): void {
  invoke("open_chat_window", { agentName: name }).catch((err) => showToast(String(err), POPOUT_ERROR_TOAST_MS));
}

export interface ChatPaneProps {
  item: ChatListItem;
  runtime: AgentRuntimeStatus | undefined;
  onOpenAgent: (name: string) => void;
}

/** Header (the inspector's three facts, from data already on the page) plus
 *  the conversation with the live task pill, as in the chat window. */
export function ChatPane({ item, runtime, onOpenAgent }: ChatPaneProps) {
  const { t } = useT();
  const preset = avatarPreset(item.agent);
  const meta = (
    <>
      <span className="mono">{item.agent.model_id}</span>
      <span className="sep">·</span>
      {item.channelId ? (
        <>
          <span className="mono">{item.channelId}</span>
          <span className="sep">·</span>
          <span>{t("chatInspector.turns", { count: item.turns ?? 0 })}</span>
        </>
      ) : (
        <span>{t("chats.noChannel")}</span>
      )}
    </>
  );
  return (
    <section className="chat-pane">
      <DetailHeader
        avatar={<PetFace presetId={preset} family={familyOf(preset)} expression="idle" size={HEADER_AVATAR_PX} />}
        title={item.displayName}
        status={statusOf(runtime?.state)}
        meta={meta}
        actions={
          <>
            <button type="button" className="btn btn--secondary" onClick={() => popOutChat(item.name)}>
              {t("chats.popout")}
            </button>
            <button type="button" className="btn btn--secondary" onClick={() => onOpenAgent(item.name)}>
              {t("chats.openAgent")}
            </button>
          </>
        }
      />
      <div className="chat-pane__body">
        <ChatTab agentName={item.name} displayName={item.displayName} aboveCompose={<TaskPill agentName={item.name} />} />
      </div>
    </section>
  );
}
```
- [ ] Rewrite `src/components/chats/ChatsPage.tsx`:

```tsx
import { useEffect, useMemo, useRef, useState } from "react";
import type { AgentEntry, AgentRuntimeStatus } from "../../types";
import type { ChannelSummary } from "../../work/types";
import { useT } from "../../i18n";
import { avatarPreset, familyOf } from "../../utils";
import { PetFace } from "../PetFace";
import { useConversations } from "../../conversation/ConversationContext";
import { SourceList } from "../shell/SourceList";
import { ListDivider } from "../shell/ListDivider";
import {
  LIST_WIDTH_DEFAULT, LIST_WIDTH_MAX, LIST_WIDTH_MIN, useResizableColumn,
} from "../shell/useResizableColumn";
import { listModeFor } from "../shell/breakpoints";
import { useWindowWidth } from "../shell/useWindowWidth";
import { readKey, writeKey } from "../shell/persist";
import { buildChatList, chatFacets, chatRows } from "./chatList";
import { ChatPane, popOutChat } from "./ChatPane";

export const LAST_SELECTED_CHAT_KEY = "mur.chats.lastSelected";
export const CHATS_LIST_WIDTH_KEY = "mur.chats.listWidth";
const ROW_AVATAR_PX = 28;

interface Props {
  agents: AgentEntry[];
  runtimeMap: Map<string, AgentRuntimeStatus>;
  /** The dashboard's channel summaries (useChannels); the primary channel per agent feeds rows and header. */
  channels: ChannelSummary[];
  /** Agent to open when entering from elsewhere (Agents → Chat). */
  initialAgent?: string | null;
  /** Called once the request is applied, so the same agent can be requested again. */
  onInitialHandled?: () => void;
  /** Reports the selection up for ⌘↩ (spec 3(a) §7). */
  onSelect?: (name: string | null) => void;
  onOpenAgent: (name: string) => void;
}

/** Chats (spec 3(a)): SourceList of agents | divider | ChatPane. */
export function ChatsPage({ agents, runtimeMap, channels, initialAgent, onInitialHandled, onSelect, onOpenAgent }: Props) {
  const { t } = useT();
  const { attention } = useConversations();
  const [selected, setSelected] = useState<string | null>(null);
  const [filter, setFilter] = useState("");
  const [facet, setFacet] = useState<string | null>(null);
  const [listShown, setListShown] = useState(false);
  // One-shot restore, the AgentsPage pattern: never re-fills after Esc.
  const restored = useRef(false);
  const column = useResizableColumn(CHATS_LIST_WIDTH_KEY, LIST_WIDTH_DEFAULT, LIST_WIDTH_MIN, LIST_WIDTH_MAX);
  const listMode = listModeFor(useWindowWidth());

  useEffect(() => {
    if (restored.current || agents.length === 0) return;
    restored.current = true;
    if (selected !== null) return;
    const last = readKey(LAST_SELECTED_CHAT_KEY);
    if (last && agents.some((a) => a.name === last)) setSelected(last);
  }, [agents, selected]);
  useEffect(() => {
    // Only after the restore ran: the mount render's null must not erase
    // the stored selection before the restore effect has read it.
    if (restored.current) writeKey(LAST_SELECTED_CHAT_KEY, selected);
  }, [selected]);

  // An explicit request (Agents → Chat) outranks the stored selection.
  useEffect(() => {
    if (!initialAgent) return;
    restored.current = true;
    setSelected(initialAgent);
    onInitialHandled?.();
  }, [initialAgent, onInitialHandled]);

  useEffect(() => {
    onSelect?.(selected);
    return () => onSelect?.(null);
  }, [selected, onSelect]);

  const items = useMemo(() => buildChatList(agents, attention, channels), [agents, attention, channels]);
  const rows = chatRows(items, runtimeMap, Date.now(), { noChannel: t("chats.noChannel") }, (item) => {
    const preset = avatarPreset(item.agent);
    return <PetFace presetId={preset} family={familyOf(preset)} expression="idle" size={ROW_AVATAR_PX} animate={false} />;
  });
  const facets = chatFacets(items, { needsYou: t("chats.facet.needsYou"), unread: t("chats.facet.unread") });
  const active = items.find((i) => i.name === selected);

  if (agents.length === 0) {
    return <div className="chats-empty"><p>{t("chats.empty")}</p></div>;
  }

  const cls = `master-detail master-detail--${listMode}${listShown ? " master-detail--list-shown" : ""}`;
  return (
    <div className={cls} style={{ ["--md-list-width" as string]: `${column.width}px` }}>
      <SourceList
        title={t("nav.chats")}
        count={agents.length}
        rows={rows}
        facets={facets}
        allLabel={t("dashboard.all")}
        activeFacet={facet}
        onFacet={setFacet}
        filter={filter}
        onFilter={setFilter}
        filterPlaceholder={t("chats.filter")}
        selectedId={selected}
        onSelect={(id) => {
          setSelected(id);
          setListShown(false);
        }}
        onOpen={popOutChat}
        unreadLabel={t("chats.unread")}
        emptyState={<p className="source-list__empty">{t("chats.noMatch")}</p>}
      />
      <ListDivider column={column} label={t("shell.resizeList")} />
      <div className="master-detail__detail">
        {listMode === "overlay" && (
          <button type="button" className="btn btn--secondary master-detail__show-list" onClick={() => setListShown((v) => !v)}>
            {t("shell.showList")}
          </button>
        )}
        {active ? (
          <ChatPane key={active.name} item={active} runtime={runtimeMap.get(active.name)} onOpenAgent={onOpenAgent} />
        ) : (
          <div className="chats-empty"><p>{t("chats.selectHint")}</p></div>
        )}
      </div>
    </div>
  );
}
```
- [ ] Create `src/styles/components/chats.css` and add `@import "./components/chats.css";` after the `detail-window.css` line in `src/styles/index.css`:

```css
/* Chats page (Phase 3(a) §5): header + conversation filling the detail column. */
.chat-pane { display: flex; flex-direction: column; height: 100%; min-height: 0; background: var(--surface-detail); }
.chat-pane__body { flex: 1; min-height: 0; display: flex; flex-direction: column; padding: 0 var(--space-8) var(--space-6); }
.chat-pane__body > .chat { flex: 1; }
.chats-empty { display: grid; place-items: center; height: 100%; color: var(--text-tertiary); font-size: var(--text-sm); }
.chats-empty p { margin: 0; }
```
- [ ] `src/components/DashboardApp.tsx`:
  - Add `import { popOutChat } from "./chats/ChatPane";`.
  - Delete the `onChatActive` callback (lines 80–83: the comment `// Stable callbacks …` and the `useCallback` that calls `setChatAgent`). Keep `chatAgent` / `setChatAgent` for now (10.4 removes them).
  - After `const openChatWith = useCallback(…)` add:
    ```tsx
    const clearChatInitial = useCallback(() => setChatInitial(null), []);
    // The Chats page reports its selection up for ⌘↩ (spec 3(a) §7).
    const [selectedChat, setSelectedChat] = useState<string | null>(null);
    const onChatSelect = useCallback((name: string | null) => setSelectedChat(name), []);
    // "Open agent" from a chat header: the Agents page with that agent selected.
    const openAgentFromChat = useCallback((name: string) => {
      setSelected(name);
      setPage("agents");
    }, [setSelected]);
    ```
  - Replace `<ChatsPage agents={agents} initialAgent={chatInitial} onActiveChange={onChatActive} />` with:
    ```tsx
            <ChatsPage
              agents={agents}
              runtimeMap={runtimeMap}
              channels={channels}
              initialAgent={chatInitial}
              onInitialHandled={clearChatInitial}
              onSelect={onChatSelect}
              onOpenAgent={openAgentFromChat}
            />
    ```
  - In the keydown effect's `isOpenInWindowShortcut` branch add, after the `fleets` case:
    ```tsx
        } else if (page === "chats" && selectedChat) {
          e.preventDefault();
          popOutChat(selectedChat);
        }
    ```
    and add `selectedChat` to that effect's dependency array.
- [ ] `npm test`, `npm run build`, `npm run lint` (0 errors).
- [ ] Browser acceptance (stub: `list_agents` two agents, `list_runtime_statuses` with one running, `channel_list` with a primary channel for one agent — `id` = its name, `preview`, `updated_at`, `turns` — plus a `fleet-x` channel, `channel_load → []`, `agent_chat_send` not needed, `open_chat_window → null`, `panel_schedule_status → { schedules: [] }`): the list shows preview · relative time for the agent with a channel and "No channel yet." for the other; the running agent's dot; no chips until a stored `hitl-approval-needed` / `chat-delta` listener is fired, then "Needs you" / "Unread" chips appear with counts and the unread row shows the dot; selecting shows the header with model · channel · turns (or "No channel yet."); `localStorage['mur.chats.lastSelected']` is written and restores on remount (switch to Agents and back); from Agents, the detail's **Chat** lands here with that agent selected, and doing it twice for the same agent still works; **Pop out**, ⌘↩ (not while the compose box is focused), and a row double-click each call `open_chat_window`; **Open agent** lands on Agents with the agent selected; the task pill mounts above the compose box (`TaskPill` listens to `runtime-status-changed` — its listener id appears in the stub's event map); Esc in the list clears the selection to the hint.
- [ ] Commit: `feat(hub): Chats page on the master–detail shell — SourceList rows, headed ChatPane with TaskPill`

### Task 10.4 — inspector retirement

**Interfaces.** Consumes nothing new. Produces no API; removes `Shell.inspector`, `isInspectorToggle`, `Inspector`, `ChatInspector`.

- [ ] Delete `src/components/inspector/ChatInspector.tsx` and `src/components/shell/Inspector.tsx`. `grep -rn 'ChatInspector\|shell/Inspector\|hasInspector\|InspectorSelection' src` → only `DashboardApp.tsx` (fixed below).
- [ ] `src/components/shell/Shell.tsx`: delete the `isInspectorToggle` function and its comment (lines 10–14); delete `inspector?: ReactNode;` from `ShellProps`; remove `inspector` from the destructured props; delete `const [inspectorVisible, setInspectorVisible] = useState(true);`; delete the `if (isInspectorToggle(e)) { … return; }` block; delete `const showInspector = …;` and the `showInspector ? "shell--with-inspector" : "",` line; delete `{showInspector && <div className="shell__inspector">{inspector}</div>}`. `useState` stays imported (the sidebar pref uses it).
- [ ] `src/components/shell/shell.test.ts`: change the import to `import { isSidebarToggle } from "./Shell";`, delete the `key()` helper (lines 4–13; only the inspector tests used it, and `noUnusedLocals` would flag it) and the whole `describe("isInspectorToggle", …)` block (lines 15–43). The `isSidebarToggle` describe builds its own `base` object and stays untouched.
- [ ] `src/components/DashboardApp.tsx`: delete the `Inspector` import line; delete the `chatAgent` state and its three-line comment (lines 76–79); delete `setChatAgent(null);` from the Esc handler; delete the whole `inspectorSelection` / `inspectorNode` block (from `// Build the contextual inspector` to the `: undefined;` line); delete `inspector={inspectorNode}` from `<Shell …>`. `grep -n 'chatAgent\|inspector' src/components/DashboardApp.tsx` → none.
- [ ] CSS: in `src/styles/components/shell.css` delete `.shell--with-inspector { … }`, the `.shell--sidebar-collapsed.shell--with-inspector { … }` rule, and `.shell__inspector { … }`. In `src/styles/tokens/primitives.css` remove ` --shell-inspector-width:320px;` from line 46. In `src/styles/components/work.css` delete the block from `/* ── Chats view — agent list + inline chat` through the end of `.chats-view__empty { … }`. In `src/styles/components/dashboard.css` delete `.conv-badge { … }`, `.conv-badge--unread { … }`, `.conv-badge--hitl { … }`. In `src/styles/components/detail-panel.css` delete `.detail-panel--inspector { … }`, `.detail-panel__close { … }`, `.detail-panel__close:hover { … }`. Then `grep -rn 'chats-view\|chats-item\|conv-badge\|detail-panel--inspector\|detail-panel__close\|shell__inspector\|shell-inspector-width' src` → none.
- [ ] i18n: delete `chatInspector.subtitle`, `chatInspector.model`, `chatInspector.channel`, `chatInspector.noChannel` from both tables (`chats.noChannel` replaced the last in 10.2). Keep `chatInspector.turns` (the header uses it). `grep -rn 'chatInspector\.' src --include='*.tsx'` → only `ChatPane.tsx` (`turns`).
- [ ] `npm test`, `npm run build`, `npm run lint`. Browser: no page shows a third column; ⌘⌥I does nothing; Agents / Fleets / Library / Chats all still render.
- [ ] Commit: `refactor(hub): retire the inspector — Shell prop, ⌘⌥I, CSS track, ChatInspector`

**Manual acceptance PR 10:** the Task 10.3 list plus: window at 960px shows the overlay list on Chats like Agents; the divider resizes and persists; light and dark themes; the chat still streams a reply from a real agent (dev build against a running Hub, or the first real-app check after install).

## Spec coverage

| Spec § | Task |
|---|---|
| 3 layout, selection, empty states, prop removals | 10.3 |
| 4 rows, `SourceRowData.unread`, chips | 10.2 |
| 5 `DetailHeader`, `ChatPane`, CSS | 10.1, 10.3 |
| 6 inspector retirement | 10.4 (i18n rename in 10.2) |
| 7 keyboard / gestures | 10.3 (⌘↩ branch, `onOpen`) |
| 8 errors / empty | 10.3 (`chats-empty`, `noChannel`, toast in `popOutChat`) |
| 9 tests | 10.1, 10.2, 10.4; browser lists in 10.3 / 10.4 |
