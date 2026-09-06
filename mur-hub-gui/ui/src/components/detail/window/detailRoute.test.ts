import { describe, expect, it } from "vitest";
import { parseDetailRoute } from "./detailRoute";

describe("parseDetailRoute", () => {
  it("parses agent and fleet routes", () => {
    expect(parseDetailRoute("#/detail/agent/aura")).toEqual({ kind: "agent", name: "aura" });
    expect(parseDetailRoute("#/detail/fleet/night-ops")).toEqual({ kind: "fleet", name: "night-ops" });
  });
  it("decodes + and percent escapes like the chat window", () => {
    expect(parseDetailRoute("#/detail/agent/my+agent")).toEqual({ kind: "agent", name: "my agent" });
    expect(parseDetailRoute("#/detail/agent/%E7%A0%94%E7%A9%B6")).toEqual({ kind: "agent", name: "研究" });
  });
  it("rejects other hashes, unknown kinds, empty names, and bad escapes", () => {
    expect(parseDetailRoute("#/chat/aura")).toBeNull();
    expect(parseDetailRoute("#/detail/skill/x")).toBeNull();
    expect(parseDetailRoute("#/detail/agent/")).toBeNull();
    expect(parseDetailRoute("#/detail/agent")).toBeNull();
    expect(parseDetailRoute("#/detail/agent/%E0%A4%A")).toBeNull();
  });
});
