import { describe, expect, it } from "vitest";
import { buildSlotGroups, decodeSel, encodeSel } from "./modelSlots";

const reg = [
  { ref_name: "anthropic_opus", provider: "anthropic", model: "claude-opus-4-6", tier: null, input_cost: null, output_cost: null, context_window: null, capabilities: [] },
];
const local = [
  { key: "ollama", name: "Ollama", base_url: "http://localhost:11434", models: [{ model: "qwen3.5:4b", alias: "ollama_qwen35_4b", input_cost: null, output_cost: null, context_window: null }] },
];

describe("buildSlotGroups", () => {
  it("groups registry by provider then local providers", () => {
    const g = buildSlotGroups(reg as never, local as never);
    expect(g[0].label).toBe("Anthropic");
    expect(g[0].options[0].payload).toEqual({ kind: "registry", ref_name: "anthropic_opus" });
    expect(g[1].label).toBe("Ollama (local)");
    expect(g[1].options[0].payload).toMatchObject({ kind: "local", model: "qwen3.5:4b" });
  });
  it("encode/decode round-trips", () => {
    const s = { kind: "registry", ref_name: "x" } as const;
    expect(decodeSel(encodeSel(s))).toEqual(s);
  });
});
