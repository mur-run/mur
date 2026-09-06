import { describe, expect, it } from "vitest";
import {
  blockedToItem,
  companionToItem,
  hitlToItem,
  installToItem,
  type RawBlockedItem,
  type RawCompanionEvent,
  type RawHitl,
  type RawInstall,
} from "./useInbox";

describe("hitlToItem", () => {
  const valid: RawHitl = {
    channel_id: "chan-1",
    hitl_id: "hitl-1",
    agent: "quill",
    summary: "delete file foo.txt",
    risk: "write",
    ts: "1720000000",
  };

  it("maps a valid record", () => {
    const item = hitlToItem(valid, "en");
    expect(item).not.toBeNull();
    expect(item?.kind).toBe("hitl");
    expect(item?.agent).toBe(valid.agent);
    expect(item?.id).toBe("chan-1:hitl-1");
    expect(item?.ts).toBe(1720000000);
    expect(item?.title).toContain("quill");
  });

  it("localises the title", () => {
    expect(hitlToItem(valid, "zh-TW")?.title).toBe("quill：需要核准");
  });

  it("falls back to the risk tier when there is no summary", () => {
    const item = hitlToItem({ ...valid, summary: "" }, "en");
    expect(item?.subtitle).toBe("Risk: write");
  });

  it("returns null for a malformed record", () => {
    expect(hitlToItem({} as RawHitl, "en")).toBeNull();
    expect(hitlToItem(null as unknown as RawHitl, "en")).toBeNull();
    expect(hitlToItem({ ...valid, hitl_id: undefined } as unknown as RawHitl, "en")).toBeNull();
  });
});

describe("installToItem", () => {
  const valid: RawInstall = {
    install_type: "skill",
    id: "req-1",
    publisher: "mur-official",
    request_id: "req-1",
    requested_at: 1720000000,
    is_official: true,
  };

  it("maps a valid record", () => {
    const item = installToItem(valid, "en");
    expect(item).not.toBeNull();
    expect(item?.kind).toBe("install");
    expect(item?.id).toBe("req-1");
    expect(item?.ts).toBe(1720000000);
    expect(item?.title).toContain("skill");
  });

  it("returns null for a malformed record", () => {
    expect(installToItem({} as RawInstall, "en")).toBeNull();
    expect(installToItem(undefined as unknown as RawInstall, "en")).toBeNull();
    expect(installToItem({ ...valid, id: 5 } as unknown as RawInstall, "en")).toBeNull();
  });
});

describe("companionToItem", () => {
  const valid: RawCompanionEvent = {
    id: "evt-1",
    situation: "Task finished",
    template_id: "tmpl-1",
    locale: "en",
    generated_at: "2026-07-04T00:00:00Z",
    body: "Your export finished.",
    response: { kind: "unset" },
  };

  it("maps a valid record", () => {
    const item = companionToItem(valid, "en", "aura");
    expect(item).not.toBeNull();
    expect(item?.kind).toBe("companion");
    expect(item?.agent).toBe("aura");
    expect(item?.id).toBe("evt-1");
    expect(item?.ts).toBe(Math.floor(Date.parse(valid.generated_at) / 1000));
    expect(item?.title).toBe("Task finished");
  });

  it("returns null for a malformed record", () => {
    expect(companionToItem({} as RawCompanionEvent, "en", "aura")).toBeNull();
    expect(companionToItem(null as unknown as RawCompanionEvent, "en", "aura")).toBeNull();
    expect(
      companionToItem({ ...valid, generated_at: "not-a-date" } as RawCompanionEvent, "en", "aura"),
    ).toBeNull();
  });
});

describe("blockedToItem", () => {
  const valid: RawBlockedItem = {
    name: "my-skill",
    dir: "/tmp/skills/my-skill",
    local_version: "1.0.0",
    latest_version: "1.1.0",
  };

  it("maps a valid record", () => {
    const item = blockedToItem(valid, "en");
    expect(item).not.toBeNull();
    expect(item?.kind).toBe("upgrade_blocked");
    expect(item?.id).toBe("my-skill");
    expect(item?.title).toContain("my-skill");
  });

  it("returns null for a malformed record", () => {
    expect(blockedToItem({} as RawBlockedItem, "en")).toBeNull();
    expect(blockedToItem(undefined as unknown as RawBlockedItem, "en")).toBeNull();
    expect(blockedToItem({ ...valid, name: "" } as RawBlockedItem, "en")).toBeNull();
  });
});
