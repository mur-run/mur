import { describe, expect, it } from "vitest";
import { rankPalette, type PaletteItem } from "./palette";

const noop = () => {};
const items: PaletteItem[] = [
  { id: "page:agents", kind: "page", label: "Agents", run: noop },
  { id: "agent:aura", kind: "agent", label: "AURA", run: noop },
  { id: "agent:auditor", kind: "agent", label: "Auditor", run: noop },
  { id: "action:stop", kind: "action", label: "Stop AURA", run: noop },
  { id: "fleet:builder", kind: "fleet", label: "builder", run: noop },
];

describe("rankPalette", () => {
  it("prefix beats substring, then kind order, then label", () => {
    expect(rankPalette(items, "au").map((i) => i.id)).toEqual(["agent:auditor", "agent:aura", "action:stop"]);
  });
  it("empty query lists everything in kind order, capped, keeping input order within a kind", () => {
    expect(rankPalette(items, "", 3).map((i) => i.kind)).toEqual(["page", "action", "agent"]);
    expect(rankPalette(items, "").filter((i) => i.kind === "agent").map((i) => i.id)).toEqual(["agent:aura", "agent:auditor"]);
  });
});
