import { describe, it, expect } from "vitest";
import type { AgentEntry } from "../../types";
import {
  sortConversations,
  groupByAgent,
  buildChatList,
  type ChatListItem,
} from "./chatList";
import type { ChannelSummary } from "../../work/types";
import type { AgentRuntimeStatus } from "../../types";
import { chatFacets, chatRows, FACET_NEEDS_YOU, FACET_UNREAD } from "./chatList";

function channel(id: string, over: Partial<ChannelSummary> = {}): ChannelSummary {
  return {
    id, title: id, state: "idle", goal: "", created_at: "2026-09-01T00:00:00Z", updated_at: "2026-09-06T10:00:00Z",
    participants: [], agents: [id], turns: 3, preview: `last from ${id}`, ...over,
  };
}

function agent(name: string, display = name): AgentEntry {
  return {
    name,
    display_name: display,
    category: "custom",
    status: "idle",
    model_id: "m",
    style_preset: "chiikawa",
    role: null,
  };
}

function item(over: Partial<ChatListItem> & { name: string }): ChatListItem {
  return {
    displayName: over.name,
    agent: agent(over.name),
    unread: false,
    hitl: false,
    ...over,
  };
}

describe("sortConversations", () => {
  it("pins HITL first, then unread, then latest activity desc", () => {
    const out = sortConversations([
      item({ name: "quiet", lastActivityMs: 100 }),
      item({ name: "recent", lastActivityMs: 900 }),
      item({ name: "unread", unread: true, lastActivityMs: 50 }),
      item({ name: "hitl", hitl: true, lastActivityMs: 10 }),
    ]);
    expect(out.map((i) => i.name)).toEqual([
      "hitl",
      "unread",
      "recent",
      "quiet",
    ]);
  });

  it("falls back to display name when activity ties", () => {
    const out = sortConversations([
      item({ name: "b", displayName: "Bravo" }),
      item({ name: "a", displayName: "Alpha" }),
    ]);
    expect(out.map((i) => i.name)).toEqual(["a", "b"]);
  });

  it("does not mutate the input array", () => {
    const input = [item({ name: "z" }), item({ name: "a" })];
    const copy = [...input];
    sortConversations(input);
    expect(input).toEqual(copy);
  });
});

describe("groupByAgent", () => {
  it("buckets rows by agent name", () => {
    const grouped = groupByAgent([
      item({ name: "a" }),
      item({ name: "b" }),
      item({ name: "a" }),
    ]);
    expect(Object.keys(grouped).sort()).toEqual(["a", "b"]);
    expect(grouped.a).toHaveLength(2);
    expect(grouped.b).toHaveLength(1);
  });

  it("returns an empty object for no rows", () => {
    expect(groupByAgent([])).toEqual({});
  });
});

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
    expect(b.status).toBe("idle");
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
