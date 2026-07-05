import { describe, expect, it } from "vitest";
import { inboxBadge, mergeInbox, visibleInboxItems, type InboxItem } from "./inbox";

function item(kind: InboxItem["kind"], id: string, ts: number, title = id): InboxItem {
  return { kind, id, ts, title, subtitle: "", payload: null };
}

describe("mergeInbox", () => {
  it("sorts descending by ts across multiple sources", () => {
    const a: InboxItem[] = [item("hitl", "1", 100), item("hitl", "2", 300)];
    const b: InboxItem[] = [item("install", "3", 200)];
    const merged = mergeInbox([a, b]);
    expect(merged.map((i) => i.id)).toEqual(["2", "3", "1"]);
  });

  it("is stable for equal ts (preserves relative input order)", () => {
    const a: InboxItem[] = [item("hitl", "1", 100), item("hitl", "2", 100)];
    const b: InboxItem[] = [item("install", "3", 100)];
    const merged = mergeInbox([a, b]);
    expect(merged.map((i) => i.id)).toEqual(["1", "2", "3"]);
  });

  it("dedups by kind+id, keeping the newest", () => {
    const a: InboxItem[] = [item("hitl", "1", 100, "old")];
    const b: InboxItem[] = [item("hitl", "1", 500, "new")];
    const merged = mergeInbox([a, b]);
    expect(merged).toHaveLength(1);
    expect(merged[0].title).toBe("new");
    expect(merged[0].ts).toBe(500);
  });

  it("does not dedup across different kinds with the same id", () => {
    const a: InboxItem[] = [item("hitl", "1", 100)];
    const b: InboxItem[] = [item("install", "1", 200)];
    const merged = mergeInbox([a, b]);
    expect(merged).toHaveLength(2);
  });

  it("returns [] for empty sources", () => {
    expect(mergeInbox([])).toEqual([]);
    expect(mergeInbox([[], []])).toEqual([]);
  });
});

describe("inboxBadge", () => {
  it("counts all items", () => {
    const items: InboxItem[] = [item("hitl", "1", 1), item("companion", "2", 2)];
    expect(inboxBadge(items)).toBe(2);
  });

  it("is 0 for empty list", () => {
    expect(inboxBadge([])).toBe(0);
  });
});

describe("visibleInboxItems", () => {
  it("removes dismissed items by kind:id", () => {
    const items: InboxItem[] = [
      item("upgrade_blocked", "1", 1),
      item("upgrade_blocked", "2", 2),
      item("hitl", "3", 3),
    ];
    const dismissed = new Set(["upgrade_blocked:1"]);
    const visible = visibleInboxItems(items, dismissed);
    expect(visible.map((i) => i.id)).toEqual(["2", "3"]);
  });

  it("keeps everything when nothing is dismissed", () => {
    const items: InboxItem[] = [item("hitl", "1", 1), item("companion", "2", 2)];
    expect(visibleInboxItems(items, new Set())).toHaveLength(2);
  });

  it("badge count matches the filtered length (no drift)", () => {
    const items: InboxItem[] = [
      item("upgrade_blocked", "1", 1),
      item("upgrade_blocked", "2", 2),
      item("hitl", "3", 3),
    ];
    const dismissed = new Set(["upgrade_blocked:1", "upgrade_blocked:2"]);
    const visible = visibleInboxItems(items, dismissed);
    expect(inboxBadge(visible)).toBe(visible.length);
    expect(inboxBadge(visible)).toBe(1);
  });
});
