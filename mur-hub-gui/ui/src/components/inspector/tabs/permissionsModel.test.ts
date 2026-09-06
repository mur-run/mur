import { describe, expect, it } from "vitest";
import { enforcementTone, permCommands } from "./permissionsModel";

describe("enforcementTone", () => {
  it("only a sealed, enforcing sandbox is ok; advisory is attention; the rest muted", () => {
    expect(enforcementTone("enforcing")).toBe("ok");
    expect(enforcementTone("advisory")).toBe("attention");
    expect(enforcementTone("not_running")).toBe("muted");
    expect(enforcementTone("seal_unknown")).toBe("muted");
  });
});

describe("permCommands", () => {
  it("names the agent in every command and covers each block", () => {
    const c = permCommands("aura");
    expect(c.hosts).toBe("mur agent perm allow-host aura <host>");
    expect(c.paths).toBe("mur agent perm allow-write aura <path>");
    expect(c.spawn).toBe("mur agent perm allow-spawn aura <program>");
    expect(c.tools).toBe("mur agent perm set-tool aura <tool> allow|ask|deny");
    expect(c.mcp).toBe("mur agent mcp set-network aura <server> --allow-host <host>");
  });
});
