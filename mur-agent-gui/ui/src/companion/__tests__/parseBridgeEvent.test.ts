import { describe, expect, it } from "vitest";
import type { BridgeEvent } from "../types";

describe("BridgeEvent", () => {
  it("typechecks unset response", () => {
    const ev: BridgeEvent = {
      id: "x",
      situation: "morning_greeting",
      template_id: "t",
      locale: "en",
      generated_at: "2026-05-02T07:00:00Z",
      body: "hi",
      response: { kind: "unset" },
    };
    expect(ev.response.kind).toBe("unset");
  });

  it("typechecks signal response", () => {
    const ev: BridgeEvent = {
      id: "x",
      situation: "morning_greeting",
      template_id: "t",
      locale: "en",
      generated_at: "2026-05-02T07:00:00Z",
      body: "hi",
      response: { kind: "signal", value: "good" },
    };
    expect(ev.response.kind === "signal" && ev.response.value).toBe("good");
  });
});
