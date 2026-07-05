import { describe, it, expect } from "vitest";
import type { AgentEntry } from "../../types";
import {
  sortConversations,
  groupByAgent,
  buildChatList,
  type ChatListItem,
} from "./chatList";

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
    const out = buildChatList(
      [agent("a", "Alpha"), agent("b", "Bravo")],
      { b: { unread: true, hitl: false } },
      undefined,
    );
    expect(out.map((i) => i.name)).toEqual(["b", "a"]);
    expect(out[0].unread).toBe(true);
  });

  it("filters by query against name and display name", () => {
    const out = buildChatList(
      [agent("scout", "Scout"), agent("mapper", "Mapper")],
      {},
      "map",
    );
    expect(out.map((i) => i.name)).toEqual(["mapper"]);
  });
});
