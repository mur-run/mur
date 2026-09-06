import { describe, expect, it } from "vitest";
import { resolveInstallTarget } from "./useInstallTarget";

describe("resolveInstallTarget", () => {
  it("keeps a stored agent that still exists, else the first agent, else empty", () => {
    const agents = [{ name: "aura" }, { name: "scout" }];
    expect(resolveInstallTarget("scout", agents)).toBe("scout");
    expect(resolveInstallTarget("ghost", agents)).toBe("aura");
    expect(resolveInstallTarget(null, [])).toBe("");
  });
});
