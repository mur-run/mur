import { describe, it, expect } from "vitest";
import { type ModelSwitchView, sanitizeChain, isChainValid } from "./modelSwitch";

describe("modelSwitch helpers", () => {
  it("sanitizeChain drops blanks and de-dupes preserving order", () => {
    expect(sanitizeChain(["a", "", "b", "a", "  "])).toEqual(["a", "b"]);
  });
  it("isChainValid requires every ref to be a known model id", () => {
    const known = new Set(["a", "b"]);
    expect(isChainValid(["a", "b"], known)).toBe(true);
    expect(isChainValid(["a", "x"], known)).toBe(false);
  });
});

// Type-only smoke check: keeps ModelSwitchView's shape exercised by this test file.
const _typeCheck: ModelSwitchView = {
  default: null,
  fallback_chain: [],
  retry: { max_retries: 0, backoff_base_ms: 0, cooldown_secs: 0 },
  routing: { enabled: false, cheap: null, frontier: null, threshold_input_tokens: null },
};
void _typeCheck;
