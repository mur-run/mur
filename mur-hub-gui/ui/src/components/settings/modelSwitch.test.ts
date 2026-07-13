import { describe, it, expect } from "vitest";
import { type ModelSwitchView, sanitizeChain, isChainValid, normalizeMs } from "./modelSwitch";

describe("modelSwitch helpers", () => {
  it("sanitizeChain drops blanks and de-dupes preserving order", () => {
    expect(sanitizeChain(["a", "", "b", "a", "  "])).toEqual(["a", "b"]);
  });
  it("isChainValid requires every ref to be a known model id", () => {
    const known = new Set(["a", "b"]);
    expect(isChainValid(["a", "b"], known)).toBe(true);
    expect(isChainValid(["a", "x"], known)).toBe(false);
  });
  it("normalizeMs fills fields the Rust config omits when empty/unset", () => {
    // A default config serializes without default/fallback_chain/routing options
    // (skip_serializing_if). normalizeMs must fill them so the UI never reads
    // `undefined.length`.
    const raw = { retry: { max_retries: 1, backoff_base_ms: 500, cooldown_secs: 60 }, routing: { enabled: false } } as unknown as ModelSwitchView;
    const ms = normalizeMs(raw);
    expect(ms.fallback_chain).toEqual([]);
    expect(ms.default).toBeNull();
    expect(ms.routing.cheap).toBeNull();
    expect(ms.routing.frontier).toBeNull();
    expect(ms.routing.threshold_input_tokens).toBeNull();
    expect(ms.routing.enabled).toBe(false);
    expect(ms.retry.cooldown_secs).toBe(60); // passthrough preserved
    // smart defaults on and auto-picks when the Rust payload omits it entirely.
    expect(ms.smart.enabled).toBe(true);
    expect(ms.smart.cheap).toBeNull();
    expect(ms.smart.max_escalations).toBe(1);
  });
  it("normalizeMs fills smart.cheap when smart is present but missing cheap", () => {
    const raw = {
      retry: { max_retries: 1, backoff_base_ms: 500, cooldown_secs: 60 },
      routing: { enabled: false },
      smart: { enabled: false, max_escalations: 3 },
    } as unknown as ModelSwitchView;
    const ms = normalizeMs(raw);
    expect(ms.smart.enabled).toBe(false); // passthrough preserved
    expect(ms.smart.cheap).toBeNull();
    expect(ms.smart.max_escalations).toBe(3); // passthrough preserved
  });
});

// Type-only smoke check: keeps ModelSwitchView's shape exercised by this test file.
const _typeCheck: ModelSwitchView = {
  default: null,
  fallback_chain: [],
  retry: { max_retries: 0, backoff_base_ms: 0, cooldown_secs: 0 },
  routing: { enabled: false, cheap: null, frontier: null, threshold_input_tokens: null },
  smart: { enabled: true, cheap: null, max_escalations: 1 },
};
void _typeCheck;
