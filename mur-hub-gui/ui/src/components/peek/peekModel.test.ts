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
