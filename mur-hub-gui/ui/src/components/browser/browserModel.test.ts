import { describe, expect, it } from "vitest";
import { pendingToItem } from "./browserModel";
import type { PendingView } from "./browserApi";

const raw: PendingView = {
  agent: "quill",
  site: "https://app.example",
  login: true,
  reason: "read the dashboard",
  requested_at: "2026-11-18T10:00:00+00:00",
};

describe("pendingToItem", () => {
  it("maps a request to a browser_request inbox item", () => {
    const it2 = pendingToItem(raw, "en");
    expect(it2?.kind).toBe("browser_request");
    expect(it2?.id).toBe("quill:https://app.example");
    expect(it2?.ts).toBe(Date.parse(raw.requested_at) / 1000);
    expect(it2?.title).toContain("login");
  });
  it("is null on malformed input", () => {
    expect(pendingToItem({ ...raw, site: 3 } as unknown as PendingView, "en")).toBeNull();
  });
});
